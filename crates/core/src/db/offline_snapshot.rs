//! Read-only inspection of a quiesced, independently consistent SQLite snapshot.
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
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

fn stamp(path: &Path) -> Result<Option<FileStamp>, DatabaseError> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_file() && m.nlink() == 1 => Ok(Some(FileStamp {
            device: m.dev(),
            inode: m.ino(),
            length: m.len(),
            change_time: (m.ctime(), m.ctime_nsec()),
            modification_time: (m.mtime(), m.mtime_nsec()),
        })),
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
fn empty_wal_index(path: &Path, length: u64) -> Result<bool, DatabaseError> {
    if length == 0 || !length.is_multiple_of(32768) {
        return Ok(false);
    }
    let mut headers = [0; 96];
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?
        .read_exact(&mut headers)?;
    let (first, second) = headers.split_at(48);
    let word = |i| u32::from_ne_bytes(headers[i..i + 4].try_into().expect("fixed WAL header"));
    let (mut a, mut b) = (0u32, 0u32);
    for i in (0..40).step_by(8) {
        a = a.wrapping_add(word(i)).wrapping_add(b);
        b = b.wrapping_add(word(i + 4)).wrapping_add(a);
    }
    Ok(first == second
        && word(0) == 3_007_000
        && headers[12] == 1
        && word(16) == 0
        && (word(40), word(44)) == (a, b))
}

#[derive(PartialEq, Eq)]
struct Snapshot {
    database: FileStamp,
    wal: Option<FileStamp>,
    index: Option<FileStamp>,
}

impl Snapshot {
    fn observe(path: &Path) -> Result<Self, DatabaseError> {
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
        let wal = stamp(&sibling(path, "-wal"))?;
        if wal.as_ref().is_some_and(|file| file.length != 0) {
            return Err(DatabaseError::Other("snapshot has a nonempty write-ahead log; supply a consistent offline SQLite backup containing its committed data".into()));
        }
        let index_path = sibling(path, "-shm");
        let index = stamp(&index_path)?;
        if let Some(file) = &index {
            if wal.is_none() || !empty_wal_index(&index_path, file.length)? {
                return Err(DatabaseError::Other("snapshot has an incomplete or active SQLite WAL index; supply a consistent offline SQLite backup".into()));
            }
        }
        Ok(Self {
            database,
            wal,
            index,
        })
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
}
