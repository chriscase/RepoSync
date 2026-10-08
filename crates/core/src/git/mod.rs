//! Git operations for RepoSync.

pub mod client;
pub mod credentials;
pub mod github;
pub mod remote_url;
pub mod subprocess_auth;

pub use client::GitClient;
pub use credentials::{
    apply_config_remote_git_credentials, apply_git_credential_chain_state,
    apply_managed_git_credentials,
};
pub use github::GitHubClient;
pub use remote_url::derive_git_remote_url;
pub use subprocess_auth::{
    apply_git_http_auth, build_git_ls_remote_command, command_args_contain_secret,
};
