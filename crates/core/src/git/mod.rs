//! Git operations for RepoSync.

pub mod client;
pub mod credentials;
pub mod github;
pub mod remote_url;
pub mod startup_handoff;
pub mod subprocess_auth;

pub use client::GitClient;

/// Why `name` cannot be used as a pair's Git branch, or `None` when it can.
///
/// The name is the bare branch (`main`, `release/2.x`), exactly as the engine
/// fetches and pushes `refs/heads/<name>`. Git's own reference-name rules
/// apply, and a leading `-` is refused because Git refuses it for branches
/// and it reads as an option on a command line.
pub fn git_branch_name_problem(name: &str) -> Option<&'static str> {
    if name.trim().is_empty() {
        return Some("must not be empty");
    }
    if name.starts_with("refs/") {
        return Some("must be a branch name such as 'main', not a full ref");
    }
    if name == "HEAD" || name == "@" {
        return Some("must not be 'HEAD' or '@', which Git reads as the current commit");
    }
    if name.starts_with('-') {
        return Some("must not start with '-'");
    }
    if !git2::Reference::is_valid_name(&format!("refs/heads/{name}")) {
        return Some(
            "is not a valid Git branch name (no spaces, control characters, \
             '..', '~', '^', ':', '?', '*', '[', '\\', '@{', a leading or trailing '/', \
             or a trailing '.' or '.lock')",
        );
    }
    None
}

#[cfg(test)]
mod branch_name_tests {
    use super::git_branch_name_problem;

    #[test]
    fn accepts_ordinary_branch_names() {
        for name in [
            "main",
            "develop",
            "release/2.x",
            "feature/ABC-12_x",
            "v1.0",
            "user@host",
        ] {
            assert_eq!(git_branch_name_problem(name), None, "{name}");
        }
    }

    #[test]
    fn refuses_names_git_or_the_engine_cannot_use() {
        for name in [
            "",
            "   ",
            " main",
            "main ",
            "feature branch",
            "refs/heads/main",
            "HEAD",
            "-main",
            "a..b",
            "a~1",
            "a^",
            "a:b",
            "a?",
            "a*",
            "a[b",
            "a\\b",
            "a@{1}",
            "/main",
            "main/",
            "a//b",
            "main.",
            "main.lock",
            "@",
            "line\nbreak",
            "tab\there",
        ] {
            assert!(
                git_branch_name_problem(name).is_some(),
                "{name:?} was accepted"
            );
        }
    }
}
pub use credentials::{
    apply_config_remote_git_credentials, apply_git_credential_chain_state,
    apply_git_credential_chain_state_for_sync, apply_managed_git_credentials,
    resolve_git_http_auth_token, resolve_git_http_auth_token_for_clone,
    resolve_git_http_auth_token_for_workdir, sync_git_http_auth_from_resolution,
};
pub use github::GitHubClient;
pub use remote_url::derive_git_remote_url;
pub use subprocess_auth::{
    apply_git_http_auth, apply_git_http_auth_optional, apply_git_http_auth_tokio,
    apply_git_http_auth_tokio_optional, build_git_cli_command, build_git_ls_remote_command,
    command_args_contain_secret, command_env_has_git_http_auth, git_cli_output, git_cli_subcommand,
    GIT_REMOTE_CLI_SUBCOMMANDS,
};
