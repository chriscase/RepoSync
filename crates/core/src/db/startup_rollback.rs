//! Inspect a recovered private preimage before allowing native rollback recovery.
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use rusqlite::{config::DbConfig, Connection};
use sha2::{Digest, Sha256};

use crate::errors::DatabaseError;

const MAGIC: [u8; 8] = [0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7];

struct Input {
    file: File,
    path: PathBuf,
    device: u64,
    inode: u64,
    length: u64,
}

impl Input {
    fn open(path: &Path) -> Result<Self, DatabaseError> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        let metadata = file.metadata()?;
        let input = Self {
            file,
            path: path.to_owned(),
            device: metadata.dev(),
            inode: metadata.ino(),
            length: metadata.len(),
        };
        input.require_current()?;
        Ok(input)
    }

    fn require_current(&self) -> Result<(), DatabaseError> {
        let held = self.file.metadata()?;
        let named = std::fs::symlink_metadata(&self.path)?;
        if !held.is_file()
            || !named.is_file()
            || held.nlink() != 1
            || named.nlink() != 1
            || held.uid() != rustix::process::geteuid().as_raw()
            || named.uid() != held.uid()
            || held.dev() != self.device
            || held.ino() != self.inode
            || named.dev() != self.device
            || named.ino() != self.inode
            || held.len() != self.length
            || named.len() != self.length
        {
            return Err(DatabaseError::WriterFenced(
                "rollback input identity changed".into(),
            ));
        }
        Ok(())
    }

    fn copy(&mut self, destination: &Path) -> Result<[u8; 32], DatabaseError> {
        self.require_current()?;
        self.file.seek(SeekFrom::Start(0))?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(destination)?;
        let digest = transfer(&mut self.file, self.length, Some(&mut output))?;
        output.flush()?;
        self.require_current()?;
        Ok(digest)
    }

    fn verify_bytes(&mut self, expected: [u8; 32]) -> Result<(), DatabaseError> {
        self.require_current()?;
        self.file.seek(SeekFrom::Start(0))?;
        if transfer(&mut self.file, self.length, None)? != expected {
            return Err(DatabaseError::WriterFenced(
                "rollback input bytes changed".into(),
            ));
        }
        self.require_current()
    }
}

fn transfer(
    input: &mut File,
    mut remaining: u64,
    mut output: Option<&mut File>,
) -> Result<[u8; 32], DatabaseError> {
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 65536];
    while remaining != 0 {
        let limit = remaining.min(buffer.len() as u64) as usize;
        let count = input.read(&mut buffer[..limit])?;
        if count == 0 {
            return Err(DatabaseError::WriterFenced(
                "rollback input shortened during copy".into(),
            ));
        }
        remaining -= count as u64;
        digest.update(&buffer[..count]);
        if let Some(output) = output.as_mut() {
            output.write_all(&buffer[..count])?;
        }
    }
    Ok(digest.finalize().into())
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

pub(super) fn journal_path(path: &Path) -> PathBuf {
    sibling(path, "-journal")
}

/// SQLite can delete journal/WAL evidence for zero-page storage even when
/// opened read-only. Entrances without verified preimage recovery must refuse
/// before giving SQLite that main file or creating it over orphaned siblings.
pub(super) fn require_safe_unrecovered_open(path: &Path) -> Result<(), DatabaseError> {
    let empty = match std::fs::symlink_metadata(path) {
        Ok(metadata)
            if metadata.is_file()
                && metadata.nlink() == 1
                && metadata.uid() == rustix::process::geteuid().as_raw() =>
        {
            metadata.len() == 0
        }
        Ok(_) => {
            return Err(DatabaseError::WriterFenced(
                "startup requires owned singly linked regular main storage".into(),
            ))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => return Err(error.into()),
    };
    for suffix in ["-journal", "-wal", "-shm"] {
        match std::fs::symlink_metadata(sibling(path, suffix)) {
            Ok(_) if empty || suffix == "-journal" => {
                return Err(DatabaseError::WriterFenced(
                    "startup refuses unverified rollback or orphaned sibling storage".into(),
                ));
            }
            Ok(metadata)
                if metadata.is_file()
                    && metadata.nlink() == 1
                    && metadata.uid() == rustix::process::geteuid().as_raw() => {}
            Ok(_) => {
                return Err(DatabaseError::WriterFenced(
                    "startup requires owned singly linked regular SQLite siblings".into(),
                ))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

pub(super) fn has_journal(path: &Path) -> Result<bool, DatabaseError> {
    match std::fs::symlink_metadata(journal_path(path)) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

#[derive(PartialEq, Eq)]
struct Fingerprint {
    path: PathBuf,
    device: u64,
    inode: u64,
    length: u64,
    digest: [u8; 32],
}

pub(super) struct Preimage {
    main: Fingerprint,
    journal: Fingerprint,
}

impl Preimage {
    pub(super) fn require_current(&self) -> Result<(), DatabaseError> {
        require_no_wal(&self.main.path)?;
        for expected in [&self.main, &self.journal] {
            let mut input = Input::open(&expected.path)?;
            if input.device != expected.device
                || input.inode != expected.inode
                || input.length != expected.length
            {
                return Err(DatabaseError::WriterFenced(
                    "rollback preimage identity changed before writer open".into(),
                ));
            }
            input.verify_bytes(expected.digest)?;
            if expected.path == self.journal.path {
                validate_journal(&mut input)?;
            }
        }
        Ok(())
    }
}

fn fingerprint(input: &Input, digest: [u8; 32]) -> Fingerprint {
    Fingerprint {
        path: input.path.clone(),
        device: input.device,
        inode: input.inode,
        length: input.length,
        digest,
    }
}

fn validate_journal(journal: &mut Input) -> Result<u32, DatabaseError> {
    journal.require_current()?;
    if journal.length < 28 {
        return Err(DatabaseError::WriterFenced(
            "unknown rollback journal".into(),
        ));
    }
    journal.file.seek(SeekFrom::Start(0))?;
    let mut header = [0u8; 28];
    journal.file.read_exact(&mut header)?;
    journal.file.seek(SeekFrom::Start(journal.length - 8))?;
    let mut footer = [0u8; 8];
    journal.file.read_exact(&mut footer)?;
    if header[..8] != MAGIC || footer == MAGIC {
        return Err(DatabaseError::WriterFenced(
            "unsupported rollback journal or super-journal".into(),
        ));
    }
    let sector = u32::from_be_bytes(
        header[20..24]
            .try_into()
            .expect("fixed journal header range"),
    );
    let page = u32::from_be_bytes(
        header[24..28]
            .try_into()
            .expect("fixed journal header range"),
    );
    if !(512..=65536).contains(&sector)
        || !sector.is_power_of_two()
        || !(512..=65536).contains(&page)
        || !page.is_power_of_two()
        || journal.length < u64::from(sector)
    {
        return Err(DatabaseError::WriterFenced(
            "unsupported rollback journal layout".into(),
        ));
    }
    journal.require_current()?;
    Ok(u32::from_be_bytes(
        header[16..20]
            .try_into()
            .expect("fixed journal header range"),
    ))
}

fn require_no_wal(path: &Path) -> Result<(), DatabaseError> {
    for extension in ["db-wal", "db-shm"] {
        match std::fs::symlink_metadata(path.with_extension(extension)) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            _ => {
                return Err(DatabaseError::WriterFenced(
                    "rollback recovery refuses concurrent WAL storage".into(),
                ))
            }
        }
    }
    Ok(())
}

pub(super) fn require_quiet_preimage(path: &Path) -> Result<Preimage, DatabaseError> {
    require_no_wal(path)?;
    let mut main = Input::open(path)?;
    let mut journal = Input::open(&journal_path(path))?;
    // Pinned SQLite 3.45 readSuperJournal/writeSuperJournal use this trailing
    // magic for a super-journal reference. Never let private-copy recovery
    // inspect or delete a referenced file outside the private directory.
    let original_pages = validate_journal(&mut journal)?;
    if main.length == 0 && original_pages != 0 {
        return Err(DatabaseError::WriterFenced(
            "rollback journal cannot establish a truncated committed preimage".into(),
        ));
    }
    #[cfg(test)]
    tests::after_source_trailer(path);
    let private = tempfile::tempdir()?;
    let copy = private.path().join("preimage.db");
    let main_digest = main.copy(&copy)?;
    let copied_journal = journal_path(&copy);
    let journal_digest = journal.copy(&copied_journal)?;
    // Validate the frozen copied trailer too, before SQLite can interpret it.
    // A source change between trailer inspection and copying cannot introduce
    // an external super-journal pointer into private recovery.
    {
        let mut copied_input = Input::open(&copied_journal)?;
        validate_journal(&mut copied_input)?;
        copied_input.verify_bytes(journal_digest)?;
    }
    {
        let recovered = Connection::open(&copy)?;
        recovered.set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)?;
        // Only this private copy may be rolled back to expose committed state.
        // A retained or corrupt authenticated hold fails before actual recovery.
        super::startup::require_quiet(&recovered)?;
    }
    require_no_wal(path)?;
    main.verify_bytes(main_digest)?;
    journal.verify_bytes(journal_digest)?;
    // Raw main-file descriptors close here, before SQLite opens the actual
    // writer. Closing one later could release SQLite's process-scoped locks.
    let proof = Preimage {
        main: fingerprint(&main, main_digest),
        journal: fingerprint(&journal, journal_digest),
    };
    proof.require_current()?;
    Ok(proof)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    type Callback = Box<dyn FnOnce() + Send>;
    type Hook = (PathBuf, Callback);
    static HOOKS: OnceLock<Mutex<Vec<Hook>>> = OnceLock::new();

    fn hooks() -> &'static Mutex<Vec<Hook>> {
        HOOKS.get_or_init(|| Mutex::new(Vec::new()))
    }

    struct HookOwner(PathBuf);
    impl Drop for HookOwner {
        fn drop(&mut self) {
            hooks().lock().unwrap().retain(|hook| hook.0 != self.0);
        }
    }

    pub(super) fn after_source_trailer(path: &Path) {
        let callback = {
            let mut hooks = hooks().lock().unwrap();
            hooks
                .iter()
                .position(|hook| hook.0 == path)
                .map(|index| hooks.remove(index).1)
        };
        if let Some(callback) = callback {
            callback();
        }
    }

    #[test]
    fn appended_superjournal_after_source_validation_never_touches_external_file() {
        let _serial = crate::writer_fence::tests::TEST_LOCK.lock().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("personal.db");
        {
            let db = super::super::Database::new(&path).unwrap();
            db.initialize().unwrap();
            db.set_state("marker", "committed").unwrap();
            db.conn()
                .execute_batch("PRAGMA journal_mode=DELETE;")
                .unwrap();
        }
        let script = "import sqlite3,sys,os\nc=sqlite3.connect(sys.argv[1])\nc.executescript('PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL; PRAGMA cache_size=1; PRAGMA cache_spill=ON; BEGIN IMMEDIATE;')\nfor i in range(64): c.execute(\"INSERT INTO kv_state VALUES(?,?,?)\", ('spill-'+str(i),'x'*32768,'uncommitted'))\nos._exit(0)\n";
        let mut child = std::process::Command::new("python3")
            .args(["-c", script])
            .arg(&path)
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("spill child timed out");
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let journal = path.with_extension("db-journal");
        let bytes = std::fs::read(&journal).unwrap();
        assert_eq!(&bytes[..8], &MAGIC);
        let page_size = u32::from_be_bytes(bytes[24..28].try_into().unwrap());
        assert!((512..=65536).contains(&page_size));
        let external = directory.path().join("external-superjournal");
        std::fs::write(&external, []).unwrap();
        let native_path = path.canonicalize().unwrap();
        let hook_journal = journal.clone();
        let hook_external = external.clone();
        hooks().lock().unwrap().push((
            native_path.clone(),
            Box::new(move || {
                let name = hook_external.to_str().unwrap().as_bytes();
                let checksum = name
                    .iter()
                    .fold(0u32, |sum, byte| sum.wrapping_add((*byte as i8) as u32));
                let mut output = OpenOptions::new().append(true).open(hook_journal).unwrap();
                output
                    .write_all(&((0x40000000u32 / page_size) + 1).to_be_bytes())
                    .unwrap();
                output.write_all(name).unwrap();
                output
                    .write_all(&(name.len() as u32).to_be_bytes())
                    .unwrap();
                output.write_all(&checksum.to_be_bytes()).unwrap();
                output.write_all(&MAGIC).unwrap();
            }),
        ));
        let _hook_owner = HookOwner(native_path);
        let before = std::fs::read(&path).unwrap();
        let outcome = super::super::Database::open_personal_writer(directory.path());
        assert!(
            external.exists(),
            "private-copy recovery deleted an external super-journal"
        );
        assert_eq!(std::fs::read(&external).unwrap(), [0u8; 0]);
        assert!(outcome.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(std::fs::read(&journal).unwrap().ends_with(&MAGIC));
    }
}
