//! Test-only adapter compiled against the unmodified 8737974 production crates.
//! It invokes the production per-repository import HTTP route, including its
//! completion writer, against disposable file:// SVN and Git repositories.
use std::{collections::HashMap, net::SocketAddr, path::PathBuf, process::Command, sync::Arc, time::Duration};

use axum::Router;
use reposync_core::{config::{AppConfig, IdentityConfig}, db::Database, git::GitClient,
    identity::IdentityMapper, import::{ImportPhase, ImportProgress}, models::Repository,
    svn::SvnClient, sync_engine::SyncEngine};
use reposync_web::{api, AppState};

fn run(program: &str, args: &[&str]) -> String {
    let result = Command::new(program).args(args).output().unwrap();
    assert!(result.status.success(), "{program} {args:?}: {}", String::from_utf8_lossy(&result.stderr));
    String::from_utf8(result.stdout).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn generate_legacy_import() {
    let root = PathBuf::from(std::env::var("REPOSYNC_OLD_FIXTURE_DIR").expect("owned output directory"));
    assert!(!root.exists(), "old fixture must be freshly generated");
    std::fs::create_dir_all(&root).unwrap();
    let source = root.join("svn_repo");
    let source_url = format!("file://{}", source.display());
    let bare = root.join("old-origin.git");
    let install = root.join("install");
    std::fs::create_dir_all(&install).unwrap();
    run("svnadmin", &["create", source.to_str().unwrap()]);
    run("svn", &["mkdir", &format!("{source_url}/trunk"), "-m", "Create trunk", "--non-interactive"]);
    let wc = root.join("source-wc");
    run("svn", &["checkout", &format!("{source_url}/trunk"), wc.to_str().unwrap(), "--non-interactive"]);
    std::fs::write(wc.join("origin.txt"), b"old SVN origin\n").unwrap();
    run("svn", &["add", wc.join("origin.txt").to_str().unwrap()]);
    run("svn", &["commit", wc.to_str().unwrap(), "-m", "Old import source", "--username", "fixture", "--non-interactive"]);
    run("git", &["init", "--bare", "--initial-branch=main", bare.to_str().unwrap()]);
    // The original route clones the configured branch. A local empty bare
    // remote does not supply a branch to libgit2, so give it an empty root.
    let bootstrap = root.join("bootstrap");
    run("git", &["init", "--initial-branch=main", bootstrap.to_str().unwrap()]);
    run("git", &["-C", bootstrap.to_str().unwrap(), "-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "commit", "--allow-empty", "-m", "Synthetic empty remote root"]);
    run("git", &["-C", bootstrap.to_str().unwrap(), "remote", "add", "origin", bare.to_str().unwrap()]);
    run("git", &["-C", bootstrap.to_str().unwrap(), "push", "origin", "main"]);

    let config_str = format!("[daemon]\ndata_dir = '{}'\n[svn]\nurl = '{}'\nusername = 'fixture'\npassword_env = 'REPOSYNC_TEST_SVN_PW'\n[github]\nrepo = 'fixture/old-origin'\ntoken_env = 'REPOSYNC_TEST_GH_TOKEN'\n", install.display(), source_url);
    std::fs::write(install.join("config.toml"), &config_str).unwrap();
    let mut config: AppConfig = toml::from_str(&config_str).unwrap();
    config.web.admin_password = Some("synthetic-only".into());
    config.svn.password = Some(String::new());
    config.github.token = Some(String::new());
    let db_path = install.join("reposync.db");
    let web_db = Database::new(&db_path).unwrap();
    web_db.initialize().unwrap();
    web_db.set_state("secret_svn_password_pair", "fixture-only-svn-secret").unwrap();
    web_db.set_state("secret_git_token_pair", "fixture-only-git-secret").unwrap();
    let now = chrono::Utc::now().to_rfc3339();
    web_db.insert_repository(&Repository {
        id: "pair".into(), name: "pinned old import".into(), svn_url: source_url.clone(),
        svn_branch: "trunk".into(), svn_username: "fixture".into(), git_provider: "local".into(),
        git_api_url: format!("file://{}", root.display()), git_repo: "old-origin".into(),
        git_branch: "main".into(), sync_mode: "team".into(), poll_interval_secs: 5,
        lfs_threshold_mb: 0, auto_merge: false, enabled: true, created_by: None,
        parent_id: None, created_at: now.clone(), updated_at: now, last_svn_rev: 0,
        last_git_sha: String::new(), last_sync_at: None, sync_status: "idle".into(),
        total_syncs: 0, total_errors: 0, allowed_paths: None, blocked_patterns: None,
        consecutive_errors: 0, teams_webhook_url: None,
    }).unwrap();
    let dummy_git = install.join("dummy-git");
    run("git", &["init", "--initial-branch=main", dummy_git.to_str().unwrap()]);
    let engine_db = Database::new(&db_path).unwrap();
    let engine = SyncEngine::new(config.clone(), engine_db,
        SvnClient::new(&format!("{source_url}/trunk"), "fixture", ""),
        GitClient::new(&dummy_git).unwrap(),
        Arc::new(IdentityMapper::new(&IdentityConfig::default()).unwrap()));
    let (sync_tx, _sync_rx) = tokio::sync::mpsc::channel(1);
    let (ws_tx, _) = tokio::sync::broadcast::channel(256);
    let state = Arc::new(AppState {
        db: web_db, sync_engine: Arc::new(engine), config,
        sync_trigger: sync_tx, ws_broadcast: ws_tx,
        sessions: tokio::sync::RwLock::new(HashMap::new()),
        import_progress: Arc::new(tokio::sync::RwLock::new(ImportProgress::default())),
        config_path: install.join("config.toml"), prev_net_snapshot: std::sync::Mutex::new(None),
        repo_import_progress: tokio::sync::RwLock::new(HashMap::new()),
        login_attempts: std::sync::Mutex::new(HashMap::new()),
        import_handles: tokio::sync::Mutex::new(Vec::new()),
    });
    state.sessions.write().await.insert("old-fixture-session".into(), chrono::Utc::now() + chrono::Duration::hours(1));
    let app = Router::new().merge(api::repos::routes()).with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await.unwrap(); });
    let response = reqwest::Client::new()
        .post(format!("http://{addr}/api/repos/pair/import"))
        .header("authorization", "Bearer old-fixture-session")
        .send().await.unwrap();
    assert!(response.status().is_success(), "old import route: {:?}", response.text().await);
    let mut completed = false;
    for _ in 0..600 {
        let progress = state.get_repo_import_progress("pair").await;
        let phase = progress.read().await.phase.clone();
        if phase == ImportPhase::Completed { completed = true; break; }
        assert_ne!(phase, ImportPhase::Failed, "old import failed: {:?}", progress.read().await.errors);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(completed, "old import did not complete");
    let progress = state.get_repo_import_progress("pair").await;
    assert!(progress.read().await.errors.is_empty(), "old import reported a partial failure");
    let repo = state.db.get_repository("pair").unwrap().unwrap();
    let kv = state.db.get_state("last_git_sha_pair").unwrap();
    assert_eq!(kv.as_deref(), Some(repo.last_git_sha.as_str()));
    assert!(repo.last_svn_rev > 0 && repo.last_git_sha.len() == 40);
    let map_count: i64 = state.db.conn().query_row(
        "SELECT COUNT(*) FROM sync_records WHERE repo_id = 'pair' AND direction = 'svn_to_git' AND git_sha = ?1 AND status = 'applied'",
        [&repo.last_git_sha], |row| row.get(0)).unwrap();
    assert_eq!(map_count, 1);
    let outbound: i64 = state.db.conn().query_row(
        "SELECT COUNT(*) FROM sync_records WHERE repo_id = 'pair' AND direction = 'git_to_svn' AND status = 'applied'",
        [], |row| row.get(0)).unwrap();
    assert_eq!(outbound, 0);
    let persisted_progress: (String, String) = state.db.conn().query_row(
        "SELECT phase, errors_json FROM import_progress WHERE id = 1", [],
        |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
    assert_eq!(persisted_progress, ("completed".to_string(), "[]".to_string()));
    assert_eq!(run("git", &["--git-dir", bare.to_str().unwrap(), "rev-parse", "refs/heads/main"]).trim(), repo.last_git_sha);
    eprintln!("OLD_FIXTURE_EVIDENCE {}", serde_json::json!({
        "generator_code": "87379741779a6259f7eeb52a68cc6f061174e5ef",
        "svn_revision": repo.last_svn_rev, "imported_git": repo.last_git_sha,
        "kv": kv, "import_mapping": map_count, "outbound_mapping": outbound,
        "source": source_url, "install": install,
    }));
    server.abort();
    // The process exits after this test. Candidate opens only a separate copy.
}

fn topology_source(root: &std::path::Path, name: &str) -> (String, PathBuf) {
    let source = root.join(format!("svn-{name}"));
    let source_url = format!("file://{}", source.display());
    run("svnadmin", &["create", source.to_str().unwrap()]);
    run("svn", &["mkdir", &format!("{source_url}/trunk"), "-m", "Create trunk", "--non-interactive"]);
    let wc = root.join(format!("source-wc-{name}"));
    run("svn", &["checkout", &format!("{source_url}/trunk"), wc.to_str().unwrap(), "--non-interactive"]);
    std::fs::write(wc.join("origin.txt"), format!("old SVN origin {name}\n")).unwrap();
    run("svn", &["add", wc.join("origin.txt").to_str().unwrap()]);
    run("svn", &["commit", wc.to_str().unwrap(), "-m", "Old import source", "--username", "fixture", "--non-interactive"]);
    let bare = root.join(format!("origin-{name}.git"));
    run("git", &["init", "--bare", "--initial-branch=main", bare.to_str().unwrap()]);
    let bootstrap = root.join(format!("bootstrap-{name}"));
    run("git", &["init", "--initial-branch=main", bootstrap.to_str().unwrap()]);
    run("git", &["-C", bootstrap.to_str().unwrap(), "-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "commit", "--allow-empty", "-m", "Synthetic empty remote root"]);
    run("git", &["-C", bootstrap.to_str().unwrap(), "remote", "add", "origin", bare.to_str().unwrap()]);
    run("git", &["-C", bootstrap.to_str().unwrap(), "push", "origin", "main"]);
    (source_url, bare)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn generate_legacy_topology() {
    let root = PathBuf::from(std::env::var("REPOSYNC_OLD_TOPOLOGY_DIR").expect("owned topology output"));
    assert!(!root.exists(), "topology must be freshly generated");
    std::fs::create_dir_all(&root).unwrap();
    let install = root.join("install");
    std::fs::create_dir_all(&install).unwrap();
    let first = topology_source(&root, "one");
    let second = topology_source(&root, "two");
    let disabled = topology_source(&root, "disabled");
    let config_str = format!("[daemon]\ndata_dir = '{}'\n[svn]\nurl = '{}'\nusername = 'fixture'\npassword_env = 'REPOSYNC_TEST_SVN_PW'\n[github]\nrepo = 'fixture/topology'\ntoken_env = 'REPOSYNC_TEST_GH_TOKEN'\n", install.display(), first.0);
    std::fs::write(install.join("config.toml"), &config_str).unwrap();
    let mut config: AppConfig = toml::from_str(&config_str).unwrap();
    config.web.admin_password = Some("synthetic-only".into());
    config.svn.password = Some(String::new());
    config.github.token = Some(String::new());
    let db_path = install.join("reposync.db");
    let web_db = Database::new(&db_path).unwrap();
    web_db.initialize().unwrap();
    let now = chrono::Utc::now().to_rfc3339();
    for (id, source, bare, enabled) in [
        ("pair", &first.0, &first.1, true),
        ("pair_two", &second.0, &second.1, true),
        ("pair_disabled", &disabled.0, &disabled.1, false),
    ] {
        web_db.insert_repository(&Repository {
            id: id.into(), name: format!("old topology {id}"), svn_url: source.clone(),
            svn_branch: "trunk".into(), svn_username: "fixture".into(), git_provider: "local".into(),
            git_api_url: format!("file://{}", root.display()),
            git_repo: bare.file_stem().unwrap().to_string_lossy().to_string(),
            git_branch: "main".into(), sync_mode: "team".into(), poll_interval_secs: 5,
            lfs_threshold_mb: 0, auto_merge: false, enabled, created_by: None,
            parent_id: None, created_at: now.clone(), updated_at: now.clone(), last_svn_rev: 0,
            last_git_sha: String::new(), last_sync_at: None, sync_status: "idle".into(),
            total_syncs: 0, total_errors: 0, allowed_paths: None, blocked_patterns: None,
            consecutive_errors: 0, teams_webhook_url: None,
        }).unwrap();
        web_db.set_state(&format!("secret_svn_password_{id}"), &format!("synthetic-svn-{id}")).unwrap();
        web_db.set_state(&format!("secret_git_token_{id}"), &format!("synthetic-git-{id}")).unwrap();
    }
    let dummy_git = install.join("dummy-git");
    run("git", &["init", "--initial-branch=main", dummy_git.to_str().unwrap()]);
    let engine = SyncEngine::new(config.clone(), Database::new(&db_path).unwrap(),
        SvnClient::new(&format!("{}/trunk", first.0), "fixture", ""),
        GitClient::new(&dummy_git).unwrap(),
        Arc::new(IdentityMapper::new(&IdentityConfig::default()).unwrap()));
    let (sync_tx, _sync_rx) = tokio::sync::mpsc::channel(1);
    let (ws_tx, _) = tokio::sync::broadcast::channel(256);
    let state = Arc::new(AppState {
        db: web_db, sync_engine: Arc::new(engine), config,
        sync_trigger: sync_tx, ws_broadcast: ws_tx,
        sessions: tokio::sync::RwLock::new(HashMap::new()),
        import_progress: Arc::new(tokio::sync::RwLock::new(ImportProgress::default())),
        config_path: install.join("config.toml"), prev_net_snapshot: std::sync::Mutex::new(None),
        repo_import_progress: tokio::sync::RwLock::new(HashMap::new()),
        login_attempts: std::sync::Mutex::new(HashMap::new()),
        import_handles: tokio::sync::Mutex::new(Vec::new()),
    });
    state.sessions.write().await.insert("old-topology-session".into(), chrono::Utc::now() + chrono::Duration::hours(1));
    let app = Router::new().merge(api::repos::routes()).with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await.unwrap(); });
    for (id, bare) in [("pair", &first.1), ("pair_two", &second.1)] {
        let response = reqwest::Client::new()
            .post(format!("http://{addr}/api/repos/{id}/import"))
            .header("authorization", "Bearer old-topology-session")
            .send().await.unwrap();
        assert!(response.status().is_success(), "old import {id}: {:?}", response.text().await);
        let mut completed = false;
        for _ in 0..600 {
            let progress = state.get_repo_import_progress(id).await;
            let phase = progress.read().await.phase.clone();
            if phase == ImportPhase::Completed { completed = true; break; }
            assert_ne!(phase, ImportPhase::Failed, "old import {id} failed: {:?}", progress.read().await.errors);
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(completed, "old import {id} did not complete");
        let repo = state.db.get_repository(id).unwrap().unwrap();
        assert_eq!(repo.last_svn_rev, 2);
        assert_eq!(state.db.get_state(&format!("last_git_sha_{id}")).unwrap().as_deref(), Some(repo.last_git_sha.as_str()));
        assert_eq!(run("git", &["--git-dir", bare.to_str().unwrap(), "rev-parse", "refs/heads/main"]).trim(), repo.last_git_sha);
        let maps: i64 = state.db.conn().query_row(
            "SELECT COUNT(*) FROM sync_records WHERE repo_id = ?1 AND svn_rev = 2 AND git_sha = ?2 AND direction = 'svn_to_git' AND status = 'applied'",
            rusqlite::params![id, repo.last_git_sha], |row| row.get(0)).unwrap();
        assert_eq!(maps, 1);
    }
    let pair = state.db.get_repository("pair").unwrap().unwrap();
    let pair_two = state.db.get_repository("pair_two").unwrap().unwrap();
    let disabled_row = state.db.get_repository("pair_disabled").unwrap().unwrap();
    assert_ne!(pair.last_git_sha, pair_two.last_git_sha);
    assert!(!disabled_row.enabled && disabled_row.last_svn_rev == 0 && disabled_row.last_git_sha.is_empty());
    let disabled_maps: i64 = state.db.conn().query_row("SELECT COUNT(*) FROM sync_records WHERE repo_id = 'pair_disabled'", [], |row| row.get(0)).unwrap();
    assert_eq!(disabled_maps, 0);
    eprintln!("OLD_TOPOLOGY_EVIDENCE {}", serde_json::json!({
        "generator_code":"87379741779a6259f7eeb52a68cc6f061174e5ef",
        "pair":{"svn_rev":pair.last_svn_rev,"git_sha":pair.last_git_sha},
        "pair_two":{"svn_rev":pair_two.last_svn_rev,"git_sha":pair_two.last_git_sha},
        "disabled":{"enabled":disabled_row.enabled,"svn_rev":disabled_row.last_svn_rev,"mappings":disabled_maps},
        "independent_sources":[first.0,second.0], "install":install,
    }));
    server.abort();
}
