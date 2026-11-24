//! R23 using the real daemon interval handler and its registered worker.
use super::*;
use reposync_core::config::IdentityConfig;
use reposync_core::db::git_push_operations::{GitPushOperation, GitPushOperationState};
use reposync_core::db::lineage_checkpoints::{
    load_canonical_lineage, load_directional_checkpoint, CheckpointDirection,
};
use serde_json::{json, Value};
use std::path::Path;
use std::process::Command;

const CASE: &str = "R23_SCHEDULER_RESTORED_DATABASE";
const REPO: &str = "r23-interval-fixture";
const DUE: &str = "2000-01-01T00:00:00Z";
const MAIN: &str = "refs/heads/main";
const SVN_AUTHOR: &str = "fixture-author";
const TEST_NAME: &str =
    "scheduler::r23_restored_database::restored_prewrite_database_holds_on_actual_interval_tick";
const CHILD_ROOT: &str = "REPOSYNC_R23_CLEAN_CHILD_ROOT";
const EVIDENCE_PREFIX: &str = "RELIABILITY_EVIDENCE ";
const SOURCE_MESSAGE: &str = "Independent SVN work after the checkpoint";
const SOURCE_BYTES: &[u8] = b"SVN r3 from a separate working copy\n";

fn command(dir: &Path, binary: &str, args: &[&str]) -> Vec<u8> {
    let out = Command::new(binary)
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("{binary} is required for {CASE}: {error}"));
    assert!(
        out.status.success(),
        "{binary} {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

fn text_out(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes)
        .unwrap()
        .trim_end_matches(['\r', '\n'])
        .to_owned()
}

fn git(path: &Path, args: &[&str]) -> String {
    text_out(command(path, "git", args))
}

fn svnlook(dir: &Path, verb: &str, args: &[&str]) -> Vec<u8> {
    let mut all = vec![verb, dir.to_str().unwrap()];
    all.extend_from_slice(args);
    command(dir, "svnlook", &all)
}

fn make_due(db: &Database) {
    assert_eq!(
        db.conn()
            .execute(
                "UPDATE repositories SET last_sync_at = ?1 WHERE id = ?2",
                [DUE, REPO],
            )
            .unwrap(),
        1
    );
}

fn verify_fixture_targets(scheduler: &Scheduler, svn: &Path, bare: &Path, bridge: &Path) {
    let repo = scheduler.db.get_repository(REPO).unwrap().unwrap();
    let svn_path = svn.to_str().unwrap();
    let git_path = bare.to_str().unwrap();
    assert_eq!(svn.canonicalize().unwrap(), svn);
    assert_eq!(bare.canonicalize().unwrap(), bare);
    assert_eq!(repo.svn_url, format!("file://{svn_path}"));
    assert!(repo.svn_branch.is_empty());
    assert_eq!(repo.git_provider, "local");
    assert!(repo.git_api_url.is_empty());
    assert_eq!(repo.git_repo, git_path);
    assert_eq!(repo.git_branch, "main");
    assert!(repo.teams_webhook_url.is_none());
    assert!(scheduler.db.resolve_credential_chain(REPO, "secret_git_token").is_none());
    assert!(scheduler.db.resolve_credential_chain(REPO, "secret_svn_password").is_none());
    let expected_bridge = scheduler.app_config.daemon.data_dir.join("repos").join(REPO).join("git-repo");
    assert_eq!(bridge, expected_bridge.as_path());
    assert_eq!(bridge.canonicalize().unwrap().as_path(), bridge);

    // Enumerate effective URLs rather than a raw config entry or first match.
    // Reject aliases, remote helpers, duplicate destinations, and URLs that
    // Git rewrites to a second target. The environment permits file only.
    assert_eq!(git(bridge, &["remote"]), "origin");
    for args in [
        &["remote", "get-url", "--all", "origin"][..],
        &["remote", "get-url", "--push", "--all", "origin"][..],
    ] {
        let effective = String::from_utf8(command(bridge, "git", args)).unwrap();
        let urls: Vec<_> = effective.lines().collect();
        assert_eq!(urls.as_slice(), &[git_path], "expected one literal local URL");
    }
}

/// Enter the same interval path as Scheduler::run and wait for exactly its
/// worker. An unchanged database alone could also mean we never scheduled it.
async fn tick_and_join(
    scheduler: &Scheduler, due_at: &mut time::Instant, svn: &Path, bare: &Path, bridge: &Path,
) -> Value {
    verify_fixture_targets(scheduler, svn, bare, bridge);
    let tick = scheduler.stats.poll_ticks.load(Ordering::SeqCst);
    let spawns = scheduler.sync_task_spawns.load(Ordering::SeqCst);
    let mut events = scheduler.ws_broadcast.subscribe();
    time::timeout(Duration::from_secs(30), scheduler.handle_interval_tick(due_at))
        .await
        .expect("interval tick timed out");
    assert_eq!(scheduler.stats.poll_ticks.load(Ordering::SeqCst), tick + 1);
    assert_eq!(scheduler.sync_task_spawns.load(Ordering::SeqCst), spawns + 1);
    let mut handle = {
        let mut handles = scheduler.sync_handles.lock().await;
        assert_eq!(handles.len(), 1, "expected one admitted scheduler worker");
        handles.pop().unwrap()
    };
    match time::timeout(Duration::from_secs(30), &mut handle).await {
        Ok(result) => result.expect("scheduler worker panicked"),
        Err(_) => {
            handle.abort();
            panic!("scheduler worker timed out");
        }
    };
    let event: Value = serde_json::from_str(&events.try_recv().expect("worker emitted no result"))
        .unwrap();
    assert_eq!(event["repo_id"], REPO);
    assert!(matches!(
        events.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
    event
}

fn assert_completion(event: Value, svn_to_git: u64) {
    assert_eq!(event["type"], "repo_sync_completed", "{event}");
    assert_eq!(event["svn_to_git"], svn_to_git);
    assert_eq!(event["git_to_svn"], 0);
    assert_eq!(event["conflicts"], 0);
}

fn publication(db: &Database, revision: i64, bare: &Path) -> GitPushOperation {
    let op = db.latest_git_push_operation(REPO).unwrap().unwrap();
    assert_eq!(op.repo_id, REPO);
    assert_eq!(op.source_svn_rev, revision);
    assert_eq!(op.state, GitPushOperationState::Completed);
    assert_eq!(op.initiator_id, "team_worker");
    assert_eq!(op.pre_push_git_branch, "main");
    let tip = git(bare, &["rev-parse", MAIN]);
    let tree = git(bare, &["rev-parse", "refs/heads/main^{tree}"]);
    assert_eq!(op.intended_local_git_sha, tip);
    assert_eq!(op.last_confirmed_git_sha.as_deref(), Some(tip.as_str()));
    assert_eq!(op.intended_local_git_tree, tree);
    assert_eq!(op.last_confirmed_git_tree.as_deref(), Some(tree.as_str()));
    assert_eq!(db.get_repo_watermark(REPO).unwrap(), (revision, tip));
    assert!(db.active_git_push_operation(REPO).unwrap().is_none());
    op
}

fn ledger(db: &Database) -> Value {
    let lineage = load_canonical_lineage(db, REPO).unwrap().unwrap();
    let conn = db.conn();
    let rows = |sql: &str, parameters: &[&dyn rusqlite::ToSql]| {
        conn.prepare(sql)
            .unwrap()
            .query_map(parameters, |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    let checkpoints = [CheckpointDirection::SvnToGit, CheckpointDirection::GitToSvn]
        .map(|direction| {
            load_directional_checkpoint(&conn, &lineage.scope_uuid, lineage.generation, direction)
                .unwrap()
        });
    let outcome_rows = rows(
        "SELECT json_array(id, source_key, direction, outcome, target_git_sha, target_svn_rev,
          reason, policy_snapshot, created_at) FROM canonical_mapping_outcomes
          WHERE scope_uuid = ?1 AND generation = ?2 ORDER BY id",
        &[&lineage.scope_uuid, &lineage.generation],
    );
    let maps = rows(
        "SELECT json_array(id, svn_rev, git_sha, direction, synced_at, svn_author, git_author)
          FROM commit_map WHERE repo_id = ?1 ORDER BY id",
        &[&REPO],
    );
    let sync = rows(
        "SELECT json_array(id, svn_rev, git_sha, direction, status, author, message, timestamp,
          synced_at) FROM sync_records WHERE repo_id = ?1 ORDER BY id",
        &[&REPO],
    );
    let pushes = rows(
        "SELECT json_array(key, value) FROM kv_state
          WHERE key GLOB 'svn_to_git_push_v1:document:*'
          AND json_extract(value, '$.repo_id') = ?1 ORDER BY key",
        &[&REPO],
    );
    drop(conn);
    json!({
        "watermark": db.get_repo_watermark(REPO).unwrap(),
        "lineage": lineage,
        "checkpoints": checkpoints,
        "inbound_git": reposync_core::echo_receipt_scope::read_team_inbound_git_checkpoint_kv(
            db, REPO
        ).unwrap(),
        "svn_mirror": db.get_state(&format!("last_svn_rev_{REPO}")).unwrap(),
        "handled_git_baseline": db.get_state(&format!("handled_git_baseline_{REPO}")).unwrap(),
        "outcomes": outcome_rows,
        "commit_map": maps,
        "sync_records": sync,
        "push_documents": pushes,
        "latest_push": db.latest_git_push_operation(REPO).unwrap(),
        "active_push": db.active_git_push_operation(REPO).unwrap(),
        "active_svn_commit": db.active_svn_commit_operation(REPO).unwrap(),
        "active_cycle_mapping": db.active_team_cycle_mapping_operation(REPO).unwrap(),
        "blocking_conflicts": db.count_conflicts_blocking_apply_for_repo(REPO).unwrap(),
    })
}

fn remote_witness(svn: &Path, bare: &Path) -> Vec<Vec<u8>> {
    vec![
        command(bare, "git", &["rev-parse", MAIN, "refs/heads/main^{tree}"]),
        command(bare, "git", &["for-each-ref", "refs/heads"]),
        std::fs::read(bare.join("logs/refs/heads/main")).unwrap(),
        command(bare, "git", &["show", "refs/heads/main:recovery.txt"]),
        svnlook(svn, "uuid", &[]),
        svnlook(svn, "youngest", &[]),
        svnlook(svn, "tree", &["--show-ids", "--full-paths"]),
        svnlook(svn, "changed", &["-r", "3"]),
        svnlook(svn, "log", &["-r", "3"]),
        svnlook(svn, "author", &["-r", "3"]),
        svnlook(svn, "cat", &["-r", "3", "recovery.txt"]),
    ]
}

fn bridge_witness(path: &Path) -> Vec<Vec<u8>> {
    assert_eq!(git(path, &["symbolic-ref", "HEAD"]), MAIN);
    assert!(!path.join("recovery.txt").exists());
    let status = command(
        path, "git", &["--no-optional-locks", "status", "--porcelain", "--untracked-files=all"],
    );
    assert!(status.is_empty());
    vec![
        command(path, "git", &["rev-parse", "HEAD", "HEAD^{tree}"]),
        command(path, "git", &["for-each-ref", "refs/heads"]),
        std::fs::read(path.join(".git/index")).unwrap(),
        std::fs::read(path.join(".git/logs/refs/heads/main")).unwrap(),
        std::fs::read(path.join(".git/logs/HEAD")).unwrap(),
        std::fs::read(path.join(".gitkeep")).unwrap(),
        std::fs::read(path.join("origin.txt")).unwrap(),
    ]
}

fn cold_scheduler(config: AppConfig, root: &Path, fallback: &Path) -> Scheduler {
    let engine = SyncEngine::new(
        config.clone(),
        Database::in_memory().unwrap(),
        SvnClient::new("file:///nonexistent", "fixture", ""),
        GitClient::new(fallback).unwrap(),
        Arc::new(IdentityMapper::new(&IdentityConfig::default()).unwrap()),
    );
    let (_, rx) = mpsc::channel(1);
    let (ws, _) = broadcast::channel(8);
    Scheduler::new(
        Arc::new(engine),
        Duration::from_secs(1),
        rx,
        ws,
        Arc::new(RwLock::new(ImportProgress::default())),
        Database::new(&root.join("reposync.db")).unwrap(),
        config,
    )
}

fn fixture_environment(root: &Path) -> Vec<(&'static str, std::ffi::OsString)> {
    use std::ffi::OsString;
    let as_path = |child: &str| root.join(child).into_os_string();
    vec![
        (CHILD_ROOT, root.as_os_str().to_owned()),
        ("HOME", as_path("home")),
        ("XDG_CONFIG_HOME", as_path("config")),
        ("XDG_CACHE_HOME", as_path("cache")),
        ("TMPDIR", as_path("tmp")),
        ("LC_ALL", OsString::from("C")),
        ("GIT_ALLOW_PROTOCOL", OsString::from("file")),
        ("GIT_TERMINAL_PROMPT", OsString::from("0")),
        ("GIT_CONFIG_NOSYSTEM", OsString::from("1")),
        ("GIT_CONFIG_SYSTEM", as_path("empty.gitconfig")),
        ("GIT_CONFIG_GLOBAL", as_path("empty.gitconfig")),
        ("GIT_TEMPLATE_DIR", as_path("template")),
        ("GIT_CONFIG_COUNT", OsString::from("2")),
        ("GIT_CONFIG_KEY_0", OsString::from("core.hooksPath")),
        ("GIT_CONFIG_VALUE_0", as_path("hooks")),
        ("GIT_CONFIG_KEY_1", OsString::from("credential.helper")),
        ("GIT_CONFIG_VALUE_1", OsString::new()),
        ("GIT_LFS_SKIP_SMUDGE", OsString::from("1")),
    ]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restored_prewrite_database_holds_on_actual_interval_tick() {
    if let Some(root) = std::env::var_os(CHILD_ROOT) {
        let root = std::path::PathBuf::from(root);
        let environment = fixture_environment(&root);
        assert_eq!(root.canonicalize().unwrap(), root);
        for (key, expected) in &environment {
            assert_eq!(std::env::var_os(*key).as_ref(), Some(expected), "{key}");
        }
        assert!(std::env::vars_os().all(|(key, _)|
            key.as_os_str() == std::ffi::OsStr::new("PATH")
                || environment.iter().any(|(name, _)| key.as_os_str() == std::ffi::OsStr::new(*name))
        ));
        let path = std::env::var_os("PATH").expect("fixture requires installed SVN and Git");
        assert!(std::env::split_paths(&path).all(|dir| dir.is_absolute()));
        assert!(std::fs::read(root.join("empty.gitconfig")).unwrap().is_empty());
        for empty in ["hooks", "template", "home", "config", "cache", "tmp"] {
            assert!(std::fs::read_dir(root.join(empty)).unwrap().next().is_none());
        }
        actual_interval_fixture().await;
        return;
    }

    // Give production scheduler subprocesses the same restricted environment
    // as fixture setup without mutating any other in-process test's env.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    for dir in ["hooks", "template", "home", "config", "cache", "tmp"] {
        std::fs::create_dir(root.join(dir)).unwrap();
    }
    std::fs::write(root.join("empty.gitconfig"), []).unwrap();
    let path = std::env::var_os("PATH").expect("fixture requires installed SVN and Git");
    assert!(
        std::env::split_paths(&path).all(|dir| dir.is_absolute()),
        "fixture tools must not resolve through cwd-relative PATH entries"
    );
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap());
    child.args([TEST_NAME, "--exact", "--nocapture", "--test-threads=1"])
        .current_dir(&root)
        .env_clear()
        .env("PATH", path)
        .envs(fixture_environment(&root))
        .kill_on_drop(true);
    let out = time::timeout(Duration::from_secs(180), child.output())
        .await.expect("clean scheduler fixture timed out").unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(out.status.success(), "clean scheduler fixture failed:\n{stdout}\n{stderr}");
    assert!(stdout.lines().any(|line| line.starts_with("test result: ok. 1 passed; 0 failed; 0 ignored;")));
    let proofs: Vec<_> = stdout.lines().chain(stderr.lines())
        .filter_map(|line| line.split_once(EVIDENCE_PREFIX).map(|(_, proof)| proof))
        .collect();
    assert_eq!(proofs.len(), 1, "expected one executed child receipt");
    let proof: Value = serde_json::from_str(proofs[0]).unwrap();
    assert_eq!(proof["case"], CASE);
    assert_eq!(proof["status"], "PASS");
    eprintln!("{EVIDENCE_PREFIX}{proof}");
}

async fn actual_interval_fixture() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let (mut live, bridge) = super::cancellation_tests::scheduler_fixture(&tmp, REPO, DUE);
    live.app_config.identity.email_domain = Some("example.invalid".into());
    live.app_config.daemon.memory_limit_mb = 0;
    live.memory_limit_bytes = 0;

    // Both VCS remotes are disposable filesystem repositories. The external
    // SVN author uses a distinct checkout, outside both scheduler state dirs.
    let svn_repo = root.join("svn-source");
    command(root, "svnadmin", &["create", svn_repo.to_str().unwrap()]);
    let svn_url = format!("file://{}", svn_repo.display());
    let svn = SvnClient::new(&svn_url, SVN_AUTHOR, "");
    let external = root.join("external-svn-wc");
    svn.checkout_head(&external).await.unwrap();
    let confined = reposync_core::svn::working_copy::ConfinedWorkingCopy::open(&external).unwrap();
    for (name, content, message, rev) in [
        (".gitkeep", b"".as_slice(), "Initial anchor", 1),
        ("origin.txt", b"Original SVN work\n".as_slice(), "Baseline SVN work", 2),
    ] {
        confined.write_file(name, content).unwrap();
        svn.add_with_retry(&external, &[name]).await.unwrap();
        assert_eq!(svn.commit(&external, message, SVN_AUTHOR).await.unwrap(), rev);
    }

    let bare = root.join("publish.git");
    git(root, &["init", "--bare", "--initial-branch=main", bare.to_str().unwrap()]);
    git(&bare, &["config", "core.logAllRefUpdates", "true"]);
    std::fs::create_dir_all(&bridge).unwrap();
    git(&bridge, &["init", "--initial-branch=main"]);
    git(&bridge, &["config", "user.name", "Fixture"]);
    git(&bridge, &["config", "user.email", "fixture@example.invalid"]);
    std::fs::write(bridge.join(".gitkeep"), []).unwrap();
    git(&bridge, &["add", ".gitkeep"]);
    git(&bridge, &["commit", "-m", "Initial anchor"]);
    git(&bridge, &["remote", "add", "origin", bare.to_str().unwrap()]);
    git(&bridge, &["push", "origin", "main"]);

    let mut repo = live.db.get_repository(REPO).unwrap().unwrap();
    repo.svn_url = svn_url;
    repo.svn_branch.clear();
    repo.svn_username = SVN_AUTHOR.into();
    repo.git_provider = "local".into();
    repo.git_api_url.clear();
    repo.git_repo = bare.to_str().unwrap().into();
    repo.sync_mode = "direct".into();
    repo.last_svn_rev = 1;
    repo.last_git_sha = git(&bridge, &["rev-parse", "HEAD"]);
    live.db.update_repository(&repo).unwrap();
    live.db.set_state(&format!("last_svn_rev_{REPO}"), "1").unwrap();
    let uuid = text_out(svnlook(&svn_repo, "uuid", &[]));
    assert!(!uuid.is_empty());
    reposync_core::db::lineage_checkpoints::record_independently_established_svn_uuid(
        &live.db, REPO, &uuid,
    ).unwrap();

    let mut maintenance = time::Instant::now();
    assert_completion(tick_and_join(&live, &mut maintenance, &svn_repo, &bare, &bridge).await, 1);
    let prior = publication(&live.db, 2, &bare);
    make_due(&live.db);
    let prior_ledger = ledger(&live.db);
    let sealed_db = root.join("sealed-r2.sqlite");
    live.db.conn().execute("VACUUM INTO ?1", [sealed_db.to_str().unwrap()]).unwrap();
    let seal = std::fs::read(&sealed_db).unwrap();

    let restored_root = root.join("restored");
    let restored_bridge = restored_root.join("repos").join(REPO).join("git-repo");
    std::fs::create_dir_all(restored_bridge.parent().unwrap()).unwrap();
    git(root, &[
        "clone", "--no-hardlinks", "--branch", "main", bare.to_str().unwrap(),
        restored_bridge.to_str().unwrap(),
    ]);
    assert_eq!(git(&restored_bridge, &["rev-parse", MAIN]), prior.intended_local_git_sha);
    let restored_checkout = bridge_witness(&restored_bridge);
    let previous_reflog = std::fs::read(bare.join("logs/refs/heads/main")).unwrap();

    confined.write_file("recovery.txt", SOURCE_BYTES).unwrap();
    svn.add_with_retry(&external, &["recovery.txt"]).await.unwrap();
    assert_eq!(svn.commit(&external, SOURCE_MESSAGE, SVN_AUTHOR).await.unwrap(), 3);
    assert_eq!(svnlook(&svn_repo, "cat", &["-r", "3", "recovery.txt"]), SOURCE_BYTES);
    assert_eq!(text_out(svnlook(&svn_repo, "log", &["-r", "3"])), SOURCE_MESSAGE);
    assert_eq!(text_out(svnlook(&svn_repo, "author", &["-r", "3"])), SVN_AUTHOR);
    assert_eq!(text_out(svnlook(&svn_repo, "changed", &["-r", "3"])), "A   recovery.txt");
    assert!(!bridge.join("recovery.txt").exists());

    assert_completion(tick_and_join(&live, &mut maintenance, &svn_repo, &bare, &bridge).await, 1);
    let genuine = publication(&live.db, 3, &bare);
    assert_ne!(genuine.id, prior.id);
    assert_eq!(genuine.pre_push_git_sha, prior.intended_local_git_sha);
    assert_eq!(genuine.source_svn_message, SOURCE_MESSAGE);
    assert_eq!(genuine.source_svn_author, SVN_AUTHOR);
    assert_eq!(command(&bare, "git", &["show", "refs/heads/main:recovery.txt"]), SOURCE_BYTES);
    let after_publish = remote_witness(&svn_repo, &bare);
    let appended = after_publish[2].strip_prefix(previous_reflog.as_slice()).unwrap();
    assert_eq!(appended.split(|b| *b == b'\n').filter(|s| !s.is_empty()).count(), 1);
    assert!(String::from_utf8_lossy(appended).starts_with(&format!(
        "{} {} ", prior.intended_local_git_sha, genuine.intended_local_git_sha,
    )));

    make_due(&live.db);
    let genuine_ledger = ledger(&live.db);
    assert_completion(tick_and_join(&live, &mut maintenance, &svn_repo, &bare, &bridge).await, 0);
    assert_eq!(ledger(&live.db), genuine_ledger);
    assert_eq!(remote_witness(&svn_repo, &bare), after_publish);
    let mut restored_config = live.app_config.clone();
    restored_config.daemon.data_dir = restored_root.clone();
    drop(live);

    let restore_db = restored_root.join("reposync.db");
    std::fs::copy(&sealed_db, &restore_db).unwrap();
    assert_eq!(std::fs::read(&restore_db).unwrap(), seal);
    let fallback = root.join("restored-fallback");
    drop(GitClient::init(&fallback).unwrap());
    for _attempt in 1..=2 {
        let scheduler = cold_scheduler(restored_config.clone(), &restored_root, &fallback);
        assert_eq!(ledger(&scheduler.db), prior_ledger);
        assert_eq!(bridge_witness(&restored_bridge), restored_checkout);
        let mut maintenance = time::Instant::now();
        let event = tick_and_join(&scheduler, &mut maintenance, &svn_repo, &bare, &restored_bridge).await;
        assert_eq!(event["type"], "repo_sync_failed", "{event}");
        assert!(event["error"].as_str().unwrap().contains("unreceipted_svn_publication"));
        let block = reposync_core::history_inspect::load_history_block_for_repo(&scheduler.db, REPO)
            .unwrap().expect("worker must persist the scoped reconciliation reason");
        assert_eq!(block["reason"], "unreceipted_svn_publication");
        assert_eq!(block["p_handled"], prior.intended_local_git_sha);
        assert_eq!(block["r_fresh_remote"], genuine.intended_local_git_sha);
        assert_eq!(block["l_bridge"], prior.intended_local_git_sha);
        assert_eq!(scheduler.db.get_repository(REPO).unwrap().unwrap().sync_status, "reconciliation_required");
        assert!(scheduler.db.get_git_push_operation(REPO, &genuine.id).unwrap().is_none());
        assert_eq!(ledger(&scheduler.db), prior_ledger);
        assert_eq!(bridge_witness(&restored_bridge), restored_checkout);
        assert_eq!(remote_witness(&svn_repo, &bare), after_publish);
    }
    assert_eq!(std::fs::read(sealed_db).unwrap(), seal);
    assert_eq!(ledger(&Database::new(&root.join("reposync.db")).unwrap()), genuine_ledger);
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        json!({
            "case": CASE, "status": "PASS", "scheduler_entry": "handle_interval_tick",
            "qualified_transport": "local filesystem Git and SVN",
            "restored_svn_revision": 2, "published_svn_revision": 3,
            "prior_git": prior.intended_local_git_sha, "published_git": genuine.intended_local_git_sha,
            "qualified_cold_attempts": 2, "registered_worker_each_attempt": true,
            "reason": "unreceipted_svn_publication", "checked_out_branch": MAIN,
            "restored_checkout_and_ledger_unchanged": true, "remote_refs_tree_reflog_and_source_unchanged": true,
            "live_receipted_repeat_noop": true,
            "exact_local_fetch_and_push_url_checked_each_tick": true,
            "source_author": SVN_AUTHOR
        })
    );
}
