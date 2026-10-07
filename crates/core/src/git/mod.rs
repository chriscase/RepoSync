//! Git operations for RepoSync.

pub mod client;
pub mod credentials;
pub mod github;
pub mod remote_url;

pub use client::GitClient;
pub use credentials::{apply_git_credential_chain_state, apply_managed_git_credentials};
pub use github::GitHubClient;
pub use remote_url::derive_git_remote_url;
