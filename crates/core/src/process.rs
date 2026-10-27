//! Bounded child supervision for import commands. Unix children get their own
//! process group so cancellation also stops helper descendants.

use std::io;
use std::io::{Seek, Write};
use std::process::Output;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;

/// The child or its output-producing descendants could not be proven stopped.
/// Callers must hold the operation, irrespective of a cancellation request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnconfirmedCleanup {
    GroupTermination,
    OutputCompletion,
}

impl std::fmt::Display for UnconfirmedCleanup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "import command cleanup unconfirmed ({self:?})")
    }
}

impl std::error::Error for UnconfirmedCleanup {}

#[derive(Debug)]
struct ConfirmedStop;

impl std::fmt::Display for ConfirmedStop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("import command stopped with confirmed cleanup")
    }
}

impl std::error::Error for ConfirmedStop {}

pub fn cleanup_unconfirmed(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|source| source.is::<UnconfirmedCleanup>())
}

pub fn confirmed_cancelled(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::Interrupted
        && error
            .get_ref()
            .is_some_and(|source| source.is::<ConfirmedStop>())
}

fn stop_result(
    killed: io::Result<()>,
    reaped: Result<io::Result<Output>, tokio::time::error::Elapsed>,
    kind: io::ErrorKind,
) -> io::Result<Output> {
    if killed.is_err() {
        return Err(io::Error::other(UnconfirmedCleanup::GroupTermination));
    }
    if !matches!(reaped, Ok(Ok(_))) {
        return Err(io::Error::other(UnconfirmedCleanup::OutputCompletion));
    }
    Err(io::Error::new(kind, ConfirmedStop))
}

#[cfg(feature = "reliability-fixture")]
async fn fixture_cleanup_fault(command: &Command) -> io::Result<()> {
    use std::io::Write as _;
    let Some(root) = std::env::var_os("REPOSYNC_FIXTURE_ROOT") else {
        return Ok(());
    };
    let Some(dir) = std::env::var_os("REPOSYNC_IMPORT_FAULT_DIR") else {
        return Ok(());
    };
    let root_path = std::path::Path::new(&root);
    let root = root_path.canonicalize()?;
    let dir = std::path::Path::new(&dir).canonicalize()?;
    if !dir.starts_with(&root) {
        return Err(io::Error::other("fixture fault escaped sealed root"));
    }
    let args: Vec<_> = command
        .as_std()
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let program = command.as_std().get_program().to_string_lossy();
    let kind = if program.contains("svn") || args.first().is_some_and(|a| a.contains("svn")) {
        "svn"
    } else if program.contains("git") || args.first().is_some_and(|a| a.contains("git")) {
        "git"
    } else {
        return Ok(());
    };
    let stage = [
        "clone",
        "ls-remote",
        "info",
        "log",
        "diff",
        "export",
        "apply",
        "push",
        "commit",
    ]
    .into_iter()
    .find(|token| args.iter().any(|arg| arg == token))
    .map(|token| format!("{kind}-{token}"));
    let Some(stage) = stage else {
        return Ok(());
    };
    let scoped = command
        .as_std()
        .get_current_dir()
        .is_some_and(|p| p.starts_with(&root) || p.starts_with(root_path))
        || args.iter().any(|arg| {
            arg.contains(root.to_string_lossy().as_ref())
                || arg.contains(root_path.to_string_lossy().as_ref())
        });
    if !scoped {
        return Err(io::Error::other(
            "fixture fault command escaped sealed root",
        ));
    }
    writeln!(
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("commands.log"))?,
        "{stage}"
    )?;
    if std::env::var("REPOSYNC_IMPORT_FAULT_STAGE").ok().as_deref() == Some(stage.as_str()) {
        std::fs::write(dir.join("fault.ready"), stage.as_bytes())?;
        let released = tokio::time::timeout(Duration::from_secs(15), async {
            while !dir.join("fault.release").exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        if released.is_err() {
            return Err(io::Error::other("fixture fault barrier timed out"));
        }
        return Err(io::Error::other(UnconfirmedCleanup::OutputCompletion));
    }
    Ok(())
}

/// Fixture-scoped replacement for import Git commands. It cannot select a
/// path outside the disposable fixture and is absent from normal builds.
pub fn import_git_command() -> io::Result<Command> {
    #[cfg(feature = "reliability-fixture")]
    if let Some(binary) = std::env::var_os("REPOSYNC_IMPORT_GIT_BINARY") {
        let root = std::env::var_os("REPOSYNC_FIXTURE_ROOT")
            .ok_or_else(|| io::Error::other("fixture Git override lacks sealed root"))?;
        let root = std::path::Path::new(&root).canonicalize()?;
        let binary = std::path::Path::new(&binary).canonicalize()?;
        if !binary.starts_with(root) {
            return Err(io::Error::other("fixture Git override escaped sealed root"));
        }
        // Fixture tmpfs may deny direct script execution. The shell reads the
        // sealed script; the real Git CLI still executes inside that script.
        let mut command = Command::new("sh");
        command.arg(binary);
        return Ok(command);
    }
    Ok(Command::new("git"))
}

/// Stage patch bytes in an anonymous regular file before spawning the child.
/// This removes the unbounded pipe-stdin write: a child that never reads
/// standard input cannot block delivery or prevent cancellation.
pub async fn run_with_input(
    mut command: Command,
    input: &[u8],
    timeout: Duration,
    cancel: Option<&Arc<AtomicBool>>,
) -> io::Result<Output> {
    if cancel.is_some_and(|c| c.load(Ordering::Acquire)) {
        return Err(io::Error::new(io::ErrorKind::Interrupted, ConfirmedStop));
    }
    let mut file = tempfile::tempfile()?;
    file.write_all(input)?;
    file.rewind()?;
    command.stdin(Stdio::from(file));
    run(command, timeout, cancel).await
}

/// Keep import Git/LFS children on the command's workdir instead of an
/// ambient checkout. CI runs `git lfs install` globally; without this, a
/// supervised `git lfs version` / clone can inherit `GIT_DIR` or smudge
/// against the RepoSync workspace LFS remote and never finish.
fn isolate_import_git_env(command: &mut Command) {
    command.env("GIT_TERMINAL_PROMPT", "0");
    command.env("GIT_LFS_SKIP_SMUDGE", "1");
    command.env_remove("GIT_DIR");
    command.env_remove("GIT_WORK_TREE");
    command.env_remove("GIT_OBJECT_DIRECTORY");
    command.env_remove("GIT_COMMON_DIR");
    command.env_remove("GIT_INDEX_FILE");
}


use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::AtomicU64;
use std::sync::{Mutex, OnceLock};
use std::task::{Context, Poll};

struct LiveHandles {
    group_id: i32,
    wait: Option<tokio::task::JoinHandle<io::Result<std::process::ExitStatus>>>,
    stdout: Option<tokio::task::JoinHandle<io::Result<Vec<u8>>>>,
    stderr: Option<tokio::task::JoinHandle<io::Result<Vec<u8>>>>,
    exit: Option<std::process::ExitStatus>,
    stdout_bytes: Option<Vec<u8>>,
    stderr_bytes: Option<Vec<u8>>,
    wait_done: bool,
    reader_failed: bool,
}

enum SlotState {
    Reserved,
    Active(LiveHandles),
    Unresolved(LiveHandles),
}

struct RetainTable {
    slots: HashMap<u64, SlotState>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    task_join_in_progress: bool,
    task_collector: Option<u64>,
    task_failures: Vec<String>,
    unresolved_context: bool,
    legacy_context_owners: HashMap<u64, usize>,
    case_context: Option<CaseContextState>,
}

static PROCESS_TABLE: OnceLock<Mutex<RetainTable>> = OnceLock::new();
static NEXT_PROCESS_ID: AtomicU64 = AtomicU64::new(1);

fn table() -> std::sync::MutexGuard<'static, RetainTable> {
    PROCESS_TABLE
        .get_or_init(|| {
            Mutex::new(RetainTable {
                slots: HashMap::new(),
                tasks: Vec::new(),
                task_join_in_progress: false,
                task_collector: None,
                task_failures: Vec::new(),
                unresolved_context: false,
                legacy_context_owners: HashMap::new(),
                case_context: None,
            })
        })
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

fn ownership_incomplete(table: &RetainTable) -> bool {
    table.unresolved_context
        || !table.legacy_context_owners.is_empty()
        || table.task_join_in_progress
        || table.task_collector.is_some()
        || !table.tasks.is_empty()
        || !table.task_failures.is_empty()
        || table
            .slots
            .values()
            .any(|slot| matches!(slot, SlotState::Unresolved(_)))
}

pub fn retained_ownership_incomplete() -> bool {
    ownership_incomplete(&table())
}

pub fn retained_child_and_readers_held() -> bool {
    let table = table();
    !table.tasks.is_empty()
        || table.slots.values().any(|slot| {
            matches!(slot, SlotState::Active(_) | SlotState::Unresolved(_))
        })
}

pub fn retained_group_id() -> i32 {
    let table = table();
    let mut found = None;
    for slot in table.slots.values() {
        let group_id = match slot {
            SlotState::Active(handles) | SlotState::Unresolved(handles) => handles.group_id,
            SlotState::Reserved => continue,
        };
        if found.is_some() {
            return 0;
        }
        found = Some(group_id);
    }
    found.unwrap_or(0)
}

pub fn note_unresolved_context() {
    table().unresolved_context = true;
}

// Compatibility for the unmodified peer producers. Their file-local integers
// are never case-context identities and cannot clear an ownerless failure.
pub fn note_session_context_retained(owner: u64) -> bool {
    let mut retained = table();
    let foreign = !retained.legacy_context_owners.is_empty();
    *retained.legacy_context_owners.entry(owner).or_default() += 1;
    foreign
}

pub fn release_session_context(owner: u64) -> Result<(), String> {
    let mut retained = table();
    if retained.unresolved_context
        || retained.task_join_in_progress
        || retained.task_collector.is_some()
        || !retained.tasks.is_empty()
        || !retained.task_failures.is_empty()
        || retained.slots.values().any(|slot| matches!(slot, SlotState::Unresolved(_)))
    {
        return Err("cleanup is uncertain; context stays retained".to_string());
    }
    match retained.legacy_context_owners.get_mut(&owner) {
        Some(count) if *count == 1 => { retained.legacy_context_owners.remove(&owner); }
        Some(count) => { *count -= 1; }
        None => return Err("this legacy owner has no retained context".to_string()),
    }
    Ok(())
}

/// Opaque identity of one admitted test case context. It is not an operation id,
/// an environment value, or a serialized authorization capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CaseContextId(u64);

impl CaseContextId {
    pub(crate) fn serial_number(self) -> u64 { self.0 }
}

struct CaseContextState {
    id: CaseContextId,
    retained: bool,
    closed: bool,
    failed: bool,
    blocking: usize,
    resources: usize,
    settled_blocking: usize,
}

static NEXT_CASE_CONTEXT: AtomicU64 = AtomicU64::new(1);

/// The context gate is process-wide. Ordinary process reservations do not take
/// it; they remain independent operations, including while a healthy case runs.
pub fn admit_case_context() -> Result<CaseContextId, String> {
    let mut retained = table();
    if retained.case_context.is_some() || ownership_incomplete(&retained) {
        return Err("case context or prior cleanup is unresolved".to_string());
    }
    let number = NEXT_CASE_CONTEXT
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |old| old.checked_add(1))
        .map_err(|_| "case identity exhausted".to_string())?;
    let id = CaseContextId(number);
    retained.case_context = Some(CaseContextState {
        id, retained: false, closed: false, failed: false,
        blocking: 0, resources: 0, settled_blocking: 0,
    });
    Ok(id)
}

pub fn case_context_held(id: CaseContextId) -> bool {
    table().case_context.as_ref().is_some_and(|case| case.id == id)
}

pub(crate) fn case_registration_open(id: CaseContextId) -> bool {
    let retained = table();
    !retained.unresolved_context && retained.case_context.as_ref().is_some_and(|case| {
        case.id == id && !case.closed && !case.failed
    })
}

pub(crate) fn retain_case_context(id: CaseContextId) {
    if let Some(case) = table().case_context.as_mut().filter(|case| case.id == id) {
        case.retained = true;
    }
}

pub(crate) fn close_case_registration(id: CaseContextId) -> Result<(), String> {
    let mut retained = table();
    let case = retained.case_context.as_mut().filter(|case| case.id == id)
        .ok_or_else(|| "case context changed before registration closed".to_string())?;
    case.closed = true;
    Ok(())
}

pub(crate) fn case_blocking_ready(id: CaseContextId) -> bool {
    let retained = table();
    !retained.unresolved_context && retained.case_context.as_ref().is_some_and(|case| {
        case.id == id && case.closed && !case.failed && case.blocking == 0
            && case.settled_blocking > 0
    })
}

pub(crate) fn case_context_ready(id: CaseContextId) -> bool {
    let retained = table();
    !retained.unresolved_context && retained.case_context.as_ref().is_some_and(|case| {
        case.id == id && !case.failed && case.blocking == 0 && case.resources == 0
            && (case.settled_blocking == 0 || case.closed)
    })
}

pub(crate) fn release_case_context(id: CaseContextId, explicit: bool) -> Result<(), String> {
    let mut retained = table();
    let ready = !ownership_incomplete(&retained) && retained.case_context.as_ref().is_some_and(|case| {
        case.id == id && !case.failed && case.blocking == 0 && case.resources == 0
            && (case.settled_blocking == 0 || case.closed)
            && (explicit || !case.retained)
    });
    if !ready { return Err("case handles or terminal failure remain retained".to_string()); }
    retained.case_context = None;
    Ok(())
}

pub(crate) struct CaseWorkToken {
    id: CaseContextId,
    resource: bool,
    settled: bool,
}

impl CaseWorkToken {
    pub(crate) fn reserve(id: CaseContextId, resource: bool) -> Result<Self, String> {
        let mut retained = table();
        let case = retained.case_context.as_mut().filter(|case| case.id == id)
            .ok_or_else(|| "blocking invocation has no admitted case".to_string())?;
        if case.closed || case.failed {
            return Err("case registration is closed or failed".to_string());
        }
        if resource { case.resources += 1; } else { case.blocking += 1; }
        Ok(Self { id, resource, settled: false })
    }

    pub(crate) fn complete(mut self) {
        let mut retained = table();
        if let Some(case) = retained.case_context.as_mut().filter(|case| case.id == self.id) {
            if self.resource { case.resources -= 1; }
            else { case.blocking -= 1; case.settled_blocking += 1; }
        }
        self.settled = true;
    }
}

impl Drop for CaseWorkToken {
    fn drop(&mut self) {
        if self.settled { return; }
        if let Some(case) = table().case_context.as_mut().filter(|case| case.id == self.id) {
            if self.resource { case.resources -= 1; } else { case.blocking -= 1; }
            case.failed = true;
            case.retained = true;
        }
    }
}

// The registry owns actual fixture resources, not only their identifiers.
// Resource access never holds PROCESS_TABLE across a poll or a user callback.
struct CaseResource {
    session: CaseContextId,
    value: Box<dyn std::any::Any + Send>,
    work: CaseWorkToken,
}
static CASE_RESOURCES: OnceLock<Mutex<HashMap<u64, CaseResource>>> = OnceLock::new();
static NEXT_CASE_RESOURCE: AtomicU64 = AtomicU64::new(1);

fn case_resources() -> std::sync::MutexGuard<'static, HashMap<u64, CaseResource>> {
    CASE_RESOURCES.get_or_init(|| Mutex::new(HashMap::new()))
        .lock().unwrap_or_else(|poison| poison.into_inner())
}

pub(crate) struct CaseResourceHandle<T> {
    id: u64,
    session: CaseContextId,
    marker: std::marker::PhantomData<fn() -> T>,
}
impl<T> Copy for CaseResourceHandle<T> {}
impl<T> Clone for CaseResourceHandle<T> {
    fn clone(&self) -> Self { *self }
}

pub(crate) fn register_case_resource<T: Send + 'static>(
    session: CaseContextId,
    create: impl FnOnce() -> Result<T, String>,
) -> Result<CaseResourceHandle<T>, String> {
    let work = CaseWorkToken::reserve(session, true)?;
    let id = NEXT_CASE_RESOURCE.fetch_update(Ordering::AcqRel, Ordering::Acquire,
        |old| old.checked_add(1)).map_err(|_| "case resource identity exhausted".to_string())?;
    // No await between creation and registration. A failed creation retains a
    // failed context through the token; it is not a successful empty case.
    let value = create()?;
    case_resources().insert(id, CaseResource { session, value: Box::new(value), work });
    Ok(CaseResourceHandle { id, session, marker: std::marker::PhantomData })
}

pub(crate) fn with_case_resource<T: Send + 'static, R>(
    handle: CaseResourceHandle<T>,
    access: impl FnOnce(&mut T) -> R,
) -> Result<R, String> {
    let mut resources = case_resources();
    let resource = resources.get_mut(&handle.id).filter(|r| r.session == handle.session)
        .ok_or_else(|| "case resource disappeared or changed owner".to_string())?;
    let value = resource.value.downcast_mut::<T>()
        .ok_or_else(|| "case resource type changed".to_string())?;
    Ok(access(value))
}

pub(crate) fn case_resource_handles<T: Send + 'static>(session: CaseContextId)
    -> Vec<CaseResourceHandle<T>>
{
    case_resources().iter().filter_map(|(id, resource)| {
        (resource.session == session && resource.value.is::<T>()).then_some(CaseResourceHandle {
            id: *id, session, marker: std::marker::PhantomData,
        })
    }).collect()
}

/// Called only by the typed owner after checked completion; dropping a handle
/// does nothing. An interrupted observer therefore cannot discard the resource.
pub(crate) fn finish_case_resource<T: Send + 'static>(handle: CaseResourceHandle<T>)
    -> Result<(), String>
{
    let resource = {
        let mut resources = case_resources();
        let valid = resources.get(&handle.id).is_some_and(|resource| {
            resource.session == handle.session && resource.value.is::<T>()
        });
        if !valid { return Err("case resource completion changed owner/type".into()); }
        resources.remove(&handle.id).unwrap()
    };
    let CaseResource { value, work, .. } = resource;
    // Destructors run outside both registry locks. The case gate is still held.
    drop(value);
    work.complete();
    Ok(())
}

pub(crate) fn owned_group_quiescent(group_id: i32) -> io::Result<bool> {
    observe_group(group_id).map(|alive| !alive)
}

#[cfg(test)]
fn collector_is_active() -> bool {
    table().task_collector.is_some()
}

pub fn retain_unjoined_task(handle: tokio::task::JoinHandle<()>) {
    table().tasks.push(handle);
}

fn handles_mut(slot: &mut SlotState) -> Option<&mut LiveHandles> {
    match slot {
        SlotState::Active(handles) | SlotState::Unresolved(handles) => Some(handles),
        SlotState::Reserved => None,
    }
}

fn group_of(id: u64) -> i32 {
    let mut table = table();
    let Some(slot) = table.slots.get_mut(&id) else {
        return 0;
    };
    handles_mut(slot).map(|handles| handles.group_id).unwrap_or(0)
}

fn classify_kill_errno(rc: i32, errno: Option<i32>) -> io::Result<bool> {
    if rc == 0 {
        return Ok(true);
    }
    if errno == Some(libc::ESRCH) {
        return Ok(false);
    }
    Err(io::Error::other(UnconfirmedCleanup::GroupTermination))
}

fn classify_kill_presence(rc: i32) -> io::Result<bool> {
    let errno = if rc == 0 {
        None
    } else {
        Some(io::Error::last_os_error().raw_os_error()).flatten()
    };
    classify_kill_errno(rc, errno)
}

fn observe_group(group_id: i32) -> io::Result<bool> {
    if group_id <= 0 {
        return Err(io::Error::other(UnconfirmedCleanup::GroupTermination));
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = group_id;
        return Err(io::Error::other(UnconfirmedCleanup::GroupTermination));
    }
    #[cfg(target_os = "linux")]
    {
        let entries = std::fs::read_dir("/proc").map_err(|_| {
            io::Error::other(UnconfirmedCleanup::GroupTermination)
        })?;
        for entry in entries {
            let entry = entry.map_err(|_| {
                io::Error::other(UnconfirmedCleanup::GroupTermination)
            })?;
            let name = entry.file_name();
            let name_text = name.to_string_lossy();
            if !name_text.bytes().all(|byte| byte.is_ascii_digit()) {
                continue;
            }
            let Ok(pid) = name_text.parse::<i32>() else {
                return Err(io::Error::other(UnconfirmedCleanup::GroupTermination));
            };
            let stat_path = format!("/proc/{pid}/stat");
            let stat = match std::fs::read_to_string(&stat_path) {
                Ok(stat) => stat,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(_) => {
                    return Err(io::Error::other(UnconfirmedCleanup::GroupTermination));
                }
            };
            let Some((_, rest)) = stat.rsplit_once(") ") else {
                return Err(io::Error::other(UnconfirmedCleanup::GroupTermination));
            };
            let mut fields = rest.split_whitespace();
            let Some(state) = fields.next() else {
                return Err(io::Error::other(UnconfirmedCleanup::GroupTermination));
            };
            let Some(_ppid) = fields.next() else {
                return Err(io::Error::other(UnconfirmedCleanup::GroupTermination));
            };
            let Some(pgrp) = fields.next() else {
                return Err(io::Error::other(UnconfirmedCleanup::GroupTermination));
            };
            if pgrp.parse::<i32>().ok() != Some(group_id) {
                continue;
            }
            if state.starts_with('Z') || state.starts_with('X') {
                continue;
            }
            match classify_kill_presence(unsafe { libc::kill(pid, 0) }) {
                Ok(true) => return Ok(true),
                Ok(false) => continue,
                Err(error) => return Err(error),
            }
        }
        Ok(false)
    }
}

struct OperationLease {
    id: u64,
    settled: bool,
}

impl OperationLease {
    fn reserve() -> io::Result<Self> {
        let mut table = table();
        if ownership_incomplete(&table) {
            return Err(io::Error::other(UnconfirmedCleanup::OutputCompletion));
        }
        let id = NEXT_PROCESS_ID.fetch_add(1, Ordering::Relaxed);
        table.slots.insert(id, SlotState::Reserved);
        Ok(Self { id, settled: false })
    }

    fn release_reservation(&mut self) {
        if self.settled {
            return;
        }
        let mut table = table();
        if matches!(table.slots.get(&self.id), Some(SlotState::Reserved)) {
            table.slots.remove(&self.id);
        }
        self.settled = true;
    }

    fn release_success(&mut self) {
        table().slots.remove(&self.id);
        self.settled = true;
    }
}

impl Drop for OperationLease {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        let mut table = table();
        match table.slots.remove(&self.id) {
            Some(SlotState::Active(handles)) | Some(SlotState::Unresolved(handles)) => {
                table.slots.insert(self.id, SlotState::Unresolved(handles));
            }
            Some(SlotState::Reserved) | None => {}
        }
    }
}

fn spawn_reader<R>(pipe: Option<R>) -> tokio::task::JoinHandle<io::Result<Vec<u8>>>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(mut pipe) = pipe {
            tokio::io::AsyncReadExt::read_to_end(&mut pipe, &mut buf).await?;
        }
        Ok(buf)
    })
}

fn poll_wait_slot(id: u64, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
    let mut table = table();
    let Some(slot) = table.slots.get_mut(&id) else {
        return Poll::Ready(Err(io::Error::other(UnconfirmedCleanup::OutputCompletion)));
    };
    let Some(handles) = handles_mut(slot) else {
        return Poll::Ready(Err(io::Error::other(UnconfirmedCleanup::OutputCompletion)));
    };
    if handles.wait_done {
        return Poll::Ready(Ok(()));
    }
    let polled = match handles.wait.as_mut() {
        Some(wait) => Pin::new(wait).poll(cx),
        None => return Poll::Ready(Err(io::Error::other(UnconfirmedCleanup::OutputCompletion))),
    };
    match polled {
        Poll::Pending => Poll::Pending,
        Poll::Ready(Ok(Ok(status))) => {
            handles.wait.take();
            handles.exit = Some(status);
            handles.wait_done = true;
            Poll::Ready(Ok(()))
        }
        Poll::Ready(_) => {
            handles.wait.take();
            handles.reader_failed = true;
            Poll::Ready(Err(io::Error::other(UnconfirmedCleanup::OutputCompletion)))
        }
    }
}

fn poll_reader_slot(id: u64, stdout: bool, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
    let mut table = table();
    let Some(slot) = table.slots.get_mut(&id) else {
        return Poll::Ready(Err(io::Error::other(UnconfirmedCleanup::OutputCompletion)));
    };
    let Some(handles) = handles_mut(slot) else {
        return Poll::Ready(Err(io::Error::other(UnconfirmedCleanup::OutputCompletion)));
    };
    if handles.reader_failed {
        return Poll::Ready(Err(io::Error::other(UnconfirmedCleanup::OutputCompletion)));
    }
    let stored = if stdout {
        handles.stdout_bytes.is_some()
    } else {
        handles.stderr_bytes.is_some()
    };
    if stored {
        return Poll::Ready(Ok(()));
    }
    let polled = if stdout {
        match handles.stdout.as_mut() {
            Some(pipe) => Pin::new(pipe).poll(cx),
            None => return Poll::Ready(Err(io::Error::other(UnconfirmedCleanup::OutputCompletion))),
        }
    } else {
        match handles.stderr.as_mut() {
            Some(pipe) => Pin::new(pipe).poll(cx),
            None => return Poll::Ready(Err(io::Error::other(UnconfirmedCleanup::OutputCompletion))),
        }
    };
    match polled {
        Poll::Pending => Poll::Pending,
        Poll::Ready(Ok(Ok(bytes))) => {
            if stdout {
                handles.stdout.take();
                handles.stdout_bytes = Some(bytes);
            } else {
                handles.stderr.take();
                handles.stderr_bytes = Some(bytes);
            }
            Poll::Ready(Ok(()))
        }
        Poll::Ready(Ok(Err(error))) => {
            if stdout {
                handles.stdout.take();
            } else {
                handles.stderr.take();
            }
            handles.reader_failed = true;
            Poll::Ready(Err(error))
        }
        Poll::Ready(Err(_)) => {
            if stdout {
                handles.stdout.take();
            } else {
                handles.stderr.take();
            }
            handles.reader_failed = true;
            Poll::Ready(Err(io::Error::other(UnconfirmedCleanup::OutputCompletion)))
        }
    }
}

async fn join_wait(id: u64) -> io::Result<()> {
    std::future::poll_fn(|cx| poll_wait_slot(id, cx)).await
}

async fn join_reader(id: u64, stdout: bool) -> io::Result<()> {
    std::future::poll_fn(|cx| poll_reader_slot(id, stdout, cx)).await
}

async fn join_outputs(id: u64) -> io::Result<()> {
    match tokio::time::timeout(Duration::from_secs(5), join_wait(id)).await {
        Ok(result) => result?,
        Err(_) => return Err(io::Error::other(UnconfirmedCleanup::OutputCompletion)),
    }
    match tokio::time::timeout(Duration::from_secs(5), join_reader(id, true)).await {
        Ok(result) => result?,
        Err(_) => return Err(io::Error::other(UnconfirmedCleanup::OutputCompletion)),
    }
    match tokio::time::timeout(Duration::from_secs(5), join_reader(id, false)).await {
        Ok(result) => result?,
        Err(_) => return Err(io::Error::other(UnconfirmedCleanup::OutputCompletion)),
    }
    Ok(())
}

fn take_output(id: u64) -> io::Result<Output> {
    let mut table = table();
    let Some(slot) = table.slots.get_mut(&id) else {
        return Err(io::Error::other(UnconfirmedCleanup::OutputCompletion));
    };
    let Some(handles) = handles_mut(slot) else {
        return Err(io::Error::other(UnconfirmedCleanup::OutputCompletion));
    };
    if handles.reader_failed || !handles.wait_done {
        return Err(io::Error::other(UnconfirmedCleanup::OutputCompletion));
    }
    let status = handles
        .exit
        .take()
        .ok_or_else(|| io::Error::other(UnconfirmedCleanup::OutputCompletion))?;
    let stdout = handles.stdout_bytes.take().unwrap_or_default();
    let stderr = handles.stderr_bytes.take().unwrap_or_default();
    Ok(Output { status, stdout, stderr })
}

async fn finish_direct(lease: &mut OperationLease) -> io::Result<Output> {
    join_outputs(lease.id).await?;
    let group_id = group_of(lease.id);
    match observe_group(group_id) {
        Ok(false) => {}
        Ok(true) | Err(_) => {
            return Err(io::Error::other(UnconfirmedCleanup::GroupTermination));
        }
    }
    let output = take_output(lease.id)?;
    lease.release_success();
    Ok(output)
}

async fn finish_stop(lease: &mut OperationLease, kind: io::ErrorKind) -> io::Result<Output> {
    let group_id = group_of(lease.id);
    if group_id <= 0 || kill_group(Some(group_id as u32)).is_err() {
        return Err(io::Error::other(UnconfirmedCleanup::GroupTermination));
    }
    if let Err(error) = join_outputs(lease.id).await {
        return Err(error);
    }
    match observe_group(group_id) {
        Ok(false) => {
            lease.release_success();
            Err(io::Error::new(kind, ConfirmedStop))
        }
        Ok(true) | Err(_) => Err(io::Error::other(UnconfirmedCleanup::GroupTermination)),
    }
}

pub async fn run(
    mut command: Command,
    timeout: Duration,
    cancel: Option<&Arc<AtomicBool>>,
) -> io::Result<Output> {
    let mut lease = OperationLease::reserve()?;
    if retained_ownership_incomplete() {
        lease.release_reservation();
        return Err(io::Error::other(UnconfirmedCleanup::OutputCompletion));
    }
    if cancel.is_some_and(|flag| flag.load(Ordering::Acquire)) {
        lease.release_reservation();
        return Err(io::Error::new(io::ErrorKind::Interrupted, ConfirmedStop));
    }
    #[cfg(feature = "reliability-fixture")]
    if let Err(error) = fixture_cleanup_fault(&command).await {
        lease.release_reservation();
        return Err(error);
    }
    if retained_ownership_incomplete() {
        lease.release_reservation();
        return Err(io::Error::other(UnconfirmedCleanup::OutputCompletion));
    }
    isolate_import_git_env(&mut command);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    command.kill_on_drop(false);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.as_std_mut().process_group(0);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            lease.release_reservation();
            return Err(error);
        }
    };
    let pid = child.id().unwrap_or(0);
    let stdout = spawn_reader(child.stdout.take());
    let stderr = spawn_reader(child.stderr.take());
    let wait = tokio::spawn(async move { child.wait().await });
    let handles = LiveHandles {
        group_id: pid as i32,
        wait: Some(wait),
        stdout: Some(stdout),
        stderr: Some(stderr),
        exit: None,
        stdout_bytes: None,
        stderr_bytes: None,
        wait_done: false,
        reader_failed: false,
    };
    {
        let mut retained = table();
        let reserved = matches!(retained.slots.get(&lease.id), Some(SlotState::Reserved));
        if reserved && pid > 0 {
            retained.slots.insert(lease.id, SlotState::Active(handles));
        } else {
            retained.slots.insert(lease.id, SlotState::Unresolved(handles));
            lease.settled = true;
            return Err(io::Error::other(UnconfirmedCleanup::OutputCompletion));
        }
    }
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            biased;
            joined = join_wait(lease.id) => {
                if let Err(error) = joined {
                    return Err(error);
                }
                return finish_direct(&mut lease).await;
            }
            _ = &mut deadline => return finish_stop(&mut lease, io::ErrorKind::TimedOut).await,
            _ = tokio::time::sleep(Duration::from_millis(100)), if cancel.is_some() => {
                if cancel.is_some_and(|flag| flag.load(Ordering::Acquire)) {
                    return finish_stop(&mut lease, io::ErrorKind::Interrupted).await;
                }
            }
        }
    }
}

struct RestoreTasks {
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Drop for RestoreTasks {
    fn drop(&mut self) {
        if self.tasks.is_empty() {
            return;
        }
        table().tasks.append(&mut self.tasks);
    }
}

struct CollectorLease {
    id: u64,
}

impl Drop for CollectorLease {
    fn drop(&mut self) {
        let mut retained = table();
        if retained.task_collector != Some(self.id) {
            return;
        }
        retained.task_collector = None;
        retained.task_join_in_progress = !retained.tasks.is_empty() || !retained.task_failures.is_empty();
    }
}

static NEXT_COLLECTOR: AtomicU64 = AtomicU64::new(1);

async fn join_retained_tasks(budget: Duration) -> io::Result<()> {
    let id = NEXT_COLLECTOR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let lease = {
        let mut retained = table();
        if retained.task_collector.is_some() {
            return Err(io::Error::other(UnconfirmedCleanup::OutputCompletion));
        }
        if retained.tasks.is_empty() {
            retained.task_join_in_progress = !retained.task_failures.is_empty();
            return if retained.task_failures.is_empty() { Ok(()) }
                else { Err(io::Error::other(UnconfirmedCleanup::OutputCompletion)) };
        }
        retained.task_collector = Some(id);
        retained.task_join_in_progress = true;
        CollectorLease { id }
    };
    let start = std::time::Instant::now();
    let result = async {
        loop {
            let mut batch = RestoreTasks {
                tasks: {
                    let mut retained = table();
                    if retained.task_collector != Some(lease.id) {
                        return Err(io::Error::other(UnconfirmedCleanup::OutputCompletion));
                    }
                    if retained.tasks.is_empty() {
                        return if retained.task_failures.is_empty() { Ok(()) }
                            else { Err(io::Error::other(UnconfirmedCleanup::OutputCompletion)) };
                    }
                    if start.elapsed() >= budget {
                        return Err(io::Error::other(UnconfirmedCleanup::OutputCompletion));
                    }
                    std::mem::take(&mut retained.tasks)
                },
            };
            let mut index = 0;
            let slice_deadline = std::time::Instant::now() + Duration::from_millis(50);
            while index < batch.tasks.len() && std::time::Instant::now() < slice_deadline {
                let ready = std::future::poll_fn(|cx| Pin::new(&mut batch.tasks[index]).poll(cx));
                match tokio::time::timeout(Duration::from_millis(20), ready).await {
                    Ok(Ok(())) => { batch.tasks.swap_remove(index); }
                    Ok(Err(error)) => {
                        // Ready(Err) consumed the handle. Preserve a terminal
                        // failure instead of putting that future back to poll.
                        batch.tasks.swap_remove(index);
                        table().task_failures.push(error.to_string());
                    }
                    Err(_) => index += 1,
                }
            }
            let pending = !batch.tasks.is_empty();
            {
                let mut retained = table();
                retained.tasks.append(&mut batch.tasks);
            }
            if !pending {
                let retained = table();
                if retained.task_collector == Some(lease.id) && retained.tasks.is_empty() {
                    return if retained.task_failures.is_empty() { Ok(()) }
                        else { Err(io::Error::other(UnconfirmedCleanup::OutputCompletion)) };
                }
            }
            if start.elapsed() >= budget {
                return Err(io::Error::other(UnconfirmedCleanup::OutputCompletion));
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    .await;
    drop(lease);
    result
}

fn cohort_ids(cohort: Option<&[u64]>) -> Vec<(u64, i32)> {
    let table = table();
    table
        .slots
        .iter()
        .filter_map(|(id, slot)| {
            let selected = match cohort {
                Some(ids) => ids.contains(id),
                None => matches!(slot, SlotState::Unresolved(_)),
            };
            if !selected {
                return None;
            }
            match slot {
                SlotState::Active(handles) | SlotState::Unresolved(handles) => {
                    Some((*id, handles.group_id))
                }
                SlotState::Reserved => None,
            }
        })
        .collect()
}

async fn observe_ids(cohort: Option<&[u64]>, budget: Duration) -> io::Result<()> {
    let start = std::time::Instant::now();
    loop {
        let ids = cohort_ids(cohort);
        if cohort.is_some() {
            if ids.is_empty() {
                return Ok(());
            }
        } else {
            let retained = table();
            if ids.is_empty()
                && retained.tasks.is_empty()
                && retained.task_collector.is_none()
                && !retained.task_join_in_progress
                && retained.task_failures.is_empty()
            {
                return Ok(());
            }
        }
        if start.elapsed() >= budget {
            return Err(io::Error::other(UnconfirmedCleanup::GroupTermination));
        }
        let mut blocked = Vec::new();
        for (id, group_id) in &ids {
            if *group_id > 0 && kill_group(Some(*group_id as u32)).is_err() {
                blocked.push(*id);
            }
        }
        let mut ready = Vec::new();
        for (id, group_id) in &ids {
            if blocked.contains(id) {
                continue;
            }
            let joined = tokio::time::timeout(Duration::from_millis(50), join_outputs(*id)).await;
            match joined {
                Ok(Ok(())) => match observe_group(*group_id) {
                    Ok(false) => ready.push(*id),
                    Ok(true) | Err(_) => {}
                },
                _ => {}
            }
        }
        if !ready.is_empty() {
            let mut table = table();
            for id in ready {
                table.slots.remove(&id);
            }
        }
        if cohort.is_none() {
            let remaining = budget.saturating_sub(start.elapsed());
            if join_retained_tasks(remaining.min(Duration::from_millis(50)))
                .await
                .is_err()
                && start.elapsed() >= budget
            {
                return Err(io::Error::other(UnconfirmedCleanup::GroupTermination));
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

pub async fn observe_retained_until_quiescent(budget: Duration) -> io::Result<()> {
    observe_ids(None, budget).await
}

pub async fn observe_owned_cohort(cohort: &[u64], budget: Duration) -> io::Result<()> {
    observe_ids(Some(cohort), budget).await
}

fn handles_mut_ref(slot: &SlotState) -> Option<&LiveHandles> {
    match slot {
        SlotState::Active(handles) | SlotState::Unresolved(handles) => Some(handles),
        SlotState::Reserved => None,
    }
}

fn kill_group(pid: Option<u32>) -> io::Result<()> {
    #[cfg(unix)]
    if let Some(pid) = pid {
        // A group was allocated for this exact child before spawn.
        if unsafe { libc::kill(-(pid as i32), libc::SIGKILL) } != 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
    }
    #[cfg(not(unix))]
    let _ = pid;
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    use crate::errors::{GitError, SvnError};

    fn stopped(pid: i32) -> bool {
        if unsafe { libc::kill(pid, 0) } != 0 {
            return true;
        }
        #[cfg(target_os = "linux")]
        {
            // A killed grandchild can remain a zombie until the container's
            // PID 1 reaps it. It cannot execute or retain open descriptors.
            if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
                return stat
                    .rsplit_once(") ")
                    .is_some_and(|(_, rest)| rest.starts_with("Z ") || rest.starts_with("X "));
            }
        }
        false
    }

    #[tokio::test]
    async fn cleanup_certainty_survives_git_svn_wrappers_with_and_without_cancel() {
        for requested_cancel in [false, true] {
            let confirmed_kind = if requested_cancel {
                io::ErrorKind::Interrupted
            } else {
                io::ErrorKind::TimedOut
            };
            for cause in [
                UnconfirmedCleanup::GroupTermination,
                UnconfirmedCleanup::OutputCompletion,
            ] {
                let reaped = if cause == UnconfirmedCleanup::OutputCompletion {
                    tokio::time::timeout(
                        Duration::ZERO,
                        std::future::pending::<io::Result<Output>>(),
                    )
                    .await
                } else {
                    Ok(Ok(std::process::Output {
                        status: std::process::ExitStatus::from_raw(0),
                        stdout: Vec::new(),
                        stderr: Vec::new(),
                    }))
                };
                let killed = if cause == UnconfirmedCleanup::GroupTermination {
                    Err(io::Error::other("kill failed"))
                } else {
                    Ok(())
                };
                let error = stop_result(killed, reaped, confirmed_kind).unwrap_err();
                assert!(cleanup_unconfirmed(&error));
                let git = GitError::IoError(io::Error::other(cause));
                let svn = SvnError::IoError(io::Error::other(cause));
                assert!(matches!(git, GitError::IoError(ref e) if cleanup_unconfirmed(e)));
                assert!(matches!(svn, SvnError::IoError(ref e) if cleanup_unconfirmed(e)));
            }
            let confirmed = stop_result(
                Ok(()),
                Ok(Ok(std::process::Output {
                    status: std::process::ExitStatus::from_raw(0),
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                })),
                confirmed_kind,
            )
            .unwrap_err();
            assert_eq!(confirmed.kind(), confirmed_kind);
            assert!(!cleanup_unconfirmed(&confirmed));
            assert_eq!(confirmed_cancelled(&confirmed), requested_cancel);
            assert!(!confirmed_cancelled(&io::Error::new(
                io::ErrorKind::Interrupted,
                "untyped I/O interruption"
            )));
        }
    }

    #[tokio::test]
    async fn cancelled_child_and_descendant_stop() {
        let temp = tempfile::tempdir().unwrap();
        let marker = temp.path().join("descendant.pid");
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg("sleep 60 & echo $! > \"$PID_FILE\"; wait")
            .env("PID_FILE", &marker);
        let signal = Arc::new(AtomicBool::new(false));
        let running = tokio::spawn({
            let signal = signal.clone();
            async move { run(command, Duration::from_secs(30), Some(&signal)).await }
        });
        let descendant: i32 = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(pid) = std::fs::read_to_string(&marker) {
                    break pid.trim().parse().unwrap();
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(unsafe { libc::kill(descendant, 0) }, 0);
        signal.store(true, Ordering::Release);
        let result = tokio::time::timeout(Duration::from_secs(7), running)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if stopped(descendant) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn staged_patch_input_cannot_block_cancellation_of_nonreader() {
        let temp = tempfile::tempdir().unwrap();
        let marker = temp.path().join("nonreader-descendant.pid");
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg("sleep 60 & echo $! > \"$PID_FILE\"; wait")
            .env("PID_FILE", &marker);
        let signal = Arc::new(AtomicBool::new(false));
        // Far larger than a normal pipe buffer. The child never reads stdin.
        let input = vec![b'x'; 2 * 1024 * 1024];
        let running = tokio::spawn({
            let signal = signal.clone();
            async move { run_with_input(command, &input, Duration::from_secs(30), Some(&signal)).await }
        });
        let descendant: i32 = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(pid) = std::fs::read_to_string(&marker) {
                    break pid.trim().parse().unwrap();
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(unsafe { libc::kill(descendant, 0) }, 0);
        signal.store(true, Ordering::Release);
        let result = tokio::time::timeout(Duration::from_secs(7), running)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if stopped(descendant) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
    }

#[cfg(all(test, unix))]
mod process_run_retention {
    use super::{cleanup_unconfirmed, classify_kill_errno, run, ConfirmedStop};
    use std::error::Error;
    use std::io::ErrorKind;
    use std::time::Duration;
    use tokio::process::Command;

    #[tokio::test]
    async fn in_group_descendant_is_unconfirmed_until_observed() {
        // The shell is the direct child and the process-group leader.
        // The background sleep does not call setsid, so it stays in that group
        // after the shell exits. It is not this process's waitable child.
        // A descendant that has left the group is outside this claim.
        let temp = tempfile::tempdir().unwrap();
        let marker = temp.path().join("in-group.pid");
        let mut command = Command::new("sh");
        command.arg("-c").arg(
            "sleep 30 >/dev/null 2>&1 & echo $! > \"$PID_FILE\"; exit 0",
        );
        command.env("PID_FILE", &marker);
        let error = run(command, Duration::from_secs(5), None)
            .await
            .expect_err("direct-child exit is not group quiescence");
        assert!(cleanup_unconfirmed(&error));
        assert!(matches!(
            error
                .get_ref()
                .and_then(|source| source.downcast_ref::<crate::process::UnconfirmedCleanup>()),
            Some(crate::process::UnconfirmedCleanup::GroupTermination)
        ));
        assert!(crate::process::retained_child_and_readers_held());
        assert!(crate::process::retained_ownership_incomplete());
        let descendant: i32 = std::fs::read_to_string(&marker).unwrap().trim().parse().unwrap();
        assert_eq!(unsafe { libc::kill(descendant, 0) }, 0);
        let mut status = 0;
        let waited = unsafe { libc::waitpid(descendant, &mut status, libc::WNOHANG) };
        assert_eq!(waited, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
        #[cfg(target_os = "linux")]
        {
            let stat = std::fs::read_to_string(format!("/proc/{descendant}/stat")).unwrap();
            let rest = stat.rsplit_once(") ").unwrap().1;
            let mut fields = rest.split_whitespace();
            let _state = fields.next().unwrap();
            let _ppid = fields.next().unwrap();
            let pgrp: i32 = fields.next().unwrap().parse().unwrap();
            let session: i32 = fields.next().unwrap().parse().unwrap();
            assert_eq!(pgrp, crate::process::retained_group_id());
            assert_ne!(session, descendant);
        }
        let blocked = run(Command::new("true"), Duration::from_secs(2), None).await;
        assert!(blocked.is_err());
        assert_eq!(unsafe { libc::kill(descendant, libc::SIGKILL) }, 0);
        crate::process::observe_retained_until_quiescent(Duration::from_secs(5))
            .await
            .expect("in-group descendant reached a real stop");
        assert!(!crate::process::retained_child_and_readers_held());
        assert!(!crate::process::retained_ownership_incomplete());
        assert!(run(Command::new("true"), Duration::from_secs(2), None)
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn sleep_timeout_with_successful_kill_and_reap_is_confirmed_stop() {
        let mut command = Command::new("sleep");
        command.arg("30");
        let error = run(command, Duration::from_millis(200), None)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::TimedOut);
        assert!(!cleanup_unconfirmed(&error));
        assert!(error
            .get_ref()
            .is_some_and(|source| source.is::<ConfirmedStop>()));
        assert!(!crate::process::retained_ownership_incomplete());
        assert!(!crate::process::retained_child_and_readers_held());
        assert!(run(Command::new("true"), Duration::from_secs(2), None)
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn ordinary_native_calls_keep_independent_reservations() {
        let first = tokio::spawn(async {
            run(Command::new("true"), Duration::from_secs(2), None).await
        });
        let second = tokio::spawn(async {
            run(Command::new("true"), Duration::from_secs(2), None).await
        });
        assert!(first.await.unwrap().is_ok());
        assert!(second.await.unwrap().is_ok());
        assert!(!crate::process::retained_ownership_incomplete());
        let denied = classify_kill_errno(-1, Some(libc::EPERM)).unwrap_err();
        assert!(cleanup_unconfirmed(&denied));
        assert_eq!(classify_kill_errno(-1, Some(libc::ESRCH)).unwrap(), false);
        assert_eq!(classify_kill_errno(0, None).unwrap(), true);
    }

    #[cfg(not(target_os = "linux"))]
    #[tokio::test]
    async fn unknown_group_observation_fails_closed() {
        let error = super::observe_group(1).expect_err("non-Linux observation is unknown");
        assert!(crate::process::cleanup_unconfirmed(&error));
    }

    #[tokio::test]
    async fn concurrent_collectors_cannot_qualify_before_owned_batches_finish() {
        let (gate, release) = tokio::sync::oneshot::channel::<()>();
        let handle = tokio::spawn(async move {
            let _ = release.await;
        });
        super::retain_unjoined_task(handle);
        let first = tokio::spawn(super::join_retained_tasks(Duration::from_secs(2)));
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if super::collector_is_active() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("collector A owns the in-flight batch");
        let second = super::join_retained_tasks(Duration::from_millis(50)).await;
        assert!(second.is_err());
        assert!(super::collector_is_active());
        assert!(crate::process::retained_ownership_incomplete());
        let _ = gate.send(());
        let finished = tokio::time::timeout(Duration::from_secs(2), first)
            .await
            .expect("collector A finishes its own batch")
            .unwrap();
        assert!(finished.is_ok());
        assert!(!super::collector_is_active());
        assert!(!crate::process::retained_ownership_incomplete());
    }
}
}
