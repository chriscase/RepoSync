use std::path::Path;

use anyhow::{bail, Context, Result};
use reposync_core::db::upgrade_preflight;

pub(super) fn run(path: &Path) -> Result<()> {
    let report = upgrade_preflight::inspect(path)
        .context("Could not determine legacy Team upgrade status from the supplied snapshot")?;
    println!(
        "Installed Team schema v{} (shared by legacy/team-install and legacy/team-install-hotfix).",
        report.schema_version
    );
    for (index, pair) in report.pairs.iter().enumerate() {
        // The old daemons generate UUIDs. Other labels can contain credentials,
        // URLs or terminal controls; keep them out of operator transcripts.
        let id = pair.id.map(|id| id.to_string()).unwrap_or_else(|| {
            format!(
                "registration {} (nonstandard identifier omitted)",
                index + 1
            )
        });
        let finding = if report.unexpected_identity_records {
            "Unknown; identity or import records have unverified provenance"
        } else if pair.retained {
            "Retained history; historical SVN identity missing"
        } else {
            "No pair-specific history found"
        };
        println!(
            "  {id}  {finding} (stored SVN revision {})",
            pair.last_svn_revision
        );
    }
    if report.pairs.is_empty() {
        println!("No registered pairs found.");
    }
    if report.unattributed_history {
        println!("Unknown ownership: this snapshot also contains history without a current registration link.");
    }
    if report.unexpected_identity_records {
        println!("Unknown provenance: these installed versions did not write the identity or import records found here.");
    }
    if report.poll_only {
        println!("A sync poll was recorded. This timestamp also occurs after an empty or failed poll. No stored revision, commit or other publication evidence was found. Check the old deployment's records for any published work before resuming.");
        bail!("Inconclusive: the poll alone cannot establish whether any pair published");
    }
    if report.needs_review() {
        println!("Keep affected pairs paused. Identify a trusted pre-upgrade SVN backup or dump, its original repository URL and branch creation or copy history through the retained revision. A saved working copy can corroborate the URL and UUID. Match the configured Git ref and full mapped commits to that history.");
        bail!("Historical evidence requires operator review before incremental sync");
    }
    println!("No retained history found in the inspected database records. Live SVN and Git continuity were outside this check.");
    Ok(())
}
