//! Git operations for RepoSync.

pub mod client;
pub mod credentials;
pub mod github;
pub mod remote_url;
pub mod startup_handoff;
pub mod subprocess_auth;

pub use client::GitClient;
pub use credentials::{
    apply_config_remote_git_credentials, apply_git_credential_chain_state,
    apply_git_credential_chain_state_for_sync, apply_managed_git_credentials,
    resolve_git_http_auth_token, resolve_git_http_auth_token_for_workdir,
    sync_git_http_auth_from_resolution,
};
pub use github::GitHubClient;
pub use remote_url::derive_git_remote_url;
pub use subprocess_auth::{
    apply_git_http_auth, apply_git_http_auth_optional, apply_git_http_auth_tokio,
    apply_git_http_auth_tokio_optional, build_git_cli_command, build_git_ls_remote_command,
    command_args_contain_secret, command_env_has_git_http_auth, git_cli_output, git_cli_subcommand,
    GIT_REMOTE_CLI_SUBCOMMANDS,
};
