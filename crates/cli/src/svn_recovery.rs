use std::ffi::OsStr;
use std::path::Path;

use anyhow::{anyhow, ensure, Context, Result};
use reposync_core::db::historical_enrollment::{
    enroll_verified_lineage, prepare_historical_cutover, EnrollmentOutcome, LegacyPairPins,
};
use reposync_core::db::historical_transport::{read_offline_recovery_inputs, RegisteredTransport};
use reposync_core::errors::DatabaseError;
use reposync_core::git::remote_url::derive_git_remote_url;
use reposync_core::legacy_svn_recovery::{HistoricalSvnSource, VerifiedHistoricalLineage};

use crate::svn_recovery_args::{RecoveryAction, RecoveryInputs};

impl RecoveryInputs {
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
    println!("Review digest: {}", verified.review_sha256());
}

fn database_failure(error: DatabaseError, fallback: &'static str) -> anyhow::Error {
    // This prefix is owned by the persistence module and contains its fixed
    // operator guidance. Raw SQLite diagnostics can contain source identifiers.
    if let DatabaseError::Other(message) = error {
        if let Some(reason) = message.strip_prefix("historical lineage enrollment refused: ") {
            return anyhow!("{fallback} {reason}");
        }
    }
    anyhow!(fallback)
}

pub(crate) async fn run(action: RecoveryAction) -> Result<()> {
    let (inputs, reviewed) = match action {
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
            (inputs, Some(reviewed_digest))
        }
    };
    ensure!(
        inputs.database.file_name() == Some(OsStr::new("reposync.db")),
        "supply the original data directory's reposync.db"
    );
    let planned = inputs.svn_backup.is_none();
    let (pair, transport) = read_offline_recovery_inputs(&inputs.database, &inputs.pair, planned)
        .map_err(|error| database_failure(error, "Cannot inspect the stopped installation for the selected recovery path. Check the database, its schema and the paused pair."))?;
    let proof = transport.verify(&pair, inputs.source()?, &inputs.git_mirror).await
        .map_err(|_| anyhow!("Could not verify the registered SVN history and Git ref through the retained mappings. Check read access, the complete Git mirror, and the selected source."))?;
    let Some(reviewed) = reviewed else {
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
        let verify = |pins: &LegacyPairPins, transport: &RegisteredTransport| {
            let source = inputs.source().map_err(|_| DatabaseError::Other("invalid recovery source".into()))?;
            runtime.block_on(transport.verify(pins, source, &inputs.git_mirror))
                .map_err(|_| DatabaseError::Other("historical evidence could not be revalidated under the writer lock".into()))
        };
        if planned {
            prepare_historical_cutover(data_dir, &inputs.pair, &reviewed, verify)
        } else {
            enroll_verified_lineage(data_dir, &inputs.pair, &reviewed, verify)
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
