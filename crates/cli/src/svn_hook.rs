//! `reposync svn-hook`: install or remove the SVN post-commit notification.
//!
//! The notification only asks RepoSync for an early poll, so a lost or late
//! one changes nothing. The installer is guarded:
//!
//! * It writes its script to `hooks/reposync-post-commit`, a file it owns.
//! * It creates `hooks/post-commit` only when none exists, or replaces one
//!   that carries its marker. A hook it does not manage is never changed;
//!   the operator is told the one line to add instead. Symlinks at either
//!   path are refused rather than replaced or followed.
//! * The shared secret is read at commit time from a file, never written into
//!   the hook, and is passed to curl as a config file on stdin (`-K -`, which
//!   old curl releases support) rather than on its command line.
//!   SVN runs hooks with an empty environment, so an environment variable
//!   would not reach the hook.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::Subcommand;

/// First-line marker identifying files this installer manages.
pub const MARKER: &str = "# reposync-managed-hook v1";
const SCRIPT_NAME: &str = "reposync-post-commit";
const HOOK_NAME: &str = "post-commit";

#[derive(Subcommand, Debug)]
pub enum SvnHookAction {
    /// Install or update the post-commit notification in an SVN repository.
    Install {
        /// Path to the SVN repository on the server (the directory `svnadmin create` made).
        #[arg(long)]
        repository: PathBuf,
        /// Base URL of the RepoSync server, for example http://reposync.internal:8080.
        #[arg(long)]
        server_url: String,
        /// Absolute path of a file holding the SVN webhook secret, readable by
        /// the account that runs SVN hooks. Omit only when no secret is configured.
        #[arg(long)]
        secret_file: Option<PathBuf>,
        /// Show what would be written without changing anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove the notification this installer added.
    Remove {
        /// Path to the SVN repository on the server.
        #[arg(long)]
        repository: PathBuf,
        /// Show what would be removed without changing anything.
        #[arg(long)]
        dry_run: bool,
    },
}

pub fn run(action: SvnHookAction) -> Result<()> {
    let mut out = std::io::stdout();
    match action {
        SvnHookAction::Install {
            repository,
            server_url,
            secret_file,
            dry_run,
        } => install(
            &repository,
            &server_url,
            secret_file.as_deref(),
            dry_run,
            &mut out,
        ),
        SvnHookAction::Remove {
            repository,
            dry_run,
        } => remove(&repository, dry_run, &mut out),
    }
}

fn hooks_dir(repository: &Path) -> Result<PathBuf> {
    let is_repo = repository.join("format").is_file()
        && repository.join("db").is_dir()
        && repository.join("hooks").is_dir();
    if !is_repo {
        bail!(
            "{} is not an SVN repository directory (expected format, db/ and hooks/)",
            repository.display()
        );
    }
    Ok(repository.join("hooks"))
}

/// Values are embedded in single-quoted shell strings, so refuse anything
/// that could end the quote or the line.
fn check_shell_safe(what: &str, value: &str) -> Result<()> {
    if value.is_empty() || value.chars().any(|c| c == '\'' || c.is_control()) {
        bail!("{what} must be non-empty and contain no quote (') or control characters");
    }
    Ok(())
}

fn endpoint(server_url: &str) -> Result<String> {
    let base = server_url.trim().trim_end_matches('/');
    let rest = base
        .strip_prefix("http://")
        .or_else(|| base.strip_prefix("https://"))
        .with_context(|| format!("server URL must start with http:// or https://: {base}"))?;
    let host = rest.split('/').next().unwrap_or_default();
    if host.is_empty()
        || host.contains('@')
        || rest
            .chars()
            .any(|c| c.is_whitespace() || c == '?' || c == '#')
    {
        bail!("server URL needs a host and no credentials, spaces, query or fragment: {base}");
    }
    let url = format!("{base}/webhook/svn");
    check_shell_safe("server URL", &url)?;
    Ok(url)
}

/// The script installed as `hooks/reposync-post-commit`.
pub fn script(url: &str, secret_file: Option<&str>) -> String {
    let secret = secret_file.unwrap_or_default();
    format!(
        r#"#!/bin/sh
{MARKER}
# Asks RepoSync for an early poll after each commit. A lost or late
# notification changes nothing, and this never delays or fails a commit.
# Managed by `reposync svn-hook install`.
REPOS="$1"
REV="$2"
URL='{url}'
SECRET_FILE='{secret}'
case "$REV" in ''|*[!0-9]*) exit 0 ;; esac
command -v curl >/dev/null 2>&1 || exit 0
# A short, ASCII delivery ID for this repository and revision.
ID="svn-$(printf '%s' "$REPOS" | cksum | cut -d' ' -f1)-$REV"
CONFIG=""
if [ -n "$SECRET_FILE" ] && [ -r "$SECRET_FILE" ]; then
  SECRET=$(head -n 1 "$SECRET_FILE" | tr -d '\r' | sed 's/[\\"]/\\&/g')
  CONFIG="header = \"X-Webhook-Secret: $SECRET\""
fi
# The secret reaches curl as a config file on stdin, never on its command
# line (printf is a shell builtin, so it does not appear in ps either).
printf '%s\n' "$CONFIG" | curl -s -o /dev/null -m 5 -K - -X POST "$URL" \
  -H "Content-Type: application/json" \
  -H "X-Webhook-Delivery: $ID" \
  --data "{{\"revision\": $REV}}" >/dev/null 2>&1 &
exit 0
"#
    )
}

/// The `hooks/post-commit` wrapper created when the repository has none.
pub fn wrapper() -> String {
    format!(
        "#!/bin/sh\n{MARKER}\n# Created by `reposync svn-hook install`. Add other post-commit steps below.\n{}\n",
        call_line()
    )
}

/// The call added to a post-commit hook. It does nothing once the script is
/// removed, so it never makes the hook fail.
fn call_line() -> String {
    format!("[ -x \"$1/hooks/{SCRIPT_NAME}\" ] && \"$1/hooks/{SCRIPT_NAME}\" \"$1\" \"$2\" || :")
}

fn read_lossy(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Whether anything (including a dangling symlink) is at `path`.
fn occupied(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

fn refuse_symlink(path: &Path) -> Result<()> {
    if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        bail!(
            "{} is a symbolic link; RepoSync does not replace or follow it. Point it at a regular file or remove it first.",
            path.display()
        );
    }
    Ok(())
}

fn is_managed(path: &Path) -> Result<bool> {
    Ok(read_lossy(path)?
        .lines()
        .take(3)
        .any(|l| l.trim() == MARKER))
}

fn calls_script(path: &Path) -> bool {
    read_lossy(path)
        .map(|t| t.contains(&format!("hooks/{SCRIPT_NAME}")))
        .unwrap_or(false)
}

fn write_executable(path: &Path, contents: &str) -> Result<()> {
    let dir = path.parent().context("hook path has no directory")?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)
        .with_context(|| format!("creating a temporary file in {}", dir.display()))?;
    tmp.write_all(contents.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o755))?;
    }
    tmp.persist(path)
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

pub fn install(
    repository: &Path,
    server_url: &str,
    secret_file: Option<&Path>,
    dry_run: bool,
    out: &mut dyn Write,
) -> Result<()> {
    let hooks = hooks_dir(repository)?;
    let url = endpoint(server_url)?;
    if let Some(file) = secret_file {
        if !file.is_absolute() {
            bail!(
                "--secret-file must be an absolute path: hooks run from an unspecified directory"
            );
        }
        let text = file
            .to_str()
            .context("--secret-file must be a UTF-8 path")?;
        check_shell_safe("secret file path", text)?;
        if !file.is_file() {
            bail!("secret file {} does not exist", file.display());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let meta = std::fs::metadata(file)?;
            if meta.permissions().mode() & 0o004 != 0 {
                writeln!(
                    out,
                    "warning: {} is readable by every account; restrict it to the account that runs SVN hooks.",
                    file.display()
                )?;
            }
            let repo_owner = std::fs::metadata(repository.join("db"))?.uid();
            if meta.uid() != repo_owner && meta.permissions().mode() & 0o044 == 0 {
                writeln!(
                    out,
                    "warning: {} is owned by another account than the repository (uid {repo_owner}) and is not group-readable; the hook may not be able to read it, and would then send no secret.",
                    file.display()
                )?;
            }
        }
    }
    let secret_text = secret_file.and_then(Path::to_str);

    let script_path = hooks.join(SCRIPT_NAME);
    let hook_path = hooks.join(HOOK_NAME);
    refuse_symlink(&script_path)?;
    refuse_symlink(&hook_path)?;
    if occupied(&script_path) && !is_managed(&script_path)? {
        bail!(
            "{} exists and was not written by RepoSync; move it aside first",
            script_path.display()
        );
    }
    // Only an unchanged wrapper is rewritten; one the operator extended is
    // left as it is.
    let hook_plan = if !occupied(&hook_path) {
        HookPlan::Create
    } else if read_lossy(&hook_path)? == wrapper() {
        HookPlan::Replace
    } else if calls_script(&hook_path) {
        HookPlan::AlreadyCalls
    } else {
        HookPlan::Foreign
    };

    let verb = if dry_run { "Would write" } else { "Wrote" };
    if !dry_run {
        write_executable(&script_path, &script(&url, secret_text))?;
    }
    writeln!(out, "{verb} {} (notifies {url})", script_path.display())?;
    match hook_plan {
        HookPlan::Create | HookPlan::Replace => {
            if !dry_run {
                write_executable(&hook_path, &wrapper())?;
            }
            writeln!(out, "{verb} {}", hook_path.display())?;
        }
        HookPlan::AlreadyCalls => {
            writeln!(
                out,
                "{} already calls {SCRIPT_NAME}; left unchanged.",
                hook_path.display()
            )?;
        }
        HookPlan::Foreign => {
            writeln!(
                out,
                "{} exists and is not managed by RepoSync, so it was left unchanged. Add this line to it:\n  {}",
                hook_path.display(),
                call_line()
            )?;
        }
    }
    if secret_file.is_none() {
        writeln!(
            out,
            "No --secret-file: notifications are sent without a secret. If svn.webhook_secret_env is set on the server, they will be refused."
        )?;
    }
    Ok(())
}

enum HookPlan {
    Create,
    Replace,
    AlreadyCalls,
    Foreign,
}

pub fn remove(repository: &Path, dry_run: bool, out: &mut dyn Write) -> Result<()> {
    let hooks = hooks_dir(repository)?;
    let verb = if dry_run { "Would remove" } else { "Removed" };
    let hook_path = hooks.join(HOOK_NAME);
    let script_path = hooks.join(SCRIPT_NAME);
    refuse_symlink(&hook_path)?;
    refuse_symlink(&script_path)?;
    if occupied(&hook_path) {
        if is_managed(&hook_path)? {
            if read_lossy(&hook_path)? == wrapper() {
                if !dry_run {
                    std::fs::remove_file(&hook_path)?;
                }
                writeln!(out, "{verb} {}", hook_path.display())?;
            } else {
                writeln!(
                    out,
                    "{} was created by RepoSync but has other steps now; left in place. The line below now does nothing and can be removed:\n  {}",
                    hook_path.display(),
                    call_line()
                )?;
            }
        } else if calls_script(&hook_path) {
            writeln!(
                out,
                "{} is not managed by RepoSync; the line below now does nothing and can be removed:\n  {}",
                hook_path.display(),
                call_line()
            )?;
        }
    }
    if occupied(&script_path) {
        if !is_managed(&script_path)? {
            bail!(
                "{} was not written by RepoSync; left unchanged",
                script_path.display()
            );
        }
        if !dry_run {
            std::fs::remove_file(&script_path)?;
        }
        writeln!(out, "{verb} {}", script_path.display())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("format"), "5\n").unwrap();
        std::fs::create_dir(dir.path().join("db")).unwrap();
        std::fs::create_dir(dir.path().join("hooks")).unwrap();
        dir
    }

    fn run_install(repo: &Path, secret: Option<&Path>, dry: bool) -> Result<String> {
        let mut out = Vec::new();
        install(repo, "http://reposync.test:8080/", secret, dry, &mut out)?;
        Ok(String::from_utf8(out).unwrap())
    }

    #[test]
    fn refuses_a_directory_that_is_not_a_repository() {
        let dir = tempfile::tempdir().unwrap();
        assert!(run_install(dir.path(), None, false).is_err());
    }

    #[test]
    fn refuses_unsafe_urls_and_paths() {
        let repo = fake_repo();
        let mut out = Vec::new();
        for url in [
            "ftp://x",
            "http://",
            "http://user:pw@host",
            "http://host/it's",
            "http://host/a b",
            "http://host/?a=b",
            "http://host/#x",
        ] {
            assert!(
                install(repo.path(), url, None, false, &mut out).is_err(),
                "{url}"
            );
        }
        assert!(run_install(repo.path(), Some(Path::new("relative/secret")), false).is_err());
        assert!(run_install(repo.path(), Some(Path::new("/nonexistent/secret")), false).is_err());
        assert!(std::fs::read_dir(repo.path().join("hooks"))
            .unwrap()
            .next()
            .is_none());
    }

    #[test]
    fn creates_then_updates_its_own_files() {
        let repo = fake_repo();
        let hooks = repo.path().join("hooks");
        run_install(repo.path(), None, false).unwrap();
        assert_eq!(
            std::fs::read_to_string(hooks.join(HOOK_NAME)).unwrap(),
            wrapper()
        );
        let first = std::fs::read_to_string(hooks.join(SCRIPT_NAME)).unwrap();
        assert!(first.contains("URL='http://reposync.test:8080/webhook/svn'"));

        let secret = repo.path().join("secret");
        std::fs::write(&secret, "s3cret\n").unwrap();
        run_install(repo.path(), Some(&secret), false).unwrap();
        let second = std::fs::read_to_string(hooks.join(SCRIPT_NAME)).unwrap();
        assert!(second.contains(&format!("SECRET_FILE='{}'", secret.display())));
        assert!(
            !second.contains("s3cret"),
            "the secret is never written into the hook"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for name in [HOOK_NAME, SCRIPT_NAME] {
                let mode = std::fs::metadata(hooks.join(name))
                    .unwrap()
                    .permissions()
                    .mode();
                assert_eq!(mode & 0o777, 0o755, "{name}");
            }
        }
    }

    #[test]
    fn never_changes_a_hook_it_does_not_manage() {
        let repo = fake_repo();
        let hook = repo.path().join("hooks").join(HOOK_NAME);
        let foreign = "#!/bin/sh\n/usr/local/bin/mail-commit \"$1\" \"$2\"\n";
        std::fs::write(&hook, foreign).unwrap();

        let out = run_install(repo.path(), None, false).unwrap();
        assert_eq!(std::fs::read_to_string(&hook).unwrap(), foreign);
        assert!(out.contains(&call_line()), "{out}");
        assert!(repo.path().join("hooks").join(SCRIPT_NAME).is_file());

        // Remove leaves the foreign hook too, and says which line to take out.
        let mut out = Vec::new();
        remove(repo.path(), false, &mut out).unwrap();
        assert_eq!(std::fs::read_to_string(&hook).unwrap(), foreign);
        assert!(!repo.path().join("hooks").join(SCRIPT_NAME).exists());
    }

    #[test]
    fn refuses_to_replace_a_foreign_file_at_the_script_name() {
        let repo = fake_repo();
        let script_path = repo.path().join("hooks").join(SCRIPT_NAME);
        std::fs::write(&script_path, "#!/bin/sh\necho mine\n").unwrap();
        assert!(run_install(repo.path(), None, false).is_err());
        assert_eq!(
            std::fs::read_to_string(&script_path).unwrap(),
            "#!/bin/sh\necho mine\n"
        );
    }

    #[test]
    fn dry_run_changes_nothing() {
        let repo = fake_repo();
        let out = run_install(repo.path(), None, true).unwrap();
        assert!(out.contains("Would write"), "{out}");
        assert!(std::fs::read_dir(repo.path().join("hooks"))
            .unwrap()
            .next()
            .is_none());

        run_install(repo.path(), None, false).unwrap();
        let mut out = Vec::new();
        remove(repo.path(), true, &mut out).unwrap();
        assert!(repo.path().join("hooks").join(SCRIPT_NAME).exists());
        assert!(repo.path().join("hooks").join(HOOK_NAME).exists());
    }

    #[test]
    fn reinstall_keeps_a_wrapper_the_operator_extended() {
        let repo = fake_repo();
        run_install(repo.path(), None, false).unwrap();
        let hook = repo.path().join("hooks").join(HOOK_NAME);
        let extended = format!("{}/usr/local/bin/mail-commit \"$1\" \"$2\"\n", wrapper());
        std::fs::write(&hook, &extended).unwrap();
        let out = run_install(repo.path(), None, false).unwrap();
        assert_eq!(std::fs::read_to_string(&hook).unwrap(), extended);
        assert!(out.contains("already calls"), "{out}");
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_at_either_path_are_refused_and_kept() {
        for name in [HOOK_NAME, SCRIPT_NAME] {
            let repo = fake_repo();
            let link = repo.path().join("hooks").join(name);
            std::os::unix::fs::symlink("/nonexistent/shared-hook", &link).unwrap();
            assert!(run_install(repo.path(), None, false).is_err(), "{name}");
            let mut out = Vec::new();
            assert!(remove(repo.path(), false, &mut out).is_err(), "{name}");
            assert!(std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink());
        }
    }

    #[test]
    fn a_non_utf8_hook_is_treated_as_foreign() {
        let repo = fake_repo();
        let hook = repo.path().join("hooks").join(HOOK_NAME);
        std::fs::write(&hook, b"\x7fELF\xff\xfe binary").unwrap();
        let out = run_install(repo.path(), None, false).unwrap();
        assert!(out.contains("Add this line"), "{out}");
        assert_eq!(std::fs::read(&hook).unwrap(), b"\x7fELF\xff\xfe binary");
    }

    #[test]
    fn remove_keeps_a_wrapper_the_operator_extended() {
        let repo = fake_repo();
        run_install(repo.path(), None, false).unwrap();
        let hook = repo.path().join("hooks").join(HOOK_NAME);
        let extended = format!("{}/usr/local/bin/other \"$1\"\n", wrapper());
        std::fs::write(&hook, &extended).unwrap();
        let mut out = Vec::new();
        remove(repo.path(), false, &mut out).unwrap();
        assert_eq!(std::fs::read_to_string(&hook).unwrap(), extended);
        assert!(!repo.path().join("hooks").join(SCRIPT_NAME).exists());
    }
}
