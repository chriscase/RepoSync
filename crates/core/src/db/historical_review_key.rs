//! A private, operator-held key for reviewing a stopped Team installation.
//! Use a separate owner-only directory outside the installation and its backups.

use std::{
    ffi::OsStr,
    fs::{self, File, Metadata, OpenOptions},
    io::{Read, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt},
    },
    path::Path,
};

use rand::{rngs::OsRng, RngCore};
use rustix::fs::{openat, AtFlags, Mode, OFlags};

use crate::errors::DatabaseError;

const HEADER: &[u8] = b"RepoSync historical review key v1\n";
const KEY_SIZE: usize = 32;

pub struct ReviewSessionKey([u8; KEY_SIZE]);

impl ReviewSessionKey {
    pub(crate) fn bytes(&self) -> &[u8; KEY_SIZE] {
        &self.0
    }
}

impl Drop for ReviewSessionKey {
    fn drop(&mut self) {
        for byte in &mut self.0 {
            // Keep the wipe of this owned allocation observable to the compiler.
            unsafe { std::ptr::write_volatile(byte, 0) };
        }
    }
}

fn refused() -> DatabaseError {
    DatabaseError::Other("review key unavailable: use a valid private key in a directory owned and accessible only by the current account".into())
}

struct Destination<'a> {
    directory: File,
    parent: &'a Path,
    name: &'a OsStr,
}

impl<'a> Destination<'a> {
    fn new(path: &'a Path) -> Result<Self, DatabaseError> {
        let name = path.file_name().ok_or_else(refused)?;
        // Path::file_name normalizes a trailing slash or "/.". Publishing
        // through dirfd must not silently turn a directory operand into a file.
        if path.as_os_str().as_bytes().rsplit(|&b| b == b'/').next() != Some(name.as_bytes()) {
            return Err(refused());
        }
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(parent)
            .map_err(|_| refused())?;
        let dest = Self {
            directory,
            parent,
            name,
        };
        dest.check_directory()?;
        Ok(dest)
    }

    fn check_directory(&self) -> Result<(), DatabaseError> {
        let fd = self.directory.metadata().map_err(|_| refused())?;
        let path = fs::symlink_metadata(self.parent).map_err(|_| refused())?;
        if !fd.is_dir()
            || !path.is_dir()
            || (fd.dev(), fd.ino()) != (path.dev(), path.ino())
            || fd.uid() != rustix::process::geteuid().as_raw()
            || fd.mode() & 0o077 != 0
        {
            return Err(refused());
        }
        Ok(())
    }

    fn read_file(&self, name: &OsStr) -> Result<File, DatabaseError> {
        openat(
            &self.directory,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(|_| refused())
    }
}

// Reading can change atime. Check all metadata relevant to identity, contents
// and access on both the open descriptor and the current named entry.
#[derive(PartialEq, Eq)]
struct KeyStamp {
    identity: (u64, u64),
    mode: u32,
    changed: (i64, i64),
    modified: (i64, i64),
}

fn stamp(md: Metadata) -> Result<KeyStamp, DatabaseError> {
    if !md.is_file()
        || md.nlink() != 1
        || md.uid() != rustix::process::geteuid().as_raw()
        || !matches!(md.mode() & 0o7777, 0o400 | 0o600)
        || md.len() != (HEADER.len() + KEY_SIZE) as u64
    {
        return Err(refused());
    }
    Ok(KeyStamp {
        identity: (md.dev(), md.ino()),
        mode: md.mode(),
        changed: (md.ctime(), md.ctime_nsec()),
        modified: (md.mtime(), md.mtime_nsec()),
    })
}

/// Load exactly one private review key. The directory and file operands must
/// themselves be real entries. Ancestors may include normal system symlinks.
pub fn open(path: &Path) -> Result<ReviewSessionKey, DatabaseError> {
    let dest = Destination::new(path)?;
    let mut file = dest.read_file(dest.name)?;
    let before = stamp(file.metadata().map_err(|_| refused())?)?;
    let mut header = [0; HEADER.len()];
    let mut key = ReviewSessionKey([0; KEY_SIZE]);
    file.read_exact(&mut header).map_err(|_| refused())?;
    if header != HEADER {
        return Err(refused());
    }
    file.read_exact(&mut key.0).map_err(|_| refused())?;
    dest.check_directory()?;
    if stamp(file.metadata().map_err(|_| refused())?)? != before
        || stamp(fs::symlink_metadata(path).map_err(|_| refused())?)? != before
        || key.0 == [0; KEY_SIZE]
    {
        return Err(refused());
    }
    Ok(key)
}

/// Create a complete fresh key with atomic no-replace publication (supported on Linux/macOS).
pub fn create(path: &Path) -> Result<(), DatabaseError> {
    let dest = Destination::new(path)?;
    if !matches!(
        rustix::fs::statat(&dest.directory, dest.name, AtFlags::SYMLINK_NOFOLLOW),
        Err(rustix::io::Errno::NOENT)
    ) {
        return Err(refused());
    }
    let mut key = ReviewSessionKey([0; KEY_SIZE]);
    OsRng.try_fill_bytes(&mut key.0).map_err(|_| refused())?;
    if key.0 == [0; KEY_SIZE] {
        return Err(refused());
    }
    // The unpredictable staging name is independent of the secret.
    let mut nonce = [0; 16];
    OsRng.try_fill_bytes(&mut nonce).map_err(|_| refused())?;
    let stage = format!(".reposync-review-{}.tmp", hex::encode(nonce));
    if dest.name == OsStr::new(&stage) {
        return Err(refused());
    }
    let mut file = File::from(
        openat(
            &dest.directory,
            stage.as_str(),
            OFlags::CREATE | OFlags::EXCL | OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(|_| refused())?,
    );
    let mut published = false;
    let result = (|| {
        rustix::fs::fchmod(&file, Mode::RUSR | Mode::WUSR).map_err(|_| refused())?;
        file.write_all(HEADER)
            .and_then(|()| file.write_all(key.bytes()))
            .and_then(|()| file.sync_all())
            .map_err(|_| refused())?;
        let staged = stamp(file.metadata().map_err(|_| refused())?)?;
        if staged
            != stamp(
                dest.read_file(OsStr::new(&stage))?
                    .metadata()
                    .map_err(|_| refused())?,
            )?
        {
            return Err(refused());
        }
        dest.check_directory()?;
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        rustix::fs::renameat_with(
            &dest.directory,
            stage.as_str(),
            &dest.directory,
            dest.name,
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .map_err(|_| refused())?;
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        return Err(refused());
        published = true;
        dest.directory.sync_all().map_err(|_| refused())?;
        let current = stamp(
            dest.read_file(dest.name)?
                .metadata()
                .map_err(|_| refused())?,
        )?;
        if current.identity != staged.identity || open(path)?.bytes() != key.bytes() {
            return Err(refused());
        }
        Ok(())
    })();
    if result.is_err() {
        if published {
            return Err(DatabaseError::Other(
                "a complete review key was published but final verification failed; check the original private directory and verify the existing key before retrying".into(),
            ));
        }
        // Cooperating creators use distinct stages in this operator-owned
        // directory. Clean up only while our stage still names our inode.
        if let (Ok(fd), Ok(named)) = (
            rustix::fs::fstat(&file),
            rustix::fs::statat(&dest.directory, stage.as_str(), AtFlags::SYMLINK_NOFOLLOW),
        ) {
            if (fd.st_ino, fd.st_dev) == (named.st_ino, named.st_dev) {
                let _ = rustix::fs::unlinkat(&dest.directory, stage.as_str(), AtFlags::empty());
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};

    fn private_dir() -> tempfile::TempDir {
        tempfile::Builder::new()
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap()
    }

    #[test]
    fn keys_are_independent_and_no_existing_output_can_be_replaced() {
        let dir = private_dir();
        let first = dir.path().join("review");
        let second = dir.path().join("other");
        create(&first).unwrap();
        let prior = fs::read(&first).unwrap();
        assert!(create(&first).is_err());
        assert_eq!(fs::read(&first).unwrap(), prior);
        assert_eq!(fs::metadata(&first).unwrap().mode() & 0o7777, 0o600);
        create(&second).unwrap();
        let a = open(&first).unwrap();
        assert!(a.bytes() == open(&first).unwrap().bytes());
        assert!(a.bytes() != open(&second).unwrap().bytes());
        fs::set_permissions(&first, fs::Permissions::from_mode(0o400)).unwrap();
        assert!(open(&first).unwrap().bytes() == a.bytes());
    }

    #[test]
    fn rejects_aliases_shared_permissions_and_unframed_or_incomplete_data() {
        let dir = private_dir();
        let path = dir.path().join("private-path-sentinel");
        let alias = dir.path().join("alias");
        create(&path).unwrap();
        let good = fs::read(&path).unwrap();
        symlink(&path, &alias).unwrap();
        assert!(open(&alias).is_err());
        assert!(create(&alias).is_err());
        assert!(fs::symlink_metadata(&alias)
            .unwrap()
            .file_type()
            .is_symlink());
        fs::remove_file(&alias).unwrap();
        fs::hard_link(&path, &alias).unwrap();
        assert!(open(&path).is_err() && open(&alias).is_err());
        fs::remove_file(&alias).unwrap();
        for mode in [0o644, 0o640, 0o610, 0o700, 0o4600] {
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
            assert!(open(&path).is_err());
        }
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let private_parent = dir.path().join("private-folder");
        fs::create_dir(&private_parent).unwrap();
        fs::set_permissions(&private_parent, fs::Permissions::from_mode(0o700)).unwrap();
        symlink(&private_parent, &alias).unwrap();
        assert!(create(&alias.join("key")).is_err());
        fs::set_permissions(&private_parent, fs::Permissions::from_mode(0o750)).unwrap();
        assert!(create(&private_parent.join("key")).is_err());
        assert_eq!(private_parent.read_dir().unwrap().count(), 0);
        let mut bad_header = good.clone();
        bad_header[0] ^= 1;
        for bad in [
            vec![1; 32],
            good[..good.len() - 1].to_vec(),
            [good.clone(), vec![0]].concat(),
            bad_header,
            [HEADER, &[0; 32]].concat(),
        ] {
            fs::write(&path, bad).unwrap();
            let err = open(&path).err().unwrap().to_string();
            assert!(!err.contains("private-path-sentinel"));
        }
    }

    #[test]
    fn concurrent_creators_leave_one_complete_private_key() {
        let dir = private_dir();
        let file = dir.path().join("key");
        std::thread::scope(|scope| {
            let one = scope.spawn(|| create(&file));
            let two = scope.spawn(|| create(&file));
            assert_ne!(one.join().unwrap().is_ok(), two.join().unwrap().is_ok());
        });
        assert!(open(&file).is_ok());
        assert_eq!(dir.path().read_dir().unwrap().count(), 1);
    }

    #[test]
    fn rejects_a_directory_spelling_before_creating_any_output() {
        let dir = private_dir();
        for spelling in ["new-key/", "new-key/."] {
            assert!(create(&dir.path().join(spelling)).is_err());
        }
        assert_eq!(dir.path().read_dir().unwrap().count(), 0);
    }
}
