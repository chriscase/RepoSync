//! No-follow, descriptor-relative mutation of regular working-copy files.
//!
//! This deliberately refuses unsupported nodes rather than unlinking a planted
//! link and declaring it a valid SVN file. Callers preflight the whole write set
//! before changing its first member; writes recheck through held directories.

use std::io::{self, Write};
use std::path::Path;

#[cfg(unix)]
pub struct ConfinedWorkingCopy {
    root: std::os::fd::OwnedFd,
}

#[cfg(unix)]
impl ConfinedWorkingCopy {
    pub fn open(root: &Path) -> io::Result<Self> {
        Ok(Self {
            root: rustix::fs::open(root, Self::directory_flags(), rustix::fs::Mode::empty())?,
        })
    }

    fn directory_flags() -> rustix::fs::OFlags {
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC
    }

    fn components(path: &str) -> io::Result<Vec<&str>> {
        super::operand::validate_relative_path(path)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
        Ok(path.split('/').collect())
    }

    fn parent(
        &self,
        path: &str,
        create: bool,
    ) -> io::Result<Option<(std::os::fd::OwnedFd, String)>> {
        let parts = Self::components(path)?;
        let mut dir = rustix::fs::openat(
            &self.root,
            ".",
            Self::directory_flags(),
            rustix::fs::Mode::empty(),
        )?;
        for part in &parts[..parts.len() - 1] {
            match rustix::fs::openat(
                &dir,
                *part,
                Self::directory_flags(),
                rustix::fs::Mode::empty(),
            ) {
                Ok(next) => dir = next,
                Err(rustix::io::Errno::NOENT) if !create => return Ok(None),
                Err(rustix::io::Errno::NOENT) => {
                    match rustix::fs::mkdirat(
                        &dir,
                        *part,
                        rustix::fs::Mode::from_bits_truncate(0o755),
                    ) {
                        Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                        Err(e) => return Err(e.into()),
                    }
                    dir = rustix::fs::openat(
                        &dir,
                        *part,
                        Self::directory_flags(),
                        rustix::fs::Mode::empty(),
                    )?;
                }
                Err(e) => return Err(e.into()),
            }
        }
        Ok(Some((dir, parts.last().unwrap().to_string())))
    }

    /// Return whether the exact regular file exists; directories, links,
    /// devices and multiply-linked files are unsupported write targets.
    pub fn inspect_file(&self, path: &str) -> io::Result<bool> {
        self.inspect_operand(path, false)
    }

    pub fn inspect_operand(&self, path: &str, allow_directory: bool) -> io::Result<bool> {
        let Some((parent, name)) = self.parent(path, false)? else {
            return Ok(false);
        };
        match rustix::fs::statat(
            &parent,
            name.as_str(),
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        ) {
            Ok(stat)
                if rustix::fs::FileType::from_raw_mode(stat.st_mode)
                    == rustix::fs::FileType::RegularFile
                    && stat.st_nlink == 1 =>
            {
                Ok(true)
            }
            Ok(stat)
                if allow_directory
                    && rustix::fs::FileType::from_raw_mode(stat.st_mode)
                        == rustix::fs::FileType::Directory =>
            {
                Ok(true)
            }
            Ok(_) => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unsupported SVN destination node: {path:?}"),
            )),
            Err(rustix::io::Errno::NOENT) => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    pub fn preflight<'a>(&self, paths: impl IntoIterator<Item = &'a str>) -> io::Result<()> {
        for path in paths {
            self.inspect_file(path)?;
        }
        Ok(())
    }

    pub fn inspect_directory(&self, path: &str) -> io::Result<()> {
        let Some((parent, name)) = self.parent(path, false)? else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "missing working-copy directory",
            ));
        };
        let stat = rustix::fs::statat(
            &parent,
            name.as_str(),
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        )?;
        if rustix::fs::FileType::from_raw_mode(stat.st_mode) != rustix::fs::FileType::Directory {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unsupported working-copy directory",
            ));
        }
        Ok(())
    }

    pub fn missing_parent_directories(&self, path: &str) -> io::Result<Vec<String>> {
        let parts = Self::components(path)?;
        let mut dir = rustix::fs::openat(
            &self.root,
            ".",
            Self::directory_flags(),
            rustix::fs::Mode::empty(),
        )?;
        let mut missing = false;
        let mut result = Vec::new();
        for (at, part) in parts[..parts.len() - 1].iter().enumerate() {
            if !missing {
                match rustix::fs::openat(
                    &dir,
                    *part,
                    Self::directory_flags(),
                    rustix::fs::Mode::empty(),
                ) {
                    Ok(next) => dir = next,
                    Err(rustix::io::Errno::NOENT) => missing = true,
                    Err(e) => return Err(e.into()),
                }
            }
            if missing {
                result.push(parts[..=at].join("/"));
            }
        }
        Ok(result)
    }

    /// Publish through a fresh regular inode in the verified parent. Replacing
    /// a raced-in link cannot write its target, and hardlinks are never edited.
    pub fn write_file(&self, path: &str, bytes: &[u8]) -> io::Result<()> {
        self.inspect_file(path)?;
        let (parent, name) = self
            .parent(path, true)?
            .ok_or_else(|| io::Error::other("missing confined parent"))?;
        let mode = match rustix::fs::statat(
            &parent,
            name.as_str(),
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        ) {
            Ok(st)
                if rustix::fs::FileType::from_raw_mode(st.st_mode)
                    == rustix::fs::FileType::RegularFile
                    && st.st_nlink == 1 =>
            {
                Some(st.st_mode & 0o777)
            }
            Err(rustix::io::Errno::NOENT) => None,
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "destination changed to an unsupported node",
                ))
            }
            Err(e) => return Err(e.into()),
        };
        let temp = format!(".reposync-write-{}", uuid::Uuid::new_v4());
        let fd = rustix::fs::openat(
            &parent,
            temp.as_str(),
            rustix::fs::OFlags::WRONLY
                | rustix::fs::OFlags::CREATE
                | rustix::fs::OFlags::EXCL
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::from_bits_truncate(mode.unwrap_or(0o666)),
        )?;
        let result = (|| {
            if let Some(mode) = mode {
                rustix::fs::fchmod(&fd, rustix::fs::Mode::from_bits_truncate(mode))?;
            }
            let mut file = std::fs::File::from(fd);
            file.write_all(bytes)?;
            file.flush()?;
            rustix::fs::renameat(&parent, temp.as_str(), &parent, name.as_str())?;
            Ok(())
        })();
        if result.is_err() {
            let _ = rustix::fs::unlinkat(&parent, temp.as_str(), rustix::fs::AtFlags::empty());
        }
        result
    }

    pub fn remove_file(&self, path: &str) -> io::Result<()> {
        if !self.inspect_file(path)? {
            return Ok(());
        }
        let Some((parent, name)) = self.parent(path, false)? else {
            return Ok(());
        };
        rustix::fs::unlinkat(&parent, name.as_str(), rustix::fs::AtFlags::empty())?;
        Ok(())
    }

    pub fn remove_empty_directory(&self, path: &str) -> io::Result<()> {
        let Some((parent, name)) = self.parent(path, false)? else {
            return Ok(());
        };
        match rustix::fs::statat(
            &parent,
            name.as_str(),
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        ) {
            Ok(st)
                if rustix::fs::FileType::from_raw_mode(st.st_mode)
                    == rustix::fs::FileType::Directory =>
            {
                rustix::fs::unlinkat(&parent, name.as_str(), rustix::fs::AtFlags::REMOVEDIR)?;
                Ok(())
            }
            Err(rustix::io::Errno::NOENT) => Ok(()),
            Ok(_) => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cleanup directory changed to an unsupported node",
            )),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(not(unix))]
pub struct ConfinedWorkingCopy {
    _private: (),
}

#[cfg(not(unix))]
impl ConfinedWorkingCopy {
    pub fn open(_: &Path) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no-follow working-copy writes require the supported Unix backend",
        ))
    }
    fn unsupported<T>() -> io::Result<T> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no-follow working-copy mutation requires Unix",
        ))
    }
    pub fn inspect_file(&self, _: &str) -> io::Result<bool> {
        Self::unsupported()
    }
    pub fn inspect_operand(&self, _: &str, _: bool) -> io::Result<bool> {
        Self::unsupported()
    }
    pub fn preflight<'a>(&self, _: impl IntoIterator<Item = &'a str>) -> io::Result<()> {
        Self::unsupported()
    }
    pub fn inspect_directory(&self, _: &str) -> io::Result<()> {
        Self::unsupported()
    }
    pub fn missing_parent_directories(&self, _: &str) -> io::Result<Vec<String>> {
        Self::unsupported()
    }
    pub fn write_file(&self, _: &str, _: &[u8]) -> io::Result<()> {
        Self::unsupported()
    }
    pub fn remove_file(&self, _: &str) -> io::Result<()> {
        Self::unsupported()
    }
    pub fn remove_empty_directory(&self, _: &str) -> io::Result<()> {
        Self::unsupported()
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn unsupported_node_anywhere_in_write_set_refuses_before_first_write() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("canary"), b"unchanged").unwrap();
        std::fs::write(root.path().join("ordinary"), b"before").unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("escape")).unwrap();
        let wc = ConfinedWorkingCopy::open(root.path()).unwrap();
        assert!(wc.preflight(["ordinary", "escape/canary"]).is_err());
        assert_eq!(
            std::fs::read(root.path().join("ordinary")).unwrap(),
            b"before"
        );
        assert!(wc.write_file("escape/canary", b"bad").is_err());
        assert_eq!(
            std::fs::read(outside.path().join("canary")).unwrap(),
            b"unchanged"
        );
        std::os::unix::fs::symlink(outside.path().join("canary"), root.path().join("leaf"))
            .unwrap();
        assert!(wc.write_file("leaf", b"bad").is_err());
        std::fs::hard_link(outside.path().join("canary"), root.path().join("hardlink")).unwrap();
        assert!(wc.write_file("hardlink", b"bad").is_err());
        assert_eq!(
            std::fs::read(outside.path().join("canary")).unwrap(),
            b"unchanged"
        );
        wc.write_file("nested/雪@-name", b"exact").unwrap();
        std::os::unix::fs::symlink("absent", root.path().join("dangling")).unwrap();
        std::os::unix::fs::symlink("cycle", root.path().join("cycle")).unwrap();
        assert!(wc.write_file("dangling", b"bad").is_err());
        assert!(wc.write_file("cycle/leaf", b"bad").is_err());
        use std::os::unix::ffi::OsStrExt;
        let fifo = std::ffi::CString::new(root.path().join("fifo").as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert!(wc.write_file("fifo", b"must not block on open").is_err());
        std::fs::create_dir(root.path().join("parent")).unwrap();
        std::fs::write(root.path().join("parent/canary"), b"inside").unwrap();
        wc.preflight(["parent/canary"]).unwrap();
        std::fs::rename(
            root.path().join("parent"),
            root.path().join("original-parent"),
        )
        .unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("parent")).unwrap();
        assert!(wc.write_file("parent/canary", b"bad").is_err());
        assert_eq!(
            std::fs::read(root.path().join("original-parent/canary")).unwrap(),
            b"inside"
        );
        assert_eq!(
            std::fs::read(outside.path().join("canary")).unwrap(),
            b"unchanged"
        );
        assert_eq!(
            std::fs::read(root.path().join("nested/雪@-name")).unwrap(),
            b"exact"
        );
        wc.remove_file("nested/雪@-name").unwrap();
        assert!(!root.path().join("nested/雪@-name").exists());
    }
}
