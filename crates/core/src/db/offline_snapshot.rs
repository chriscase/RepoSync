//! Read-only inspection of a quiesced, independently consistent SQLite snapshot.
use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, Read, Seek};
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

use crate::errors::DatabaseError;

#[derive(PartialEq, Eq)]
struct FileStamp {
    device: u64,
    inode: u64,
    length: u64,
    change_time: (i64, i64),
    modification_time: (i64, i64),
}

impl From<fs::Metadata> for FileStamp {
    fn from(m: fs::Metadata) -> Self {
        Self {
            device: m.dev(),
            inode: m.ino(),
            length: m.len(),
            change_time: (m.ctime(), m.ctime_nsec()),
            modification_time: (m.mtime(), m.mtime_nsec()),
        }
    }
}

fn stamp(path: &Path) -> Result<Option<FileStamp>, DatabaseError> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_file() && m.nlink() == 1 => Ok(Some(m.into())),
        Ok(_) => Err(DatabaseError::Other(
            "expected a regular snapshot file without aliases".into(),
        )),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

// SQLite's WAL-index format (https://sqlite.org/walformat.html#wal_index_format)
// has two 48-byte native-endian headers. A leftover index paired with an empty
// WAL is usable only if both valid headers agree that zero frames are present.
fn open_read(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
}

fn index_header(file: &File, length: u64) -> Result<Option<[u8; 48]>, DatabaseError> {
    if length == 0 || !length.is_multiple_of(32768) {
        return Ok(None);
    }
    let mut headers = [0; 96];
    file.read_exact_at(&mut headers, 0)?;
    let (first, second) = headers.split_at(48);
    let word = |i| u32::from_ne_bytes(headers[i..i + 4].try_into().expect("fixed WAL header"));
    let (mut a, mut b) = (0u32, 0u32);
    for i in (0..40).step_by(8) {
        a = a.wrapping_add(word(i)).wrapping_add(b);
        b = b.wrapping_add(word(i + 4)).wrapping_add(a);
    }
    Ok((first == second
        && word(0) == 3_007_000
        && headers[12] == 1
        && (word(40), word(44)) == (a, b))
        .then(|| first.try_into().expect("fixed WAL header")))
}

fn empty_wal_index(path: &Path, length: u64) -> Result<bool, DatabaseError> {
    if length == 0 || !length.is_multiple_of(32768) {
        return Ok(false);
    }
    Ok(index_header(&open_read(path)?, length)?.is_some_and(|header| header[16..20] == [0; 4]))
}

#[derive(PartialEq, Eq)]
struct Snapshot {
    database: FileStamp,
    wal: Option<FileStamp>,
    index: Option<FileStamp>,
}

impl Snapshot {
    fn inventory(path: &Path) -> Result<Self, DatabaseError> {
        let database = stamp(path)?
            .filter(|file| file.length >= 100)
            .ok_or_else(|| {
                DatabaseError::Other("missing or incomplete database snapshot".into())
            })?;
        if stamp(&super::startup_rollback::journal_path(path))?.is_some() {
            return Err(DatabaseError::Other(
                "snapshot has a rollback journal; supply a consistent offline SQLite backup".into(),
            ));
        }
        Ok(Self {
            database,
            wal: stamp(&sibling(path, "-wal"))?,
            index: stamp(&sibling(path, "-shm"))?,
        })
    }

    fn observe(path: &Path) -> Result<Self, DatabaseError> {
        let files = Self::inventory(path)?;
        if files.wal.as_ref().is_some_and(|file| file.length != 0) {
            return Err(DatabaseError::Other("snapshot has a nonempty write-ahead log; supply a consistent offline SQLite backup containing its committed data".into()));
        }
        if let Some(file) = &files.index {
            if files.wal.is_none() || !empty_wal_index(&sibling(path, "-shm"), file.length)? {
                return Err(DatabaseError::Other("snapshot has an incomplete or active SQLite WAL index; supply a consistent offline SQLite backup".into()));
            }
        }
        Ok(files)
    }
}

/// The caller supplies an offline snapshot. The metadata checks detect observed
/// changes during inspection; they cannot establish the provenance of a backup.
pub(super) fn with_snapshot<T>(
    path: &Path,
    inspect: impl FnOnce(&Connection) -> Result<T, DatabaseError>,
) -> Result<T, DatabaseError> {
    let before = Snapshot::observe(path)?;
    let canonical = fs::canonicalize(path)?;
    let mut uri = reqwest::Url::from_file_path(&canonical)
        .map_err(|()| DatabaseError::Other("invalid snapshot path".into()))?;
    uri.set_query(Some("mode=ro&immutable=1"));
    // Unlike an ordinary mode=ro connection to a WAL database, immutable=1
    // neither creates its sidecars nor requires the snapshot directory writable.
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
        | OpenFlags::SQLITE_OPEN_URI
        | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let conn = Connection::open_with_flags(uri.as_str(), flags)?;
    conn.execute_batch(
        "PRAGMA query_only=ON; PRAGMA trusted_schema=OFF; PRAGMA temp_store=MEMORY;",
    )?;
    let report = inspect(&conn)?;
    drop(conn);
    if Snapshot::observe(path)? != before || fs::canonicalize(path)? != canonical {
        return Err(DatabaseError::Other(
            "snapshot changed during inspection; supply a quiesced, consistent offline SQLite backup".into(),
        ));
    }
    Ok(report)
}

fn recovery_error() -> DatabaseError {
    DatabaseError::Other(
        "recovery requires a stopped installation with a complete and consistent SQLite file set"
            .into(),
    )
}

fn open_source(path: &Path, expected: &FileStamp) -> Result<File, DatabaseError> {
    let file = open_read(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 || FileStamp::from(metadata) != *expected {
        return Err(recovery_error());
    }
    Ok(file)
}

// The recovery CLI has no other SQLite connection on the source. A hypothetical
// whole-file write lock conflicts with any external SQLite lock, including SHM
// write/checkpoint/read slots. F_GETLK only queries the kernel.
fn require_no_sqlite_process(file: &File) -> Result<(), DatabaseError> {
    use rustix::process::{fcntl_getlk, FlockType};
    if fcntl_getlk(file, &FlockType::WriteLock.into())
        .map_err(io::Error::from)?
        .is_some()
    {
        return Err(recovery_error());
    }
    Ok(())
}

fn fingerprint(file: &mut File) -> Result<[u8; 32], DatabaseError> {
    use sha2::{Digest, Sha256};
    file.rewind()?;
    let mut hash = Sha256::new();
    io::copy(file, &mut hash)?;
    Ok(hash.finalize().into())
}

// Return a digest of the bytes actually copied, for comparison with a second
// read of the source after inspection. Keep SQLite away from every source fd.
fn copy_to_private(file: &mut File, path: &Path) -> Result<[u8; 32], DatabaseError> {
    let mut copy = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    io::copy(file, &mut copy)?;
    fingerprint(&mut copy)
}

// SQLite's WAL recovery validates the WAL header, frame salts and rolling
// checksums. It normally tolerates a torn/uncommitted tail by stopping early.
// Insist that its last recovered commit accounts for the entire physical WAL.
fn validate_rebuilt_index(
    private: &Path,
    wal_length: u64,
    source_index: Option<(&File, &[u8; 48])>,
) -> Result<(), DatabaseError> {
    let mut db_header = [0; 20];
    File::open(private)?.read_exact(&mut db_header)?;
    let encoded_page_size = u16::from_be_bytes([db_header[16], db_header[17]]);
    let page_size = if encoded_page_size == 1 {
        65_536
    } else {
        u64::from(encoded_page_size)
    };
    if db_header[..16] != *b"SQLite format 3\0"
        || db_header[18..20] != [2, 2]
        || !(512..=65_536).contains(&page_size)
        || !page_size.is_power_of_two()
        || !private.metadata()?.len().is_multiple_of(page_size)
    {
        return Err(recovery_error());
    }
    let rebuilt = File::open(sibling(private, "-shm"))?;
    let hdr = index_header(&rebuilt, rebuilt.metadata()?.len())?.ok_or_else(recovery_error)?;
    let frames = u64::from(u32::from_ne_bytes(hdr[16..20].try_into().unwrap()));
    let pages = u32::from_ne_bytes(hdr[20..24].try_into().unwrap());
    if frames == 0
        || pages == 0
        || (frames * (page_size + 24) + 32) != wal_length
        || u16::from_ne_bytes([hdr[14], hdr[15]]) != encoded_page_size
    {
        return Err(recovery_error());
    }
    if let Some((source, original)) = source_index {
        if original[13..40] != hdr[13..40] {
            return Err(recovery_error());
        }
        // A stopped source can retain different reader marks and backfill
        // progress. Its committed frame map and hash slots must still agree.
        for at in [96, 128] {
            let mut word = [0; 4];
            source.read_exact_at(&mut word, at)?;
            if u64::from(u32::from_ne_bytes(word)) > frames {
                return Err(recovery_error());
            }
        }
        let mut remaining = frames;
        let mut offset = 0;
        while remaining > 0 {
            let mut expected = [0; 32768];
            let mut actual = [0; 32768];
            rebuilt.read_exact_at(&mut expected, offset)?;
            source.read_exact_at(&mut actual, offset)?;
            let start = if offset == 0 { 136 } else { 0 };
            let used = remaining.min((16384 - start) / 4);
            let indices = start as usize..(start + used * 4) as usize;
            if actual[indices.clone()] != expected[indices] || actual[16384..] != expected[16384..]
            {
                return Err(recovery_error());
            }
            remaining -= used;
            offset += 32768;
        }
    }
    Ok(())
}

/// Read a stopped installation without allowing SQLite to open the original
/// writable or to create/rebuild its sidecars. An intact original file set is
/// a prerequisite: the SQLite WAL format carries no main-file identity.
pub(super) fn with_recovery_snapshot<T>(
    path: &Path,
    inspect: impl FnOnce(&Connection) -> Result<T, DatabaseError>,
) -> Result<T, DatabaseError> {
    let before = Snapshot::inventory(path)?;
    let canonical = fs::canonicalize(path)?;
    let lock_path = path.with_file_name("reposync.lock");
    let lock_stamp = stamp(&lock_path)?;
    let _daemon_lock = lock_stamp
        .as_ref()
        .map(|expected| {
            let file = open_source(&lock_path, expected)?;
            rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockShared)
                .map_err(|_| recovery_error())?;
            Ok::<_, DatabaseError>(file)
        })
        .transpose()?;
    let mut db = open_source(path, &before.database)?;
    require_no_sqlite_process(&db)?;
    let Some(wal_stamp) = before.wal.as_ref().filter(|wal| wal.length > 0) else {
        let report = with_snapshot(path, inspect)?;
        if Snapshot::inventory(path)? != before
            || fs::canonicalize(path)? != canonical
            || stamp(&lock_path)? != lock_stamp
        {
            return Err(recovery_error());
        }
        require_no_sqlite_process(&db)?;
        return Ok(report);
    };
    let mut wal = open_source(&sibling(path, "-wal"), wal_stamp)?;
    let mut index = before
        .index
        .as_ref()
        .map(|expected| -> Result<_, DatabaseError> {
            let mut file = open_source(&sibling(path, "-shm"), expected)?;
            require_no_sqlite_process(&file)?;
            let header = index_header(&file, expected.length)?.ok_or_else(recovery_error)?;
            let digest = fingerprint(&mut file)?;
            Ok((file, header, digest))
        })
        .transpose()?;
    let temporary = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()?;
    let private = temporary.path().join("recovery.db");
    let db_digest = copy_to_private(&mut db, &private)?;
    let wal_digest = copy_to_private(&mut wal, &sibling(&private, "-wal"))?;
    // Read-write permission is confined to the private copy and lets SQLite
    // rebuild the disposable index itself. query_only protects the callback.
    let conn = Connection::open_with_flags(
        &private,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    conn.set_db_config(
        rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
        true,
    )?;
    conn.execute_batch(
        "PRAGMA query_only=ON; PRAGMA trusted_schema=OFF; PRAGMA temp_store=MEMORY; BEGIN;",
    )?;
    let integrity: String = conn.query_row("PRAGMA integrity_check(1)", [], |r| r.get(0))?;
    if integrity != "ok" {
        return Err(recovery_error());
    }
    validate_rebuilt_index(
        &private,
        wal_stamp.length,
        index.as_ref().map(|i| (&i.0, &i.1)),
    )?;
    let report = inspect(&conn)?;
    require_no_sqlite_process(&db)?;
    if let Some((file, _, digest)) = &mut index {
        require_no_sqlite_process(file)?;
        if fingerprint(file)? != *digest {
            return Err(recovery_error());
        }
    }
    if fingerprint(&mut db)? != db_digest
        || fingerprint(&mut wal)? != wal_digest
        || Snapshot::inventory(path)? != before
        || fs::canonicalize(path)? != canonical
        || stamp(&lock_path)? != lock_stamp
    {
        return Err(recovery_error());
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create(path: &Path) -> Connection {
        let db = Connection::open(path).unwrap();
        db.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
             CREATE TABLE records(id INTEGER); INSERT INTO records VALUES(3);",
        )
        .unwrap();
        db
    }

    fn value(conn: &Connection) -> Result<i64, DatabaseError> {
        Ok(conn.query_row("SELECT id FROM records", [], |row| row.get(0))?)
    }

    fn files(path: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut entries: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| {
                let path = entry.unwrap().path();
                let bytes = fs::read(&path).unwrap();
                (path, bytes)
            })
            .collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        entries
    }

    #[test]
    fn immutable_open_handles_literal_names_and_leaves_no_sidecars() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snapshot %23?#.sqlite");
        assert!(with_snapshot(&path, value).is_err());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
        drop(create(&path));
        let before = files(&path);
        assert_eq!(before.len(), 1);
        assert_eq!(
            with_snapshot(&path, |conn| {
                assert!(conn.execute("UPDATE records SET id=4", []).is_err());
                value(conn)
            })
            .unwrap(),
            3
        );
        assert_eq!(files(&path), before);
    }

    #[test]
    fn refuses_wal_frames_accepts_checkpointed_source_and_refuses_partial_sidecars() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reposync.db");
        let db = create(&path);
        let before = files(&path);
        assert!(with_snapshot(&path, |_| -> Result<(), DatabaseError> {
            panic!("source with uncheckpointed changes reached inspection")
        })
        .is_err());
        assert_eq!(files(&path), before);

        db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
        let before = files(&path);
        assert_eq!(before.len(), 3);
        assert_eq!(with_snapshot(&path, value).unwrap(), 3);
        assert_eq!(files(&path), before);
        let index_path = sibling(&path, "-shm");
        let mut invalid_index = fs::read(&index_path).unwrap();
        drop(db);

        // A truncated WAL must not validate an index that still describes frames.
        invalid_index[16..20].copy_from_slice(&2u32.to_ne_bytes());
        fs::write(sibling(&path, "-wal"), []).unwrap();
        fs::write(&index_path, invalid_index).unwrap();
        let before = files(&path);
        assert!(with_snapshot(&path, value).is_err());
        assert_eq!(files(&path), before);
        fs::remove_file(index_path).unwrap();
        fs::remove_file(sibling(&path, "-wal")).unwrap();

        // Use an independent stopped fixture for each sidecar refusal.
        for (suffix, bytes) in [("-shm", b"partial".as_slice()), ("-journal", b"")] {
            let sidecar = sibling(&path, suffix);
            fs::write(&sidecar, bytes).unwrap();
            let before = files(&path);
            assert!(with_snapshot(&path, value).is_err());
            assert_eq!(files(&path), before);
            fs::remove_file(sidecar).unwrap();
        }
    }

    #[test]
    fn refuses_a_source_replaced_while_inspected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reposync.db");
        drop(create(&path));
        let replacement = dir.path().join("replacement");
        fs::copy(&path, &replacement).unwrap();
        let result = with_snapshot(&path, |conn| {
            let result = value(conn)?;
            fs::rename(&replacement, &path)?;
            Ok(result)
        });
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("changed during inspection"));
    }

    fn stopped_with_wal(path: &Path) {
        let db = create(path);
        db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); UPDATE records SET id=19;")
            .unwrap();
        db.set_db_config(
            rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
            true,
        )
        .unwrap();
        drop(db);
        assert!(sibling(path, "-wal").metadata().unwrap().len() > 32);
    }

    #[test]
    fn stopped_recovery_reads_the_committed_wal_without_touching_source() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("site ?#%.db");
        stopped_with_wal(&path);
        let standalone = dir.path().join("main-only");
        fs::copy(&path, &standalone).unwrap();
        assert_eq!(with_snapshot(&standalone, value).unwrap(), 3);
        let before = files(&path);
        assert_eq!(
            with_recovery_snapshot(&path, |conn| {
                assert!(conn.execute("INSERT INTO records VALUES(9)", []).is_err());
                value(conn)
            })
            .unwrap(),
            19
        );
        assert_eq!(files(&path), before);
        fs::remove_file(sibling(&path, "-shm")).unwrap();
        let before = files(&path);
        assert_eq!(with_recovery_snapshot(&path, value).unwrap(), 19);
        assert_eq!(files(&path), before);
    }

    #[test]
    fn recovery_refuses_partial_corrupt_mismatched_or_changing_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reposync.db");
        stopped_with_wal(&path);
        let wal = sibling(&path, "-wal");
        let original = fs::read(&wal).unwrap();
        for pos in [4, 16, 32 + 8, 32 + 16, 32 + 24] {
            let mut damaged = original.clone();
            damaged[pos] ^= 0x01;
            fs::write(&wal, damaged).unwrap();
            let before = files(&path);
            assert!(with_recovery_snapshot(&path, value).is_err());
            assert_eq!(files(&path), before);
        }
        fs::write(&wal, &original[..original.len() - 1]).unwrap();
        assert!(with_recovery_snapshot(&path, value).is_err());
        fs::write(&wal, &original).unwrap();
        let other = dir.path().join("other.db");
        stopped_with_wal(&other);
        let index = sibling(&path, "-shm");
        let retained = fs::read(&index).unwrap();
        fs::copy(sibling(&other, "-shm"), &index).unwrap();
        assert!(with_recovery_snapshot(&path, value).is_err());
        for offset in [99, 136, 16384] {
            let mut broken = retained.clone();
            broken[offset] ^= 0xff;
            fs::write(&index, broken).unwrap();
            assert!(with_recovery_snapshot(&path, value).is_err());
        }
        fs::write(&index, retained).unwrap();
        fs::write(sibling(&path, "-journal"), b"").unwrap();
        assert!(with_recovery_snapshot(&path, value).is_err());
        fs::remove_file(sibling(&path, "-journal")).unwrap();
        assert!(with_recovery_snapshot(&path, |c| {
            fs::write(&wal, &original)?;
            value(c)
        })
        .is_err());
    }

    #[test]
    fn recovery_refuses_an_owned_daemon_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reposync.db");
        stopped_with_wal(&path);
        let lock = File::create(dir.path().join("reposync.lock")).unwrap();
        rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive).unwrap();
        assert!(with_recovery_snapshot(&path, value).is_err());
    }

    #[test]
    fn recovery_refuses_a_live_external_sqlite_writer() {
        use std::io::BufRead;
        use std::process::{Command, Stdio};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reposync.db");
        stopped_with_wal(&path);
        let script = "import sqlite3,sys\nc=sqlite3.connect(sys.argv[1]); c.execute('BEGIN IMMEDIATE')\nprint('ready',flush=True); sys.stdin.read(); c.rollback(); c.close()";
        let mut writer = Command::new("python3")
            .args(["-c", script])
            .arg(&path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut ready = String::new();
        io::BufReader::new(writer.stdout.take().unwrap())
            .read_line(&mut ready)
            .unwrap();
        let refused = with_recovery_snapshot(&path, value).is_err();
        drop(writer.stdin.take());
        assert!(writer.wait().unwrap().success());
        assert_eq!(ready.trim(), "ready");
        assert!(refused);
    }

    #[test]
    fn recovery_refuses_valid_frames_after_the_last_commit() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("original.db");
        stopped_with_wal(&source);
        let wal = sibling(&source, "-wal");
        let committed = wal.metadata().unwrap().len();
        let pending = Connection::open(&source).unwrap();
        pending.execute_batch("PRAGMA cache_size=2; BEGIN IMMEDIATE; CREATE TABLE pending(payload BLOB); INSERT INTO pending VALUES(zeroblob(400000));").unwrap();
        assert!(wal.metadata().unwrap().len() > committed);
        let copy = dir.path().join("cutoff.db");
        fs::copy(&source, &copy).unwrap();
        fs::copy(wal, sibling(&copy, "-wal")).unwrap();
        assert!(with_recovery_snapshot(&copy, value).is_err());
    }
}
