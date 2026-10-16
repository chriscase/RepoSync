//! Attestation for owned refresh resources, not a general SQLite open policy.
use std::ffi::{c_int, c_void, CStr};
use std::os::fd::BorrowedFd;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use rusqlite::{ffi, Connection};

/// A path identity is accepted only when the connected main-file descriptor
/// has that device and inode. Unsupported native implementations stay unknown.
#[derive(PartialEq, Eq)]
pub(crate) struct OpenedDatabaseFile {
    pub path: PathBuf,
    pub device: u64,
    pub inode: u64,
    pub aliased: bool,
}

impl OpenedDatabaseFile {
    pub(crate) fn require_unaliased_owner(
        &self,
        conn: &Connection,
    ) -> Result<(), crate::errors::DatabaseError> {
        let refused = || {
            crate::errors::DatabaseError::WriterFenced(
                "ordinary database requires its owned singly linked native file".into(),
            )
        };
        let named = std::fs::symlink_metadata(&self.path).map_err(|_| refused())?;
        let held = connected_main_file_stat(conn).ok_or_else(refused)?;
        if !named.is_file()
            || named.nlink() != 1
            || named.uid() != rustix::process::geteuid().as_raw()
            || native_number(held.st_nlink) != Some(1)
            || held.st_uid != named.uid()
            || named.dev() != self.device
            || named.ino() != self.inode
            || native_number(held.st_dev) != Some(self.device)
            || native_number(held.st_ino) != Some(self.inode)
        {
            return Err(refused());
        }
        Ok(())
    }

    pub(crate) fn capture(path: &Path) -> Option<Self> {
        let metadata = std::fs::symlink_metadata(path).ok()?;
        let parent = path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let parent_metadata = std::fs::symlink_metadata(parent).ok()?;
        let canonical = std::fs::canonicalize(path).ok()?;
        let target = std::fs::metadata(&canonical).ok()?;
        target.is_file().then_some(Self {
            path: canonical,
            device: target.dev(),
            inode: target.ino(),
            aliased: metadata.file_type().is_symlink() || parent_metadata.file_type().is_symlink(),
        })
    }

    pub(crate) fn attest(conn: &Connection, path: &Path, prior: Option<Self>) -> Option<Self> {
        let mut after = Self::capture(path)?;
        if after.aliased {
            return None;
        }
        if prior.is_some_and(|before| before != after) {
            return None;
        }
        let held = connected_main_file_stat(conn)?;
        if native_number(held.st_dev)? != after.device || native_number(held.st_ino)? != after.inode
        {
            return None;
        }
        // Detect a path change during the native check too. An initial missing
        // file needs no path-only fallback: its newly opened fd is attested.
        if Self::capture(path)? != after {
            return None;
        }
        after.device = native_number(held.st_dev)?;
        after.inode = native_number(held.st_ino)?;
        Some(after)
    }
}

fn native_number(value: impl TryInto<u64>) -> Option<u64> {
    value.try_into().ok()
}

// libsqlite3-sys 0.28.0's bundled sqlite3.c, SQLite 3.45.0, unixFile's
// unconditional prefix. This is deliberately NOT treated as a public ABI.
// Runtime version/source/VFS checks below precede access. A SQLite upgrade
// requires auditing this adapter; otherwise resource admission is unavailable.
#[repr(C)]
struct BundledUnixFilePrefix {
    methods: *const ffi::sqlite3_io_methods,
    vfs: *mut ffi::sqlite3_vfs,
    inode_info: *mut c_void,
    descriptor: c_int,
}

fn connected_main_file_stat(conn: &Connection) -> Option<rustix::fs::Stat> {
    const SOURCE_ID: &[u8] =
        b"2024-01-15 17:01:13 1066602b2b1976fe58b5150777cced894af17c803e068f5918390d6915b46e1d";
    // SAFETY: the connection is live and exclusively borrowed here. Public
    // file-control APIs return connection-owned pointers. Only the exact pinned
    // bundled Unix implementation is inspected; the fd stays borrowed and is
    // never duplicated, closed, or otherwise mutated by this adapter.
    unsafe {
        if std::mem::size_of::<usize>() != 8
            || ffi::sqlite3_libversion_number() != 3_045_000
            || CStr::from_ptr(ffi::sqlite3_sourceid()).to_bytes() != SOURCE_ID
        {
            return None;
        }
        let mut vfs: *mut ffi::sqlite3_vfs = std::ptr::null_mut();
        if ffi::sqlite3_file_control(
            conn.handle(),
            c"main".as_ptr(),
            ffi::SQLITE_FCNTL_VFS_POINTER,
            (&mut vfs as *mut *mut ffi::sqlite3_vfs).cast(),
        ) != ffi::SQLITE_OK
            || vfs.is_null()
            || (*vfs).zName.is_null()
        {
            return None;
        }
        let name = CStr::from_ptr((*vfs).zName).to_bytes();
        // Both built-in VFS entries use unixOpen and allocate unixFile. A
        // wrapper with a familiar name but a different open/allocation method
        // is not safe to interpret. In-process VFS registration is trusted;
        // runtime replacement of the built-in adapters is unsupported.
        let unix = ffi::sqlite3_vfs_find(c"unix-excl".as_ptr());
        if !matches!(name, b"unix" | b"unix-posix" | b"unix-excl")
            || ffi::sqlite3_vfs_find((*vfs).zName) != vfs
            || unix.is_null()
            || (*vfs).szOsFile != (*unix).szOsFile
            || !std::ptr::fn_addr_eq((*vfs).xOpen?, (*unix).xOpen?)
            || (*vfs).szOsFile < std::mem::size_of::<BundledUnixFilePrefix>() as c_int
        {
            return None;
        }
        let mut file: *mut ffi::sqlite3_file = std::ptr::null_mut();
        if ffi::sqlite3_file_control(
            conn.handle(),
            c"main".as_ptr(),
            ffi::SQLITE_FCNTL_FILE_POINTER,
            (&mut file as *mut *mut ffi::sqlite3_file).cast(),
        ) != ffi::SQLITE_OK
            || file.is_null()
        {
            return None;
        }
        let methods = (*file).pMethods;
        if methods.is_null() || !(1..=3).contains(&(*methods).iVersion) {
            return None;
        }
        let native = std::ptr::read(file.cast::<BundledUnixFilePrefix>());
        if native.vfs != vfs || native.methods != methods || native.descriptor < 0 {
            return None;
        }
        let held = rustix::fs::fstat(BorrowedFd::borrow_raw(native.descriptor)).ok()?;
        (rustix::fs::FileType::from_raw_mode(held.st_mode) == rustix::fs::FileType::RegularFile)
            .then_some(held)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newly_created_database_is_attested_against_its_connected_descriptor() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("fresh.db");
        let before = OpenedDatabaseFile::capture(&path);
        assert!(before.is_none());
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE original(value TEXT); INSERT INTO original VALUES('original');",
        )
        .unwrap();
        let identity = OpenedDatabaseFile::attest(&conn, &path, before).unwrap();
        let metadata = std::fs::metadata(&path).unwrap();
        assert_eq!(identity.device, metadata.dev());
        assert_eq!(identity.inode, metadata.ino());
        assert_eq!(identity.path, path.canonicalize().unwrap());
    }

    #[test]
    fn first_open_replacement_cannot_become_the_connected_database_identity() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("fresh.db");
        let before = OpenedDatabaseFile::capture(&path);
        assert!(before.is_none());
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE original(value TEXT); INSERT INTO original VALUES('original');",
        )
        .unwrap();
        let old = directory.path().join("old.db");
        std::fs::rename(&path, &old).unwrap();
        std::fs::copy(&old, &path).unwrap();
        assert_ne!(
            std::fs::metadata(&old).unwrap().ino(),
            std::fs::metadata(&path).unwrap().ino()
        );
        assert!(OpenedDatabaseFile::attest(&conn, &path, before).is_none());
        let original: String = conn
            .query_row("SELECT value FROM original", [], |row| row.get(0))
            .unwrap();
        assert_eq!(original, "original");
        assert!(old.is_file());
        assert!(path.is_file());
        let replacement = Connection::open(&path).unwrap();
        assert!(OpenedDatabaseFile::attest(
            &replacement,
            &path,
            OpenedDatabaseFile::capture(&path)
        )
        .is_some());
    }

    #[test]
    fn memory_and_unsupported_native_vfs_remain_unattested() {
        assert!(connected_main_file_stat(&Connection::open_in_memory().unwrap()).is_none());
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("dotfile.db");
        let conn = Connection::open_with_flags_and_vfs(
            &path,
            rusqlite::OpenFlags::default(),
            "unix-dotfile",
        )
        .unwrap();
        assert!(OpenedDatabaseFile::attest(&conn, &path, None).is_none());
        assert!(path.is_file());
    }
}
