use std::ffi::OsStr;
use std::path::Path;

use anyhow::{anyhow, ensure, Context, Result};
use reposync_core::db::historical_enrollment::{
    enroll_verified_lineage, prepare_historical_cutover, EnrollmentOutcome, LegacyPairPins,
};
use reposync_core::db::historical_review_key::{self, ReviewSessionKey};
use reposync_core::db::historical_transport::{read_offline_recovery_inputs, RegisteredTransport};
use reposync_core::errors::DatabaseError;
use reposync_core::git::remote_url::derive_git_remote_url;
use reposync_core::legacy_svn_recovery::{
    HistoricalSvnSource, HistoricalVerifyError, VerifiedHistoricalLineage,
};

use crate::svn_recovery_args::{parse_review_token, RecoveryAction, RecoveryInputs};

impl RecoveryInputs {
    fn review_key(&self) -> Result<Option<ReviewSessionKey>> {
        let Some(path) = &self.review_key_file else {
            return Ok(None);
        };
        let key_path = path
            .canonicalize()
            .map_err(|_| anyhow!("cannot locate the private review key"))?;
        let db_dir = self
            .database
            .canonicalize()
            .map_err(|_| anyhow!("cannot locate the stopped database"))?;
        for root in [
            db_dir.parent(),
            Some(self.git_mirror.as_path()),
            self.svn_backup.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            let root = root.canonicalize().map_err(|_| {
                anyhow!("cannot identify the database or repository used for review")
            })?;
            ensure!(!key_path.starts_with(&root), "the private review key must be stored outside the installation and repository inputs");
        }
        historical_review_key::open(path)
            .map(Some)
            .map_err(|_| anyhow!("could not open the private operator-owned review key"))
    }

    fn source(&self) -> Result<HistoricalSvnSource> {
        match (&self.svn_backup, &self.historical_root_url) {
            (None, None) => Ok(HistoricalSvnSource::PlannedCutover),
            (Some(repository), Some(original_root_url)) => Ok(HistoricalSvnSource::Restored {
                repository: repository.clone(),
                original_root_url: original_root_url.clone(),
            }),
            _ => anyhow::bail!(
                "a restored backup and its historical root URL must be supplied together"
            ),
        }
    }
}

fn report(verified: &VerifiedHistoricalLineage) {
    let pair = verified.pair();
    let lineage = verified.lineage();
    let source = if verified.is_planned_cutover() {
        "Known-good original installation before replacement"
    } else {
        "Independently trusted historical SVN backup"
    };
    println!("{source}");
    println!("Pair {}", pair.repo_id.escape_debug());
    println!(
        "SVN branch {} (UUID {}, branch created at r{})",
        lineage.svn_url.escape_debug(),
        lineage.svn_uuid.escape_debug(),
        lineage.incarnation_created_rev
    );
    let git = derive_git_remote_url(&pair.git_api_url, None, &pair.git_repo);
    println!(
        "Git {} refs/heads/{}",
        git.escape_debug(),
        pair.git_branch.escape_debug()
    );
    println!(
        "Verified {} retained revision-to-commit mappings through r{} at {}.",
        pair.mappings.len(),
        pair.last_svn_rev,
        pair.last_git_sha.escape_debug()
    );
    if pair.scope_uuid.is_empty() {
        println!("The first migration assigns this legacy registration its scope and generation.");
    } else {
        println!(
            "Registered scope {} generation {}.",
            pair.scope_uuid, pair.generation
        );
    }
    for ((revision, commit), tree) in pair.mappings.iter().zip(verified.projected_tree_ids()) {
        println!("  SVN r{revision}  Git commit {commit}  projected tree {tree}");
    }
    println!(
        "Verified {} SVN branch revisions. History SHA-256 {}",
        verified.verified_branch_revisions(),
        verified.history_sha256()
    );
    println!(
        "Review digest: r{}.{}",
        verified.reviewed_svn_floor(),
        verified.review_sha256()
    );
}

fn database_failure(error: DatabaseError, fallback: &'static str) -> anyhow::Error {
    // This prefix is owned by the persistence module and contains its fixed
    // operator guidance. Raw SQLite diagnostics can contain source identifiers.
    if let DatabaseError::Other(message) = error {
        if let Some(reason) = message
            .strip_prefix("historical lineage enrollment refused: ")
            .or_else(|| message.strip_prefix("registered lineage verification refused: "))
        {
            return anyhow!("{fallback} {reason}");
        }
    }
    anyhow!(fallback)
}

pub(crate) async fn run(action: RecoveryAction) -> Result<()> {
    let (inputs, reviewed) = match action {
        RecoveryAction::CreateReviewKey { output } => {
            historical_review_key::create(&output).map_err(|_| anyhow!("Could not create a new review key in the private operator directory. Check its permissions and choose an unused file name."))?;
            println!("Private review key created. Use the same file for preview and apply.");
            return Ok(());
        }
        RecoveryAction::Preview(inputs) => (inputs, None),
        RecoveryAction::Apply {
            inputs,
            reviewed_digest,
            confirm_stopped_writers,
            attest_known_good_install,
            attest_trusted_backup,
        } => {
            let planned = inputs.svn_backup.is_none();
            ensure!(
                confirm_stopped_writers
                    && attest_known_good_install == planned
                    && attest_trusted_backup != planned,
                "confirm the maintenance window and attest to the selected source before applying"
            );
            let (floor, sha256) = parse_review_token(&reviewed_digest)
                .context("expected the complete revision-bound token from preview")?;
            (inputs, Some((floor, sha256.to_owned())))
        }
    };
    ensure!(
        inputs.database.file_name() == Some(OsStr::new("reposync.db")),
        "supply the original data directory's reposync.db"
    );
    let planned = inputs.svn_backup.is_none();
    let review_key = inputs.review_key()?;
    let (pair, transport) = read_offline_recovery_inputs(&inputs.database, &inputs.pair, planned, review_key.as_ref())
        .map_err(|error| database_failure(error, "Cannot inspect the stopped installation for the selected recovery path. Check the database, its schema and the paused pair."))?;
    let floor = reviewed.as_ref().map(|(revision, _)| *revision);
    let proof = transport
        .verify(&pair, inputs.source()?, &inputs.git_mirror, floor)
        .await
        .map_err(|e| HistoricalVerifyError::classify(&e))?;
    let Some((_, reviewed)) = reviewed else {
        report(&proof);
        println!("Preview complete. No database or repository change was made.");
        return Ok(());
    };
    ensure!(
        proof.review_sha256() == reviewed,
        "review digest no longer matches the current evidence; run preview again"
    );
    let runtime = tokio::runtime::Handle::current();
    let outcome = tokio::task::spawn_blocking(move || {
        let parent = inputs.database.parent().unwrap_or_else(|| Path::new("."));
        let data_dir = if parent.as_os_str().is_empty() { Path::new(".") } else { parent };
        let review_key = inputs.review_key().map_err(|_| DatabaseError::Other("cannot reopen the private review key".into()))?;
        let verify = |pins: &LegacyPairPins, transport: &RegisteredTransport| {
            let source = inputs.source().map_err(|_| DatabaseError::Other("invalid recovery source".into()))?;
            runtime.block_on(transport.verify(pins, source, &inputs.git_mirror, floor))
                .map_err(|e| DatabaseError::Other(format!("registered lineage verification refused: {}", HistoricalVerifyError::classify(&e))))
        };
        if planned {
            prepare_historical_cutover(data_dir, &inputs.pair, &reviewed, review_key.as_ref(), verify)
        } else {
            enroll_verified_lineage(data_dir, &inputs.pair, &reviewed, review_key.as_ref(), verify)
        }
    }).await.context("lineage verification worker stopped")?
      .map_err(|error| database_failure(error, "Enrollment was refused. Confirm writers are stopped and the database and registered endpoints still match the review."))?;
    report(&proof);
    match outcome {
        EnrollmentOutcome::Enrolled => {
            println!("Historical SVN identity enrolled. The pair remains paused.")
        }
        EnrollmentOutcome::AlreadyEnrolled => println!(
            "The same historical SVN identity is already enrolled. The pair remains paused."
        ),
    }
    Ok(())
}
