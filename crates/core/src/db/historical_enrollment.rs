//! Historical Team root enrollment after an installed database has been migrated.
use serde::Serialize;

/// Existing registered ownership and retained evidence for a single legacy root.
/// The verifier binds these pins to the SVN and Git observations in its review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LegacyPairPins {
    pub repo_id: String,
    pub scope_uuid: String,
    pub generation: i64,
    pub svn_url: String,
    pub svn_branch: String,
    pub git_provider: String,
    pub git_api_url: String,
    pub git_repo: String,
    pub git_branch: String,
    pub last_svn_rev: i64,
    pub last_git_sha: String,
    pub mappings: Vec<(i64, String)>,
    pub storage_sha256: String,
}
