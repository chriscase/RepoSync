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

/// Build `git ls-remote` against a clean remote URL (credentials via env only).
pub fn build_git_ls_remote_command(clean_url: &str, token: Option<&str>, refspec: &str) -> Command {
    let mut cmd = Command::new("git");
    cmd.args(["ls-remote", "--exit-code", clean_url, refspec]);
    if let Some(tok) = token {
        apply_git_http_auth(&mut cmd, tok);
    } else {
        cmd.env("GIT_TERMINAL_PROMPT", "0");
    }
    cmd
}

/// True when `token` appears in any command argument (used by tests).
pub fn command_args_contain_secret(cmd: &Command, token: &str) -> bool {
    cmd.get_args()
        .any(|arg| arg.to_string_lossy().contains(token))
}
