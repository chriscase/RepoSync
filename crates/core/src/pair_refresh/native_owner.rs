//! Native process identity for authenticated recovery. Missing/unsupported boot
//! or PID-namespace evidence, EPERM and a live/reused PID never prove death.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::errors::DatabaseError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeProcessOwner {
    version: u8,
    platform: String,
    host_digest: String,
    boot_digest: String,
    pid_namespace: String,
    pid: u32,
    uid: u32,
}

fn refused(detail: &str) -> DatabaseError {
    DatabaseError::Other(format!("pair refresh native owner refused: {detail}"))
}

fn hash(value: &[u8]) -> String {
    hex::encode(Sha256::digest(value))
}

impl NativeProcessOwner {
    pub(crate) fn capture() -> Option<Self> {
        let (platform, host_digest, boot_digest, pid_namespace) = capture_domain()?;
        let owner = Self {
            version: 1,
            platform,
            host_digest,
            boot_digest,
            pid_namespace,
            pid: std::process::id(),
            uid: rustix::process::geteuid().as_raw(),
        };
        owner.validate().ok()?;
        Some(owner)
    }

    pub(crate) fn validate(&self) -> Result<(), DatabaseError> {
        let digest = |value: &str| {
            value.len() == 64
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        };
        if self.version != 1
            || !matches!(self.platform.as_str(), "linux" | "macos")
            || self.pid == 0
            || self.pid > i32::MAX as u32
            || !digest(&self.host_digest)
            || !digest(&self.boot_digest)
            || self.pid_namespace.is_empty()
            || self.pid_namespace.len() > 128
        {
            return Err(refused("unsupported native identity"));
        }
        Ok(())
    }

    pub(crate) fn same_domain(&self, other: &Self) -> bool {
        self.version == other.version
            && self.platform == other.platform
            && self.host_digest == other.host_digest
            && self.boot_digest == other.boot_digest
            && self.pid_namespace == other.pid_namespace
            && self.uid == other.uid
    }

    pub(crate) fn require_current(&self) -> Result<(), DatabaseError> {
        if Self::capture().as_ref() != Some(self) {
            return Err(refused("current process/domain is unknown or changed"));
        }
        Ok(())
    }
    pub(crate) fn pid(&self) -> u32 {
        self.pid
    }
}

/// Private construction is a fresh OS observation, not a client PID claim.
pub(crate) struct NativeDeadOwnerProof {
    old: NativeProcessOwner,
    current: NativeProcessOwner,
}

impl NativeDeadOwnerProof {
    pub(crate) fn capture(old: Option<&NativeProcessOwner>) -> Result<Self, DatabaseError> {
        let old = old.ok_or_else(|| refused("recorded native owner is unknown"))?;
        old.validate()?;
        let current = NativeProcessOwner::capture()
            .ok_or_else(|| refused("current native domain is unknown"))?;
        if !old.same_domain(&current) || old.pid == current.pid {
            return Err(refused("owner domain differs or owner is current"));
        }
        require_absent(old.pid)?;
        let proof = Self {
            old: old.clone(),
            current,
        };
        proof.require_current()?;
        Ok(proof)
    }

    pub(crate) fn require_current(&self) -> Result<(), DatabaseError> {
        self.old.validate()?;
        self.current.require_current()?;
        if !self.old.same_domain(&self.current) {
            return Err(refused("native owner domain changed"));
        }
        require_absent(self.old.pid)?;
        self.current.require_current()
    }

    pub(crate) fn old(&self) -> &NativeProcessOwner {
        &self.old
    }
    pub(crate) fn current(&self) -> &NativeProcessOwner {
        &self.current
    }

    pub(crate) fn fence_host(&self) -> String {
        // Unlike the ordinary fence's optional environment label, this remains
        // stable across restart and is available only after native proof.
        format!(
            "pair-refresh-native-{}",
            hash(
                format!(
                    "{}:{}:{}:{}:{}",
                    self.current.platform,
                    self.current.host_digest,
                    self.current.boot_digest,
                    self.current.pid_namespace,
                    self.current.uid
                )
                .as_bytes()
            )
        )
    }
}

fn require_absent(pid: u32) -> Result<(), DatabaseError> {
    if pid == 0 || pid > i32::MAX as u32 {
        return Err(refused("invalid owner PID"));
    }
    // SAFETY: signal 0 has no effect and only probes this exact native namespace.
    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if result == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
        return Ok(());
    }
    Err(refused("owner is live, reused or liveness is unknown"))
}

#[cfg(target_os = "linux")]
fn capture_domain() -> Option<(String, String, String, String)> {
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    fn read_native(path: &str) -> Option<Vec<u8>> {
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
            .ok()?;
        let metadata = file.metadata().ok()?;
        if !metadata.is_file() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
            return None;
        }
        let mut bytes = Vec::new();
        file.by_ref().take(4097).read_to_end(&mut bytes).ok()?;
        (bytes.len() <= 4096).then_some(bytes)
    }
    let machine = read_native("/etc/machine-id")?;
    let machine = std::str::from_utf8(&machine).ok()?.trim();
    if machine.len() != 32
        || !machine
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    let boot = read_native("/proc/sys/kernel/random/boot_id")?;
    let boot = uuid::Uuid::parse_str(std::str::from_utf8(&boot).ok()?.trim()).ok()?;
    if boot.is_nil() {
        return None;
    }
    let proc = rustix::fs::statfs("/proc").ok()?;
    if proc.f_type != rustix::fs::PROC_SUPER_MAGIC {
        return None;
    }
    let namespace = std::fs::metadata("/proc/self/ns/pid").ok()?;
    Some((
        "linux".into(),
        hash(machine.as_bytes()),
        hash(boot.as_bytes()),
        format!("{}:{}", namespace.dev(), namespace.ino()),
    ))
}

#[cfg(target_os = "macos")]
fn capture_domain() -> Option<(String, String, String, String)> {
    let mut host = [0_u8; 16];
    let timeout = libc::timespec {
        tv_sec: 1,
        tv_nsec: 0,
    };
    // SAFETY: the OS writes one uuid_t; timeout is initialized and read-only.
    if unsafe { libc::gethostuuid(host.as_mut_ptr(), &timeout) } != 0 || host == [0; 16] {
        return None;
    }
    let mut bytes = [0_u8; 128];
    let mut length = bytes.len();
    // SAFETY: bounded output buffer and initialized size; no new value supplied.
    if unsafe {
        libc::sysctlbyname(
            c"kern.bootsessionuuid".as_ptr(),
            bytes.as_mut_ptr().cast(),
            &mut length,
            std::ptr::null_mut(),
            0,
        )
    } != 0
        || length == 0
        || length > bytes.len()
    {
        return None;
    }
    let boot = uuid::Uuid::parse_str(
        std::str::from_utf8(&bytes[..length])
            .ok()?
            .trim_end_matches('\0'),
    )
    .ok()?;
    if boot.is_nil() {
        return None;
    }
    Some((
        "macos".into(),
        hash(&host),
        hash(boot.as_bytes()),
        "global_macos_pid_namespace".into(),
    ))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn capture_domain() -> Option<(String, String, String, String)> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_identity_and_current_owner_never_prove_death() {
        assert!(NativeDeadOwnerProof::capture(None).is_err());
        match NativeProcessOwner::capture() {
            Some(owner) => {
                owner.require_current().unwrap();
                assert!(NativeDeadOwnerProof::capture(Some(&owner)).is_err());
            }
            None => assert!(NativeDeadOwnerProof::capture(None).is_err()),
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn actual_native_child_requires_esrch_in_the_same_stable_namespace() {
        struct Child(std::process::Child);
        impl Drop for Child {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut child = Child(
            std::process::Command::new("sleep")
                .arg("60")
                .spawn()
                .unwrap(),
        );
        let mut old = NativeProcessOwner::capture()
            .expect("standard Linux native identity must be available");
        old.pid = child.0.id();
        assert!(NativeDeadOwnerProof::capture(Some(&old)).is_err());
        child.0.kill().unwrap();
        child.0.wait().unwrap();
        let proof = NativeDeadOwnerProof::capture(Some(&old)).unwrap();
        proof.require_current().unwrap();
        let mut changed_boot = old.clone();
        changed_boot.boot_digest = "0".repeat(64);
        assert!(NativeDeadOwnerProof::capture(Some(&changed_boot)).is_err());
        let mut changed_namespace = old.clone();
        changed_namespace.pid_namespace.push_str("-foreign");
        assert!(NativeDeadOwnerProof::capture(Some(&changed_namespace)).is_err());
        let mut changed_host = old;
        changed_host.host_digest = "0".repeat(64);
        assert!(NativeDeadOwnerProof::capture(Some(&changed_host)).is_err());
    }
}
