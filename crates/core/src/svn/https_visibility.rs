//! Private limits and trust for the opt-in Personal Apache DAV pilot only.

use std::{
    io::{self, Read},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use crate::errors::SvnError;

const CA_MAX_BYTES: u64 = 1024 * 1024;
const PLAN_LIMIT: Duration = Duration::from_secs(90);
const COMMAND_LIMIT: Duration = Duration::from_secs(30);
const SCRATCH_BYTES: u64 = 512 * 1024 * 1024;
const SCRATCH_ENTRIES: usize = 25_000;
const PER_FILE_BYTES: u64 = 64 * 1024 * 1024;
const STDOUT_BYTES: u64 = 16 * 1024 * 1024;
const STDERR_BYTES: u64 = 1024 * 1024;

pub(crate) struct HttpsVisibility {
    pub(crate) trust_sha256: String,
    deadline: Instant,
    directory: tempfile::TempDir,
}

fn held(reason: &'static str, detail: &'static str) -> SvnError {
    SvnError::LineageRefused {
        reason: reason.into(),
        detail: detail.into(),
    }
}

/// Freeze explicitly selected trust bytes once. This never changes any home
/// config or system trust bundle, nor enables any cert failure override.
pub(crate) fn install_trust(path: &Path, private: &Path) -> Result<String, SvnError> {
    if !path.is_absolute() {
        return Err(held(
            "svn_https_trust_unavailable",
            "HTTPS SVN CA bundle must use an absolute path",
        ));
    }
    let bad = || {
        held("svn_https_trust_unavailable", "HTTPS SVN CA bundle must be a readable, small, regular PEM file; refusing system-trust fallback")
    };
    if std::fs::canonicalize(path).map_err(|_| bad())? != path {
        return Err(bad());
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|_| bad())?;
    let meta = file.metadata().map_err(|_| bad())?;
    if !meta.is_file() || meta.len() > CA_MAX_BYTES {
        return Err(bad());
    }
    let mut pem = Vec::new();
    file.take(CA_MAX_BYTES + 1)
        .read_to_end(&mut pem)
        .map_err(|_| bad())?;
    if pem.len() as u64 > CA_MAX_BYTES
        || !reqwest::Certificate::from_pem_bundle(&pem).is_ok_and(|certs| !certs.is_empty())
    {
        return Err(bad());
    }
    let pem_text = std::str::from_utf8(&pem).map_err(|_| bad())?;
    if pem_text
        .lines()
        .any(|line| line.starts_with("-----BEGIN ") && line != "-----BEGIN CERTIFICATE-----")
    {
        return Err(bad());
    }
    use sha2::{Digest, Sha256};
    let fingerprint = hex::encode(Sha256::digest(&pem));
    let bundle = private.join("reposync-scoped-ca.pem");
    std::fs::write(&bundle, pem)?;
    let bundle = bundle
        .to_str()
        .filter(|s| !s.contains(['\n', '\r', ';']))
        .ok_or_else(bad)?;
    use std::io::Write;
    let mut servers = std::fs::OpenOptions::new()
        .append(true)
        .open(private.join("servers"))?;
    writeln!(
        servers,
        "ssl-trust-default-ca = no\nssl-authority-files = {bundle}"
    )?;
    Ok(fingerprint)
}

impl HttpsVisibility {
    pub(crate) fn new(trust_sha256: String) -> io::Result<Self> {
        Ok(Self {
            deadline: Instant::now() + PLAN_LIMIT,
            trust_sha256,
            directory: tempfile::Builder::new()
                .prefix("reposync-personal-https-")
                .tempdir()?,
        })
    }

    pub(crate) fn scratch(&self) -> &Path {
        self.directory.path()
    }

    pub(crate) fn deadline(&self) -> Result<Duration, SvnError> {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(held("svn_https_planning_timeout", "90-second Personal HTTPS planning deadline reached; current SVN revision remains pending"));
        }
        Ok(remaining.min(COMMAND_LIMIT))
    }

    pub(crate) fn check(&self) -> Result<(), SvnError> {
        self.deadline()?;
        let mut paths: Vec<PathBuf> = vec![self.scratch().to_owned()];
        let mut entries = 0;
        let mut bytes = 0u64;
        while let Some(path) = paths.pop() {
            self.deadline()?;
            let contents = match std::fs::symlink_metadata(&path) {
                Ok(meta) if meta.is_dir() => match std::fs::read_dir(&path) {
                    Ok(contents) => contents,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                    Err(e) => return Err(e.into()),
                },
                Ok(_) => continue,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            for entry in contents {
                let entry = entry?;
                entries += 1;
                if entries > SCRATCH_ENTRIES {
                    return Err(self.storage());
                }
                let meta = match std::fs::symlink_metadata(entry.path()) {
                    Ok(meta) => meta,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                    Err(e) => return Err(e.into()),
                };
                if meta.is_dir() {
                    paths.push(entry.path());
                } else if meta.is_file() {
                    bytes = bytes.saturating_add(meta.len());
                    if bytes > SCRATCH_BYTES {
                        return Err(self.storage());
                    }
                }
            }
        }
        Ok(())
    }

    fn storage(&self) -> SvnError {
        held("svn_https_scratch_limit", "Personal HTTPS scratch guard reached 512 MiB or 25000 entries; current SVN revision remains pending")
    }

    pub(crate) fn configure(&self, command: &mut tokio::process::Command) {
        for name in ["TMPDIR", "TMP", "TEMP"] {
            command.env(name, self.scratch());
        }
        for name in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
        ] {
            command.env_remove(name);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // SAFETY: only the async-signal-safe setrlimit syscall is used in
            // the child between fork and exec; no global host quota is changed.
            unsafe {
                command.as_std_mut().pre_exec(|| {
                    let mut current = std::mem::MaybeUninit::<libc::rlimit>::uninit();
                    if libc::getrlimit(libc::RLIMIT_FSIZE, current.as_mut_ptr()) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                    let mut current = current.assume_init();
                    current.rlim_cur = current.rlim_cur.min(PER_FILE_BYTES as libc::rlim_t);
                    if libc::setrlimit(libc::RLIMIT_FSIZE, &current) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
    }

    pub(crate) async fn run(
        &self,
        mut command: tokio::process::Command,
    ) -> Result<std::process::Output, SvnError> {
        self.check()?;
        self.configure(&mut command);
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stdout = tempfile::NamedTempFile::new_in(self.scratch())?;
        let stderr = tempfile::NamedTempFile::new_in(self.scratch())?;
        let check_output = || -> Result<(), SvnError> {
            if stdout.as_file().metadata()?.len() > STDOUT_BYTES
                || stderr.as_file().metadata()?.len() > STDERR_BYTES
            {
                return Err(held("svn_https_output_limit", "Personal HTTPS command output exceeds its capture budget; revision remains pending"));
            }
            Ok(())
        };
        let run = crate::process::run_to_owned_files(
            command,
            self.deadline()?,
            Some(&cancel),
            stdout.reopen()?,
            stderr.reopen()?,
        );
        tokio::pin!(run);
        loop {
            tokio::select! {
                result = &mut run => {
                    let mut result = result.map_err(|e| if e.kind() == io::ErrorKind::TimedOut {
                        held("svn_https_command_timeout", "Personal HTTPS SVN command timed out; current SVN revision remains pending")
                    } else { SvnError::IoError(e) })?;
                    self.check()?;
                    check_output()?;
                    stdout.reopen()?.take(STDOUT_BYTES + 1).read_to_end(&mut result.stdout)?;
                    stderr.reopen()?.take(STDERR_BYTES + 1).read_to_end(&mut result.stderr)?;
                    return Ok(result);
                },
                _ = tokio::time::sleep(Duration::from_millis(250)) => {
                    if let Err(error) = self.check().and_then(|_| check_output()) {
                        cancel.store(true, std::sync::atomic::Ordering::Release);
                        // Do not drop the subprocess future: wait for the
                        // supervisor to terminate and reap its owned group.
                        let _ = run.await;
                        return Err(error);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_or_retargeted_ca_does_not_seed_private_trust() {
        let private = tempfile::tempdir().unwrap();
        let invalid = private.path().join("invalid");
        std::fs::write(&invalid, "not a certificate bundle").unwrap();
        assert!(install_trust(&invalid, private.path()).is_err());
        #[cfg(unix)]
        {
            let reparse = private.path().join("alias");
            std::os::unix::fs::symlink(&invalid, &reparse).unwrap();
            assert!(install_trust(&reparse, private.path()).is_err());
        }
        assert!(!private.path().join("reposync-scoped-ca.pem").exists());
    }

    #[test]
    fn logical_scratch_beyond_budget_holds_without_discarding_private_evidence() {
        let pilot = HttpsVisibility::new("fixture".into()).unwrap();
        let path = pilot.scratch().join("sparse");
        std::fs::File::create(&path)
            .unwrap()
            .set_len(SCRATCH_BYTES + 1)
            .unwrap();
        assert!(pilot.check().unwrap_err().to_string().contains("scratch"));
        assert!(path.is_file());
    }

    #[tokio::test]
    async fn oversized_stdout_does_not_become_readable_svn_evidence() {
        let pilot = HttpsVisibility::new("fixture".into()).unwrap();
        let mut command = tokio::process::Command::new("head");
        command
            .arg("-c")
            .arg((STDOUT_BYTES + 1).to_string())
            .arg("/dev/zero");
        let error = pilot.run(command).await.unwrap_err().to_string();
        assert!(error.contains("command output"), "{error}");
    }

    #[tokio::test]
    async fn child_file_limit_does_not_raise_inherited_ceiling() {
        let pilot = HttpsVisibility::new("fixture".into()).unwrap();
        let destination = pilot.scratch().join("oversize");
        let mut command = tokio::process::Command::new("truncate");
        command
            .arg("-s")
            .arg((PER_FILE_BYTES + 1).to_string())
            .arg(&destination);
        let output = pilot.run(command).await.unwrap();
        assert!(!output.status.success());
        assert!(std::fs::metadata(destination).unwrap().len() <= PER_FILE_BYTES);
    }

    #[tokio::test]
    async fn elapsed_planning_deadline_refuses_before_starting_another_command() {
        let mut pilot = HttpsVisibility::new("fixture".into()).unwrap();
        pilot.deadline = Instant::now();
        let marker = pilot.scratch().join("must-not-create");
        let mut command = tokio::process::Command::new("touch");
        command.arg(&marker);
        assert!(pilot
            .run(command)
            .await
            .unwrap_err()
            .to_string()
            .contains("planning deadline"));
        assert!(!marker.exists());
    }
}
