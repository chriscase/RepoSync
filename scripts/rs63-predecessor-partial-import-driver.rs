// Test-only tail for a temporary 54e18348 archive. Append after the exact
// 133,430-byte crates/web/tests/server_freeze.rs prefix. Stripping this tail
// recovers blob 82e2f8e1dfa8c8b4d550a6c94d1ff8461644e41c. This file is not a
// current production binary and is not compiled by the current workspace.

use std::io::Read;
// The 133,430-byte server_freeze.rs prefix already imports std::path::Path.
use std::path::PathBuf;
use std::process::Command;

const REVISIONS: i64 = 51;
const ATTEMPT_ROOT_ENV: &str = "REPOSYNC_RS63_ATTEMPT_ROOT";
const ATTEMPT_ID_ENV: &str = "REPOSYNC_RS63_ATTEMPT_ID";
const PRESERVED_FAILURE_ROOTS: &[&str] = &[
    "/tmp/rs63-predecessor-partial-import-hold",
    "/private/tmp/rs63-predecessor-partial-import-hold",
    "/tmp/.tmpRJUzYe",
    "/private/tmp/.tmpRJUzYe",
];

fn run_ok(cmd: &mut Command) -> std::process::Output {
    let output = cmd.output().unwrap_or_else(|error| panic!("{cmd:?}: {error}"));
    assert!(
        output.status.success(),
        "{cmd:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn history_body(revision: i64) -> String {
    format!("revision-{revision}\n")
}

fn create_fifty_one_revisions(root: &Path) -> PathBuf {
    let svn_repo = root.join("svn-repo");
    run_ok(Command::new("svnadmin").args(["create", svn_repo.to_str().unwrap()]));
    let seed = root.join("seed");
    std::fs::create_dir_all(seed.join("trunk")).unwrap();
    std::fs::write(seed.join("trunk").join("history.txt"), history_body(1)).unwrap();
    let svn_url = format!("file://{}", svn_repo.display());
    run_ok(Command::new("svn").args([
        "import",
        seed.join("trunk").to_str().unwrap(),
        &format!("{svn_url}/trunk"),
        "-m",
        "revision 1",
        "--username",
        "fixture",
        "--non-interactive",
    ]));
    let checkout = root.join("svn-wc");
    run_ok(Command::new("svn").args([
        "checkout",
        &format!("{svn_url}/trunk"),
        checkout.to_str().unwrap(),
        "--username",
        "fixture",
        "--non-interactive",
    ]));
    for revision in 2..=REVISIONS {
        std::fs::write(checkout.join("history.txt"), history_body(revision)).unwrap();
        run_ok(Command::new("svn").args([
            "commit",
            "-m",
            &format!("revision {revision}"),
            "--username",
            "fixture",
            "--non-interactive",
        ]).current_dir(&checkout));
    }
    let youngest = run_ok(Command::new("svnlook").args(["youngest", svn_repo.to_str().unwrap()]));
    assert_eq!(String::from_utf8_lossy(&youngest.stdout).trim(), "51");
    for revision in 1..=REVISIONS {
        let body = run_ok(Command::new("svnlook").args([
            "cat",
            "-r",
            &revision.to_string(),
            svn_repo.to_str().unwrap(),
            "trunk/history.txt",
        ]));
        let text = String::from_utf8(body.stdout).unwrap();
        assert_eq!(text, history_body(revision), "r{revision} is not an eligible nonempty revision");
    }
    svn_repo
}

fn sha256_bytes(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

fn sha256_file(path: &Path) -> (u64, String) {
    // The historical prefix scopes Digest inside a different function.
    use sha2::Digest;
    let mut file = std::fs::File::open(path).unwrap();
    let mut hasher = sha2::Sha256::new();
    let mut buf = [0_u8; 64 * 1024];
    let mut bytes = 0_u64;
    loop {
        let read = file.read(&mut buf).unwrap();
        if read == 0 {
            break;
        }
        bytes += read as u64;
        hasher.update(&buf[..read]);
    }
    (bytes, hex::encode(hasher.finalize()))
}

fn unix_metadata_mode(meta: &std::fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.mode()
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        panic!("predecessor partial-import member modes require Unix metadata");
    }
}

fn seal_members(root: &Path) -> Vec<serde_json::Value> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<serde_json::Value>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap())
            .collect();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let meta = std::fs::symlink_metadata(&path).unwrap();
            let rel = path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            let mode = unix_metadata_mode(&meta);
            if meta.file_type().is_symlink() {
                let target = std::fs::read_link(&path).unwrap();
                let encoded = target.to_string_lossy();
                out.push(serde_json::json!({
                    "path": rel,
                    "kind": "symlink",
                    "bytes": encoded.len(),
                    "mode": mode,
                    "sha256": sha256_bytes(encoded.as_bytes()),
                    "link": encoded,
                }));
            } else if meta.is_dir() {
                out.push(serde_json::json!({
                    "path": rel,
                    "kind": "dir",
                    "bytes": 0,
                    "mode": mode,
                    "sha256": sha256_bytes(b""),
                }));
                walk(root, &path, out);
            } else {
                let (bytes, sha256) = sha256_file(&path);
                out.push(serde_json::json!({
                    "path": rel,
                    "kind": "file",
                    "bytes": bytes,
                    "mode": mode,
                    "sha256": sha256,
                }));
            }
        }
    }
    let mut members = Vec::new();
    walk(root, root, &mut members);
    members
}

fn same_or_nested(left: &Path, right: &Path) -> bool {
    left == right || left.starts_with(right) || right.starts_with(left)
}

fn touches_preserved(path: &Path) -> Result<(), String> {
    for raw in PRESERVED_FAILURE_ROOTS {
        let preserved = Path::new(raw);
        if same_or_nested(path, preserved) {
            return Err(format!("attempt root touches preserved failure {raw}"));
        }
        if let Ok(meta) = std::fs::symlink_metadata(preserved) {
            if meta.is_dir() && !meta.file_type().is_symlink() {
                if let Ok(canonical) = preserved.canonicalize() {
                    if same_or_nested(path, &canonical) {
                        return Err(format!(
                            "attempt root touches preserved failure {}",
                            canonical.display()
                        ));
                    }
                }
            }
        }
    }
    if path.starts_with("/home") || path.starts_with("/Users") {
        return Err("attempt root is an unproved user location".into());
    }
    if let Ok(home) = std::env::var("HOME") {
        let home = PathBuf::from(home);
        if path.starts_with(&home) {
            return Err("attempt root is an unproved user location".into());
        }
        if let Ok(meta) = std::fs::symlink_metadata(&home) {
            if meta.is_dir() && !meta.file_type().is_symlink() {
                if let Ok(canonical) = home.canonicalize() {
                    if path.starts_with(&canonical) {
                        return Err("attempt root is an unproved user location".into());
                    }
                }
            }
        }
    }
    Ok(())
}

fn exclusive_attempt_leaf(requested: &Path) -> Result<PathBuf, String> {
    if !requested.is_absolute() {
        return Err("attempt root is not absolute".into());
    }
    if requested.components().any(|component| {
        matches!(
            component,
            std::path::Component::ParentDir | std::path::Component::CurDir
        )
    }) {
        return Err("attempt root contains . or ..".into());
    }
    touches_preserved(requested)?;
    let parent = requested
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or("attempt root has no parent")?;
    let parent_meta = std::fs::symlink_metadata(parent)
        .map_err(|error| format!("attempt parent: {error}"))?;
    if !parent_meta.is_dir() || parent_meta.file_type().is_symlink() {
        return Err("attempt parent is not a real owned directory".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let euid = unsafe { libc::geteuid() };
        if parent_meta.uid() != euid {
            return Err("attempt parent is not owned by this process".into());
        }
    }
    #[cfg(not(unix))]
    {
        return Err("attempt root ownership requires Unix metadata".into());
    }
    match std::fs::symlink_metadata(requested) {
        Ok(meta) => {
            let kind = if meta.file_type().is_symlink() {
                "symlink"
            } else if meta.is_dir() {
                "directory"
            } else if meta.is_file() {
                "file"
            } else {
                "other"
            };
            return Err(format!("attempt root collision: existing {kind}"));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("attempt root: {error}")),
    }
    let canonical_parent = parent
        .canonicalize()
        .map_err(|error| format!("attempt parent: {error}"))?;
    let leaf = requested
        .file_name()
        .ok_or("attempt root has no leaf name")?;
    let canonical = canonical_parent.join(leaf);
    touches_preserved(&canonical)?;
    std::fs::create_dir(&canonical)
        .map_err(|error| format!("exclusive attempt create: {error}"))?;
    Ok(canonical)
}

fn supplied_attempt(root: Option<&str>, attempt_id: Option<&str>) -> Result<(String, PathBuf), String> {
    let attempt_id = attempt_id
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("missing {ATTEMPT_ID_ENV}"))?
        .to_string();
    if attempt_id.contains('/') || attempt_id.contains('\\') || attempt_id.contains('\0') {
        return Err("attempt id is not a single path component".into());
    }
    let root = root.ok_or_else(|| format!("missing {ATTEMPT_ROOT_ENV}"))?;
    let canonical = exclusive_attempt_leaf(Path::new(root))?;
    Ok((attempt_id, canonical))
}

fn require_owner_attempt() -> Result<(String, PathBuf), String> {
    let root = std::env::var(ATTEMPT_ROOT_ENV).ok();
    let attempt_id = std::env::var(ATTEMPT_ID_ENV).ok();
    supplied_attempt(root.as_deref(), attempt_id.as_deref())
}

fn copy_permissions(from: &Path, to: &Path) -> Result<(), String> {
    let meta = std::fs::symlink_metadata(from)
        .map_err(|error| format!("mode source {}: {error}", from.display()))?;
    std::fs::set_permissions(to, meta.permissions())
        .map_err(|error| format!("mode copy {}: {error}", to.display()))
}

fn copy_tree_independent(src: &Path, dst: &Path) -> Result<(), String> {
    if std::fs::symlink_metadata(dst).is_ok() {
        return Err(format!("snapshot destination already exists {}", dst.display()));
    }
    std::fs::create_dir(dst).map_err(|error| format!("snapshot create {}: {error}", dst.display()))?;
    copy_permissions(src, dst)?;
    copy_children_independent(src, dst)?;
    assert_distinct_file_inodes(src, dst)?;
    Ok(())
}

fn copy_children_independent(src: &Path, dst: &Path) -> Result<(), String> {
    let mut entries = std::fs::read_dir(src)
        .map_err(|error| format!("read {}: {error}", src.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("entry: {error}"))?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let from = entry.path();
        let name = entry.file_name();
        let into = dst.join(&name);
        let meta = std::fs::symlink_metadata(&from)
            .map_err(|error| format!("{}: {error}", from.display()))?;
        if meta.file_type().is_symlink() {
            let target = std::fs::read_link(&from)
                .map_err(|error| format!("link {}: {error}", from.display()))?;
            #[cfg(unix)]
            {
                std::os::unix::fs::symlink(&target, &into)
                    .map_err(|error| format!("symlink {}: {error}", into.display()))?;
            }
            #[cfg(not(unix))]
            {
                let _ = target;
                return Err("symlink snapshots require Unix".into());
            }
        } else if meta.is_dir() {
            std::fs::create_dir(&into)
                .map_err(|error| format!("dir {}: {error}", into.display()))?;
            copy_permissions(&from, &into)?;
            copy_children_independent(&from, &into)?;
        } else if meta.is_file() {
            std::fs::copy(&from, &into)
                .map_err(|error| format!("file {}: {error}", into.display()))?;
        } else {
            return Err(format!("unsupported member {}", from.display()));
        }
    }
    Ok(())
}

fn assert_distinct_file_inodes(left: &Path, right: &Path) -> Result<(), String> {
    #[cfg(not(unix))]
    {
        let _ = (left, right);
        return Err("inode independence requires Unix metadata".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        fn visit(base: &Path, dir: &Path, other_root: &Path) -> Result<(), String> {
            let mut entries = std::fs::read_dir(dir)
                .map_err(|error| format!("inode walk {}: {error}", dir.display()))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| format!("inode entry: {error}"))?;
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                let path = entry.path();
                let meta = std::fs::symlink_metadata(&path)
                    .map_err(|error| format!("{}: {error}", path.display()))?;
                let rel = path
                    .strip_prefix(base)
                    .map_err(|_| format!("inode path escaped {}", path.display()))?;
                if meta.file_type().is_symlink() {
                    continue;
                }
                if meta.is_dir() {
                    visit(base, &path, other_root)?;
                    continue;
                }
                if !meta.is_file() {
                    continue;
                }
                let other = other_root.join(rel);
                let other_meta = match std::fs::symlink_metadata(&other) {
                    Ok(meta) => meta,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(format!("{}: {error}", other.display())),
                };
                if other_meta.is_file()
                    && meta.dev() == other_meta.dev()
                    && meta.ino() == other_meta.ino()
                {
                    return Err(format!("inode alias {}", rel.display()));
                }
            }
            Ok(())
        }
        visit(left, left, right)
    }
}

fn members_match(label: &str, root: &Path, expected: &[serde_json::Value]) -> Result<(), String> {
    let actual = seal_members(root);
    let mut expected_map = std::collections::BTreeMap::new();
    for member in expected {
        let path = member["path"]
            .as_str()
            .ok_or_else(|| format!("{label} member path"))?;
        if expected_map.insert(path, member).is_some() {
            return Err(format!("{label} duplicate member {path}"));
        }
    }
    let mut actual_map = std::collections::BTreeMap::new();
    for member in &actual {
        let path = member["path"]
            .as_str()
            .ok_or_else(|| format!("{label} actual path"))?;
        actual_map.insert(path, member);
    }
    for (path, member) in &expected_map {
        let found = actual_map
            .get(path)
            .ok_or_else(|| format!("{label} missing {path}"))?;
        if *found != *member {
            return Err(format!("{label} changed {path}"));
        }
    }
    for path in actual_map.keys() {
        if !expected_map.contains_key(path) {
            return Err(format!("{label} extra {path}"));
        }
    }
    Ok(())
}

fn seal_raw_snapshots(
    live: &Path,
    master: &Path,
    snapshot: &Path,
) -> Result<Vec<serde_json::Value>, String> {
    let members = seal_members(live);
    copy_tree_independent(live, master)?;
    copy_tree_independent(live, snapshot)?;
    members_match("master", master, &members)?;
    members_match("snapshot", snapshot, &members)?;
    members_match("live", live, &members)?;
    assert_distinct_file_inodes(live, master)?;
    assert_distinct_file_inodes(live, snapshot)?;
    assert_distinct_file_inodes(master, snapshot)?;
    Ok(members)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rs63_capture_predecessor_partial_import() {
    use std::sync::Arc;

    let (attempt_id, attempt_root) = require_owner_attempt().expect("owner attempt root");
    let live = attempt_root.join("live");
    std::fs::create_dir(&live).expect("exclusive live subtree");
    std::fs::create_dir(attempt_root.join("sealed")).expect("exclusive sealed directory");
    std::fs::create_dir(attempt_root.join("parked")).expect("exclusive parked directory");
    let root = live.canonicalize().expect("canonical live root");
    let svn_repo = create_fifty_one_revisions(&root);
    let bare = root.join("local").join("history.git");
    std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
    run_ok(Command::new("git").args(["init", "--bare", bare.to_str().unwrap()]));
    let observed_uuid = String::from_utf8(
        run_ok(Command::new("svnlook").args(["uuid", svn_repo.to_str().unwrap()])).stdout,
    )
    .unwrap();
    let observed_uuid = observed_uuid.trim().to_string();
    assert!(!observed_uuid.is_empty());

    let config = minimal_config(&root);
    let db_path = root.join("reposync.db");
    let db = Database::new(&db_path).unwrap();
    db.initialize().unwrap();
    let engine_db = Database::in_memory().unwrap();
    engine_db.initialize().unwrap();
    let dummy_git = root.join("dummy-git");
    git2::Repository::init(&dummy_git).unwrap();
    let engine = Arc::new(SyncEngine::new(
        config.clone(),
        engine_db,
        SvnClient::new("file:///nonexistent", "fixture", ""),
        GitClient::new(&dummy_git).unwrap(),
        Arc::new(IdentityMapper::new(&IdentityConfig::default()).unwrap()),
    ));
    let (sync_tx, _) = tokio::sync::mpsc::channel(1);
    let (ws_tx, _) = tokio::sync::broadcast::channel(32);
    let state = Arc::new(AppState {
        db,
        sync_engine: engine,
        config,
        sync_trigger: sync_tx,
        ws_broadcast: ws_tx,
        sessions: tokio::sync::RwLock::new(HashMap::new()),
        import_progress: Arc::new(tokio::sync::RwLock::new(ImportProgress::default())),
        config_path: root.join("config.toml"),
        prev_net_snapshot: std::sync::Mutex::new(None),
        repo_import_progress: tokio::sync::RwLock::new(HashMap::new()),
        login_attempts: std::sync::Mutex::new(HashMap::new()),
        import_handles: tokio::sync::Mutex::new(Vec::new()),
    });
    std::fs::write(
        &state.config_path,
        format!(
            "[daemon]\ndata_dir = \"{}\"\n",
            root.display().to_string().replace('\\', "/")
        ),
    )
    .unwrap();
    state.sessions.write().await.insert(
        TEST_TOKEN.into(),
        chrono::Utc::now() + chrono::Duration::hours(1),
    );
    let app = Router::new()
        .merge(api::repos::routes())
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let svn_url = format!("file://{}", svn_repo.display());
    let client = authed_client();
    let created = client
        .post(format!("http://{addr}/api/repos"))
        .json(&serde_json::json!({
            "name": "history",
            "svn_url": svn_url,
            "svn_branch": "trunk",
            "svn_username": "fixture",
            "git_provider": "gitea",
            "git_api_url": format!("file://{}", root.display()),
            "git_repo": "local/history",
            "git_branch": "main",
        }))
        .send()
        .await
        .unwrap();
    assert!(created.status().is_success(), "{}", created.text().await.unwrap());
    let created: serde_json::Value = created.json().await.unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    let fixture_root = root.canonicalize().unwrap();
    std::env::set_var("REPOSYNC_FIXTURE_ROOT", &fixture_root);
    std::env::set_var("REPOSYNC_IMPORT_LOST_PUSH_REPLY", &id);
    let started = client
        .post(format!("http://{addr}/api/repos/{id}/import"))
        .header("x-request-id", "rs63-predecessor-partial")
        .send()
        .await
        .unwrap();
    assert!(started.status().is_success(), "{}", started.text().await.unwrap());
    let started: serde_json::Value = started.json().await.unwrap();
    let op_id = started["operation_id"].as_str().unwrap().to_string();
    let mut final_status = serde_json::Value::Null;
    for _ in 0..900 {
        final_status = client
            .get(format!("http://{addr}/api/repos/{id}/import/status"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let lifecycle = final_status["lifecycle"].as_str().unwrap_or("");
        if lifecycle == "reconciliation_required" || lifecycle == "failed" || lifecycle == "completed" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    assert_eq!(final_status["lifecycle"], "reconciliation_required", "{final_status}");
    let handles: Vec<_> = std::mem::take(&mut *state.import_handles.lock().await);
    for handle in handles {
        tokio::time::timeout(std::time::Duration::from_secs(60), handle)
            .await
            .expect("import worker did not retire")
            .expect("import worker join");
    }
    let op = state.db.get_import_operation(&id, &op_id).unwrap().unwrap();
    assert_eq!(op.processed_revisions, 50, "{op:?}");
    assert_eq!(op.total_revisions, Some(51));
    assert_eq!(op.local_commits, 50);
    assert_eq!(op.intended_ref.as_deref(), Some("refs/heads/main"));
    let intended_sha = op.intended_git_sha.clone().expect("lost-reply publication intent");
    assert_eq!(intended_sha.len(), 40);
    assert_eq!(op.last_local_git_sha.as_deref(), Some(intended_sha.as_str()));
    assert!(op.last_confirmed_git_sha.is_none());
    let (watermark_rev, watermark_sha) = state.db.get_repo_watermark(&id).unwrap();
    assert_eq!(watermark_rev, 0);
    assert!(watermark_sha.is_empty());
    let schema_version: i64 = state
        .db
        .conn()
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(schema_version, 12);
    let workdir = state
        .config
        .daemon
        .data_dir
        .join("repos")
        .join(&id)
        .join("git-repo");
    let local_tip = run_ok(Command::new("git").args(["-C", workdir.to_str().unwrap(), "rev-parse", "HEAD"]));
    assert_eq!(String::from_utf8_lossy(&local_tip.stdout).trim(), intended_sha);
    let local_count = run_ok(Command::new("git").args([
        "-C",
        workdir.to_str().unwrap(),
        "rev-list",
        "--count",
        "HEAD",
    ]));
    assert_eq!(String::from_utf8_lossy(&local_count.stdout).trim(), "50");
    let remote_tip = run_ok(Command::new("git").args([
        "--git-dir",
        bare.to_str().unwrap(),
        "rev-parse",
        "refs/heads/main",
    ]));
    assert_eq!(String::from_utf8_lossy(&remote_tip.stdout).trim(), intended_sha);
    let remote_count = run_ok(Command::new("git").args([
        "--git-dir",
        bare.to_str().unwrap(),
        "rev-list",
        "--count",
        "refs/heads/main",
    ]));
    assert_eq!(String::from_utf8_lossy(&remote_count.stdout).trim(), "50");
    let tree = run_ok(Command::new("git").args([
        "-C",
        workdir.to_str().unwrap(),
        "rev-parse",
        "HEAD^{tree}",
    ]));
    let tree = String::from_utf8(tree.stdout).unwrap().trim().to_string();
    let file = run_ok(Command::new("git").args([
        "-C",
        workdir.to_str().unwrap(),
        "show",
        "HEAD:history.txt",
    ]));
    assert_eq!(file.stdout, history_body(50).as_bytes());
    let unprocessed = run_ok(Command::new("svnlook").args([
        "cat",
        "-r",
        "51",
        svn_repo.to_str().unwrap(),
        "trunk/history.txt",
    ]));
    assert_eq!(unprocessed.stdout, history_body(51).as_bytes());
    assert_eq!(op.last_local_svn_rev, Some(50));
    let fingerprint = op.target_fingerprint.clone();
    assert!(!fingerprint.is_empty());

    server.abort();
    let _ = server.await;
    drop(state);
    std::env::remove_var("REPOSYNC_IMPORT_LOST_PUSH_REPLY");
    std::env::remove_var("REPOSYNC_FIXTURE_ROOT");

    let master = attempt_root.join("sealed").join("master");
    let snapshot = attempt_root.join("sealed").join("snapshot");
    let members = seal_raw_snapshots(&root, &master, &snapshot).expect("raw double seal");
    let manifest = serde_json::json!({
        "profile": "RS63_PREDECESSOR_PARTIAL_IMPORT_HOLD",
        "producer_commit": "54e183484961bd9ad12bd03505b3b00610de7cd9",
        "attempt_id": attempt_id,
        "attempt_root": attempt_root,
        "schema_version": schema_version,
        "repo_id": id,
        "operation_id": op_id,
        "root": root,
        "master": master,
        "snapshot": snapshot,
        "svn_url": svn_url,
        "svn_repo": svn_repo,
        "bare": bare,
        "workdir": workdir,
        "db": db_path,
        "data_dir": root,
        "fingerprint": fingerprint,
        "intended_ref": "refs/heads/main",
        "intended_sha": intended_sha,
        "tree": tree,
        "processed_revisions": 50,
        "total_revisions": 51,
        "local_commits": 50,
        "last_local_svn_rev": 50,
        "observed_svn_uuid": observed_uuid,
        "members": members,
    });
    let manifest_path = attempt_root.join("manifest.json");
    let mut manifest_file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&manifest_path)
        .expect("exclusive manifest");
    std::io::Write::write_all(
        &mut manifest_file,
        &serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    drop(manifest_file);
    members_match("master after manifest", &master, manifest["members"].as_array().unwrap())
        .expect("master changed while writing the manifest");
    members_match(
        "snapshot after manifest",
        &snapshot,
        manifest["members"].as_array().unwrap(),
    )
    .expect("snapshot changed while writing the manifest");
    eprintln!("RS63 capture sealed at {}", root.display());
}

fn canary_parent(label: &str) -> PathBuf {
    let parent = std::env::temp_dir().join(format!(
        "rs63-canary-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&parent).unwrap();
    parent
}

#[test]
fn rs63_attempt_root_refuses_missing_owner_input() {
    let error = supplied_attempt(None, None).unwrap_err();
    assert!(error.contains("missing REPOSYNC_RS63_ATTEMPT_ID"), "{error}");
}

#[test]
fn rs63_attempt_root_refuses_existing_directory() {
    let parent = canary_parent("dir");
    let leaf = parent.join("attempt");
    std::fs::create_dir(&leaf).unwrap();
    let error = exclusive_attempt_leaf(&leaf).unwrap_err();
    assert!(error.contains("existing directory"), "{error}");
    assert!(leaf.is_dir());
    std::fs::remove_dir_all(&parent).unwrap();
}

#[test]
fn rs63_attempt_root_refuses_regular_file() {
    let parent = canary_parent("file");
    let leaf = parent.join("attempt");
    std::fs::write(&leaf, b"occupied\n").unwrap();
    let error = exclusive_attempt_leaf(&leaf).unwrap_err();
    assert!(error.contains("existing file"), "{error}");
    assert_eq!(std::fs::read(&leaf).unwrap(), b"occupied\n");
    std::fs::remove_dir_all(&parent).unwrap();
}

#[cfg(unix)]
#[test]
fn rs63_attempt_root_refuses_dangling_symlink() {
    let parent = canary_parent("symlink");
    let leaf = parent.join("attempt");
    std::os::unix::fs::symlink("/tmp/rs63-canary-dangling-target", &leaf).unwrap();
    let error = exclusive_attempt_leaf(&leaf).unwrap_err();
    assert!(error.contains("existing symlink"), "{error}");
    assert!(std::fs::symlink_metadata(&leaf).unwrap().file_type().is_symlink());
    std::fs::remove_dir_all(&parent).unwrap();
}

#[test]
fn rs63_attempt_root_refuses_preserved_failure_descendant() {
    let preserved = Path::new("/tmp/rs63-predecessor-partial-import-hold");
    let leaf = preserved.join("rs63-must-not-create");
    let error = exclusive_attempt_leaf(&leaf).unwrap_err();
    assert!(error.contains("preserved failure"), "{error}");
    assert!(std::fs::symlink_metadata(&leaf).is_err());
}
