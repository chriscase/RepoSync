//! Commit text travels through a private regular file, never an argv element.

use std::io::{Seek, Write};

/// Return a rewound anonymous file for the child's standard input. Moving it
/// into `Stdio` keeps the underlying file alive in the child even if the caller
/// future is dropped. There is no named path to remove while the child reads.
pub(crate) fn input(message: &str) -> std::io::Result<std::fs::File> {
    let mut file = tempfile::tempfile()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Linux's anonymous-file path can inherit a broader creation mode.
        // Restrict this newly owned file before any message bytes are written.
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(message.as_bytes())?;
    file.flush()?;
    file.rewind()?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn large_utf8_message_is_complete_rewound_and_private() {
        let message = format!(
            "{}\n\n[reposync] synthetic marker\nquotes: '\"\\\n",
            "é中文😀 multiline\n".repeat(16_384)
        );
        assert!(message.len() >= 256 * 1024);
        let mut file = input(&message).unwrap();
        let metadata = file.metadata().unwrap();
        assert!(metadata.is_file());
        assert_eq!(metadata.len(), message.len() as u64);
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
            assert_eq!(metadata.permissions().mode() & 0o077, 0);
            assert_eq!(metadata.nlink(), 0, "message must have no public pathname");
        }
        let mut actual = Vec::new();
        file.read_to_end(&mut actual).unwrap();
        assert_eq!(actual, message.as_bytes());
    }

    #[tokio::test]
    async fn known_launch_failure_returns_without_a_message_path_or_publication() {
        let root = tempfile::tempdir().unwrap();
        let mut command = tokio::process::Command::new(root.path().join("absent-svn"));
        command.stdin(std::process::Stdio::from(
            input(&"界😀\n".repeat(65_536)).unwrap(),
        ));
        let error = tokio::time::timeout(std::time::Duration::from_secs(5), command.output())
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }
}
