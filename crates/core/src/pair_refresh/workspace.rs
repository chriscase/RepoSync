//! Owned persistent preparation resources. This is not job admission authority.

use std::collections::BTreeMap;
use std::fs::{File, Metadata};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::Mutex;

use serde::Serialize;
use uuid::Uuid;

use crate::db::Database;
use crate::errors::DatabaseError;

use super::actor_policy::RefreshActorPolicyPin;

const NAMESPACE: &str = "pair-refresh-resources-v2";

#[cfg(test)]
#[path = "workspace_scm_role_tests.rs"]
mod scm_role_tests;

fn directory_flags() -> rustix::fs::OFlags {
    rustix::fs::OFlags::RDONLY
        | rustix::fs::OFlags::DIRECTORY
        | rustix::fs::OFlags::NOFOLLOW
        | rustix::fs::OFlags::CLOEXEC
}

fn open_directory(path: &Path) -> Result<File, DatabaseError> {
    rustix::fs::open(path, directory_flags(), rustix::fs::Mode::empty())
        .map(File::from)
        .map_err(|_| refused("could not open owned directory"))
}

fn open_child(
    parent: &File,
    name: &std::ffi::CStr,
    directory: bool,
) -> Result<File, DatabaseError> {
    let flags = if directory {
        directory_flags()
    } else {
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC
            | rustix::fs::OFlags::NONBLOCK
    };
    rustix::fs::openat(parent, name, flags, rustix::fs::Mode::empty())
        .map(File::from)
        .map_err(|_| refused("could not open confined resource"))
}

fn entry_names(directory: &File) -> Result<Vec<std::ffi::CString>, DatabaseError> {
    // Independent offsets and no /dev/fd reopening on macOS.
    let fd = rustix::fs::openat(directory, ".", directory_flags(), rustix::fs::Mode::empty())
        .map_err(|_| refused("could not open confined iteration"))?;
    let entries =
        rustix::fs::Dir::new(fd).map_err(|_| refused("could not iterate owned directory"))?;
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|_| refused("owned directory iteration failed"))?;
        if !matches!(entry.file_name().to_bytes(), b"." | b"..") {
            names.push(entry.file_name().to_owned());
        }
    }
    Ok(names)
}

fn sync_tree(directory: &File) -> Result<(), DatabaseError> {
    for name in entry_names(directory)? {
        let entry = entry_stat(directory, &name)?;
        let kind = rustix::fs::FileType::from_raw_mode(entry.st_mode);
        if kind == rustix::fs::FileType::Symlink {
            // The link is directory metadata. Never open its target, including
            // a dangling link or one pointing outside this resource tree.
            continue;
        }
        if !matches!(
            kind,
            rustix::fs::FileType::Directory | rustix::fs::FileType::RegularFile
        ) {
            return Err(refused(
                "resource synchronization requires an ordinary tree",
            ));
        }
        let file = open_child(directory, &name, kind == rustix::fs::FileType::Directory)?;
        require_entry_handle(&entry, &file)?;
        if kind == rustix::fs::FileType::Directory {
            sync_tree(&file)?;
        }
        file.sync_all()
            .map_err(|_| refused("resource synchronization failed"))?;
        require_same_entry(&entry, &entry_stat(directory, &name)?)?;
    }
    directory
        .sync_all()
        .map_err(|_| refused("resource directory synchronization failed"))
}

fn remove_contents(directory: &File) -> Result<(), DatabaseError> {
    for name in entry_names(directory)? {
        let entry = entry_stat(directory, &name)?;
        let is_directory =
            rustix::fs::FileType::from_raw_mode(entry.st_mode) == rustix::fs::FileType::Directory;
        if is_directory {
            let file = open_child(directory, &name, true)?;
            require_entry_handle(&entry, &file)?;
            remove_contents(&file)?;
        }
        // Inspect the namespace again before unlinking. The traversal used held
        // descriptors. Links and non-directory entries are unlinked directly;
        // no target is opened and a hard link only loses this directory entry.
        require_same_entry(&entry, &entry_stat(directory, &name)?)?;
        let flags = if is_directory {
            rustix::fs::AtFlags::REMOVEDIR
        } else {
            rustix::fs::AtFlags::empty()
        };
        rustix::fs::unlinkat(directory, name.as_c_str(), flags)
            .map_err(|_| refused("confined cleanup failed"))?;
    }
    Ok(())
}

fn entry_stat(directory: &File, name: &std::ffi::CStr) -> Result<rustix::fs::Stat, DatabaseError> {
    rustix::fs::statat(directory, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|_| refused("resource entry metadata is unavailable"))
}

fn require_same_entry(
    before: &rustix::fs::Stat,
    after: &rustix::fs::Stat,
) -> Result<(), DatabaseError> {
    if before.st_dev != after.st_dev
        || before.st_ino != after.st_ino
        || rustix::fs::FileType::from_raw_mode(before.st_mode)
            != rustix::fs::FileType::from_raw_mode(after.st_mode)
    {
        return Err(refused("resource entry changed during traversal"));
    }
    Ok(())
}

fn require_entry_handle(entry: &rustix::fs::Stat, file: &File) -> Result<(), DatabaseError> {
    let held =
        rustix::fs::fstat(file).map_err(|_| refused("held entry metadata is unavailable"))?;
    require_same_entry(entry, &held)
}

fn refused(detail: &str) -> DatabaseError {
    DatabaseError::Other(format!("pair refresh resources refused: {detail}"))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResourceIdentity {
    pub path: PathBuf,
    pub device: u64,
    pub inode: u64,
    pub owner: u32,
}

impl ResourceIdentity {
    fn capture_generated_private_directory(
        path: &Path,
        held: &File,
    ) -> Result<Self, DatabaseError> {
        // SVN creates its operation/working-copy roots using the process
        // umask. Only these generated role roots become private; versioned
        // children retain their original permissions and link semantics.
        let before = std::fs::symlink_metadata(path)
            .map_err(|_| refused("generated role metadata is unavailable"))?;
        if !before.is_dir()
            || before.uid() != rustix::process::geteuid().as_raw()
            || std::fs::canonicalize(path)
                .map_err(|_| refused("generated role path is unavailable"))?
                != path
        {
            return Err(refused("generated role type, owner or path is unsupported"));
        }
        let metadata = held
            .metadata()
            .map_err(|_| refused("held generated role metadata is unavailable"))?;
        if metadata.dev() != before.dev()
            || metadata.ino() != before.ino()
            || metadata.uid() != before.uid()
        {
            return Err(refused("generated role changed before private permissions"));
        }
        rustix::fs::fchmod(held, rustix::fs::Mode::from_raw_mode(0o700))
            .map_err(|_| refused("could not make generated role private"))?;
        let identity = Self::capture(path, true, true)?;
        identity.require_handle(held)?;
        Ok(identity)
    }
    fn require_handle(&self, file: &File) -> Result<(), DatabaseError> {
        let metadata = file
            .metadata()
            .map_err(|_| refused("held resource metadata is unavailable"))?;
        if metadata.dev() != self.device
            || metadata.ino() != self.inode
            || metadata.uid() != self.owner
        {
            return Err(refused("held resource identity changed"));
        }
        Ok(())
    }
    fn capture(path: &Path, directory: bool, private: bool) -> Result<Self, DatabaseError> {
        let metadata = std::fs::symlink_metadata(path)
            .map_err(|_| refused("resource metadata is unavailable"))?;
        Self::require_metadata(&metadata, directory, private)?;
        if std::fs::canonicalize(path).map_err(|_| refused("resource path is unavailable"))? != path
        {
            return Err(refused("resource path is not canonical"));
        }
        Ok(Self {
            path: path.to_owned(),
            device: metadata.dev(),
            inode: metadata.ino(),
            owner: metadata.uid(),
        })
    }

    fn require_metadata(
        metadata: &Metadata,
        directory: bool,
        private: bool,
    ) -> Result<(), DatabaseError> {
        if metadata.file_type().is_symlink()
            || (directory && !metadata.is_dir())
            || (!directory && !metadata.is_file())
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.mode() & 0o022 != 0
            || (private && metadata.mode() & 0o777 != 0o700)
            || (!directory && metadata.nlink() != 1)
        {
            return Err(refused(
                "resource type, owner, permissions or alias is unsupported",
            ));
        }
        Ok(())
    }

    fn require_current(&self, directory: bool, private: bool) -> Result<(), DatabaseError> {
        if Self::capture(&self.path, directory, private)? != *self {
            return Err(refused("resource identity changed"));
        }
        Ok(())
    }
}

/// Internal server-computed resource metadata for later authenticated binding.
/// A serialized pin cannot construct a live workspace or authorize cleanup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RefreshWorkspacePin {
    pub version: u8,
    pub operation_id: Uuid,
    pub database: ResourceIdentity,
    pub data_directory: ResourceIdentity,
    pub namespace: ResourceIdentity,
    pub operation_directory: ResourceIdentity,
    pub actor_policy: RefreshActorPolicyPin,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RefreshResourcePin {
    pub workspace: RefreshWorkspacePin,
    pub directories: BTreeMap<String, ResourceIdentity>,
}

struct WorkspaceOwner {
    pin: RefreshWorkspacePin,
    // Set before any durable enrollment attempt, not after a successful commit.
    // An unknown commit outcome must not trigger resource deletion on drop.
    retained: AtomicBool,
    directories: Mutex<BTreeMap<String, ResourceIdentity>>,
    namespace_handle: File,
    operation_handle: File,
}

impl WorkspaceOwner {
    fn require_root(&self) -> Result<(), DatabaseError> {
        self.pin.database.require_current(false, false)?;
        self.pin.data_directory.require_current(true, false)?;
        self.pin.namespace.require_current(true, true)?;
        self.pin.operation_directory.require_current(true, true)?;
        self.pin.namespace.require_handle(&self.namespace_handle)?;
        self.pin
            .operation_directory
            .require_handle(&self.operation_handle)?;
        Ok(())
    }

    fn require_current(&self) -> Result<(), DatabaseError> {
        self.require_root()?;
        for directory in self
            .directories
            .lock()
            .map_err(|_| refused("resource tracking is poisoned"))?
            .values()
        {
            directory.require_current(true, true)?;
        }
        Ok(())
    }
}

impl Drop for WorkspaceOwner {
    fn drop(&mut self) {
        if self.retained.load(Ordering::Acquire) {
            return;
        }
        // No search, expiry, adoption or deletion through a replacement path.
        let result = self.require_current().and_then(|()| {
            remove_contents(&self.operation_handle)?;
            self.require_root()?;
            rustix::fs::unlinkat(
                &self.namespace_handle,
                self.pin.operation_id.to_string(),
                rustix::fs::AtFlags::REMOVEDIR,
            )
            .map_err(|_| refused("unpublished resource directory cleanup failed"))
        });
        if let Err(error) = result {
            tracing::warn!(operation_id=%self.pin.operation_id,%error,"refresh resources retained after drop");
        }
    }
}

/// Clones share one owner. Unpublished cleanup occurs only after every source
/// snapshot has dropped. This type cannot be built from a client pin/path.
#[derive(Clone)]
pub struct RefreshWorkspace {
    owner: Arc<WorkspaceOwner>,
}

impl RefreshWorkspace {
    /// Snapshot the complete currently tracked resource layout for later
    /// authenticated enrollment. This snapshot is not cleanup/adoption authority.
    pub fn resource_pin(&self) -> Result<RefreshResourcePin, DatabaseError> {
        self.require_current()?;
        let directories = self
            .owner
            .directories
            .lock()
            .map_err(|_| refused("resource tracking is poisoned"))?
            .clone();
        Ok(RefreshResourcePin {
            workspace: self.owner.pin.clone(),
            directories,
        })
    }

    pub fn pin(&self) -> &RefreshWorkspacePin {
        &self.owner.pin
    }

    pub fn require_current(&self) -> Result<(), DatabaseError> {
        self.owner.require_current()
    }

    pub(crate) fn create_directory(&self, name: &str) -> Result<PathBuf, DatabaseError> {
        if !matches!(name, "git" | "svn") {
            return Err(refused("unknown resource role"));
        }
        self.require_current()?;
        let path = self.owner.pin.operation_directory.path.join(name);
        rustix::fs::mkdirat(
            &self.owner.operation_handle,
            name,
            rustix::fs::Mode::from_raw_mode(0o700),
        )
        .map_err(|_| refused("resource role must be fresh"))?;
        let identity = ResourceIdentity::capture(&path, true, true)?;
        let held = open_directory(&path)?;
        identity.require_handle(&held)?;
        self.owner
            .directories
            .lock()
            .map_err(|_| refused("resource tracking is poisoned"))?
            .insert(name.to_owned(), identity);
        self.require_current()?;
        Ok(path)
    }

    pub(crate) fn track_working_copy(&self, working_copy: &Path) -> Result<(), DatabaseError> {
        self.require_current()?;
        let operation = self
            .owner
            .pin
            .operation_directory
            .path
            .join("svn")
            .join(self.owner.pin.operation_id.to_string());
        if working_copy != operation.join("child") {
            return Err(refused("working copy is outside its owned role"));
        }
        let svn_identity = self
            .owner
            .directories
            .lock()
            .map_err(|_| refused("resource tracking is poisoned"))?
            .get("svn")
            .cloned()
            .ok_or_else(|| refused("SVN role is not owned"))?;
        let svn_handle = open_child(&self.owner.operation_handle, c"svn", true)?;
        svn_identity.require_handle(&svn_handle)?;
        let operation_name = std::ffi::CString::new(self.owner.pin.operation_id.to_string())
            .map_err(|_| refused("invalid operation directory name"))?;
        let operation_handle = open_child(&svn_handle, &operation_name, true)?;
        let working_handle = open_child(&operation_handle, c"child", true)?;
        let operation_identity =
            ResourceIdentity::capture_generated_private_directory(&operation, &operation_handle)?;
        let working_identity =
            ResourceIdentity::capture_generated_private_directory(working_copy, &working_handle)?;
        {
            let mut directories = self
                .owner
                .directories
                .lock()
                .map_err(|_| refused("resource tracking is poisoned"))?;
            if directories.contains_key("svn-operation")
                || directories.contains_key("svn-working-copy")
            {
                return Err(refused("working copy role was already tracked"));
            }
            directories.insert("svn-operation".into(), operation_identity);
            directories.insert("svn-working-copy".into(), working_identity);
        }
        self.require_current()
    }

    /// This does not authorize enrollment or publication. The future admission
    /// caller must mark retention before attempting its durable transaction.
    pub(crate) fn retain_before_enrollment(
        &self,
        authorize: impl FnOnce() -> Result<(), DatabaseError>,
    ) -> Result<(), DatabaseError> {
        self.require_current()?;
        sync_tree(&self.owner.operation_handle)?;
        self.owner
            .namespace_handle
            .sync_all()
            .map_err(|_| refused("resource namespace synchronization failed"))?;
        self.require_current()?;
        authorize()?;
        self.owner.retained.store(true, Ordering::Release);
        Ok(())
    }

    pub(crate) fn require_binding(
        &self,
        db: &Database,
        actor: &RefreshActorPolicyPin,
    ) -> Result<(), DatabaseError> {
        self.require_current()?;
        let opened = db.opened_file()?;
        if opened.aliased
            || opened.path != self.owner.pin.database.path
            || opened.device != self.owner.pin.database.device
            || opened.inode != self.owner.pin.database.inode
            || *actor != self.owner.pin.actor_policy
        {
            return Err(refused(
                "workspace belongs to another database or actor context",
            ));
        }
        Ok(())
    }
}

pub(crate) fn create(
    db: &Database,
    configured_data_directory: &Path,
    actor_policy: &RefreshActorPolicyPin,
) -> Result<RefreshWorkspace, DatabaseError> {
    let opened = db.opened_file()?;
    if opened.aliased {
        return Err(refused("database was opened through an alias"));
    }
    let raw_directory = std::fs::symlink_metadata(configured_data_directory)
        .map_err(|_| refused("configured data directory is unavailable"))?;
    ResourceIdentity::require_metadata(&raw_directory, true, false)?;
    let directory = std::fs::canonicalize(configured_data_directory)
        .map_err(|_| refused("configured data directory is unavailable"))?;
    if opened.path.parent() != Some(directory.as_path()) {
        return Err(refused(
            "configured data directory is not the connected database parent",
        ));
    }
    let database = ResourceIdentity::capture(&opened.path, false, false)?;
    if database.device != opened.device || database.inode != opened.inode {
        return Err(refused("connected database file identity changed"));
    }
    let data_directory = ResourceIdentity::capture(&directory, true, false)?;
    let data_handle = open_directory(&directory)?;
    data_directory.require_handle(&data_handle)?;
    // All known storage/actor holds refuse before a namespace/directory write.
    db.transaction(|tx| {
        crate::db::pair_refresh_operations::require_no_pair_refresh_hold(tx, None)
    })?;
    let namespace_path = directory.join(NAMESPACE);
    match rustix::fs::mkdirat(
        &data_handle,
        NAMESPACE,
        rustix::fs::Mode::from_raw_mode(0o700),
    ) {
        Ok(()) => {}
        Err(error) if error == rustix::io::Errno::EXIST => {}
        Err(_) => return Err(refused("could not create resource namespace")),
    }
    let namespace = ResourceIdentity::capture(&namespace_path, true, true)?;
    let namespace_handle = open_directory(&namespace_path)?;
    namespace.require_handle(&namespace_handle)?;
    let operation_id = Uuid::new_v4();
    let operation_path = namespace_path.join(operation_id.to_string());
    // No existing path is adopted or reinitialized, including an empty one.
    rustix::fs::mkdirat(
        &namespace_handle,
        operation_id.to_string(),
        rustix::fs::Mode::from_raw_mode(0o700),
    )
    .map_err(|_| refused("operation directory must be fresh"))?;
    let operation_directory = ResourceIdentity::capture(&operation_path, true, true)?;
    let operation_handle = open_directory(&operation_path)?;
    operation_directory.require_handle(&operation_handle)?;
    data_handle
        .sync_all()
        .map_err(|_| refused("data directory synchronization failed"))?;
    let workspace = RefreshWorkspace {
        owner: Arc::new(WorkspaceOwner {
            pin: RefreshWorkspacePin {
                version: 2,
                operation_id,
                database,
                data_directory,
                namespace,
                operation_directory,
                actor_policy: actor_policy.clone(),
            },
            retained: AtomicBool::new(false),
            directories: Mutex::new(BTreeMap::new()),
            namespace_handle,
            operation_handle,
        }),
    };
    workspace.require_current()?;
    Ok(workspace)
}
