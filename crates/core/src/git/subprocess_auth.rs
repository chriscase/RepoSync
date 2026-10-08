//! HTTP(S) Git CLI authentication without embedding tokens in argv.

use std::process::Command;

use base64::Engine;

/// Environment entries for `GIT_CONFIG_COUNT` / `GIT_CONFIG_KEY_n` / `GIT_CONFIG_VALUE_n`.
pub fn git_http_auth_env(token: &str) -> Vec<(String, String)> {
    let basic = base64::engine::general_purpose::STANDARD.encode(format!("x-access-token:{token}"));
    let header = format!("Authorization: Basic {basic}");
    vec![
        ("GIT_CONFIG_COUNT".into(), "1".into()),
        ("GIT_CONFIG_KEY_0".into(), "http.extraHeader".into()),
        ("GIT_CONFIG_VALUE_0".into(), header),
        ("GIT_TERMINAL_PROMPT".into(), "0".into()),
    ]
}

/// Apply HTTP Git token auth to a `std::process::Command` (token never appears in argv).
pub fn apply_git_http_auth(cmd: &mut Command, token: &str) {
    for (key, value) in git_http_auth_env(token) {
        cmd.env(key, value);
    }
}

/// Apply auth when a token is present; otherwise disable terminal prompts only.
pub fn apply_git_http_auth_optional(cmd: &mut Command, token: Option<&str>) {
    if let Some(tok) = token.filter(|t| !t.is_empty()) {
        apply_git_http_auth(cmd, tok);
    } else {
        cmd.env("GIT_TERMINAL_PROMPT", "0");
    }
}

/// Same as [`apply_git_http_auth`] for `tokio::process::Command`.
pub fn apply_git_http_auth_tokio(cmd: &mut tokio::process::Command, token: &str) {
    for (key, value) in git_http_auth_env(token) {
        cmd.env(key, value);
    }
}

/// Optional-token variant for async git subprocesses.
pub fn apply_git_http_auth_tokio_optional(cmd: &mut tokio::process::Command, token: Option<&str>) {
    if let Some(tok) = token.filter(|t| !t.is_empty()) {
        apply_git_http_auth_tokio(cmd, tok);
    } else {
        cmd.env("GIT_TERMINAL_PROMPT", "0");
    }
}

/// Build `git ls-remote` against a clean remote URL (credentials via env only).
pub fn build_git_ls_remote_command(clean_url: &str, token: Option<&str>, refspec: &str) -> Command {
    let mut cmd = Command::new("git");
    cmd.args(["ls-remote", "--exit-code", clean_url, refspec]);
    apply_git_http_auth_optional(&mut cmd, token);
    cmd
}

/// True when `token` appears in any command argument (used by tests).
pub fn command_args_contain_secret(cmd: &Command, token: &str) -> bool {
    cmd.get_args()
        .any(|arg| arg.to_string_lossy().contains(token))
}

/// True when the child env carries the GIT_CONFIG http.extraHeader for `token`.
pub fn command_env_has_git_http_auth(cmd: &Command, token: &str) -> bool {
    let expected = git_http_auth_env(token);
    expected.iter().all(|(key, value)| {
        cmd.get_envs()
            .any(|(k, v)| k == key.as_str() && v.map(|s| s == value.as_str()).unwrap_or(false))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "ghp_subprocess_auth_test_secret";

    #[test]
    fn import_push_subprocess_uses_env_auth_not_argv() {
        let mut push = Command::new("git");
        push.args(["push", "--progress", "origin", "main"])
            .current_dir("/tmp");
        apply_git_http_auth(&mut push, TOKEN);
        assert!(command_env_has_git_http_auth(&push, TOKEN));
        assert!(!command_args_contain_secret(&push, TOKEN));
        assert!(!command_args_contain_secret(&push, "x-access-token"));
    }

    #[test]
    fn import_ls_remote_origin_subprocess_uses_env_auth() {
        let mut inspect = Command::new("git");
        inspect.args(["ls-remote", "--exit-code", "origin", "refs/heads/main"]);
        apply_git_http_auth_optional(&mut inspect, Some(TOKEN));
        assert!(command_env_has_git_http_auth(&inspect, TOKEN));
        assert!(!command_args_contain_secret(&inspect, TOKEN));
    }

    #[test]
    fn late_pair_fetch_subprocess_uses_env_auth() {
        let mut fetch = Command::new("git");
        fetch.args([
            "fetch",
            "--no-tags",
            "origin",
            "refs/heads/feature:refs/heads/feature",
        ]);
        apply_git_http_auth_optional(&mut fetch, Some(TOKEN));
        assert!(command_env_has_git_http_auth(&fetch, TOKEN));
        assert!(!command_args_contain_secret(&fetch, TOKEN));
    }

    #[test]
    fn build_ls_remote_command_uses_env_auth() {
        let cmd = build_git_ls_remote_command(
            "https://github.com/example/repo.git",
            Some(TOKEN),
            "refs/heads/main",
        );
        assert!(command_env_has_git_http_auth(&cmd, TOKEN));
        assert!(!command_args_contain_secret(&cmd, TOKEN));
        assert!(cmd
            .get_args()
            .any(|a| a == "https://github.com/example/repo.git"));
    }
}
