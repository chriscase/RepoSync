//! v2 transport boundaries use a current database actor and the actual engine.
use std::sync::Arc;

use reposync_core::config::AppConfig;
use reposync_core::db::Database;
use reposync_core::git::GitClient;
use reposync_core::identity::IdentityMapper;
use reposync_core::import::ImportProgress;
use reposync_core::svn::SvnClient;
use reposync_core::sync_engine::SyncEngine;
use reposync_web::{api, AppState, WebServer};

struct Fixture {
    state: Arc<AppState>,
    url: String,
    server: tokio::task::JoinHandle<()>,
    _temp: tempfile::TempDir,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

fn engine(config: &AppConfig, db: Database, git: &std::path::Path) -> Arc<SyncEngine> {
    Arc::new(SyncEngine::new(
        config.clone(),
        db,
        SvnClient::new("file:///unused", "", ""),
        GitClient::new(git).unwrap(),
        Arc::new(IdentityMapper::new(&Default::default()).unwrap()),
    ))
}

async fn fixture(wrong_engine: bool) -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let db = Database::new(root.join("reposync.db")).unwrap();
    db.initialize().unwrap();
    let mut config: AppConfig = toml::from_str(
        r#"
        [daemon]
        data_dir = "/unused"
        [svn]
        url = "file:///unused"
        username = ""
        password_env = ""
        [github]
        repo = "fixture/repo"
        token_env = ""
    "#,
    )
    .unwrap();
    config.daemon.data_dir = root.clone();
    db.set_state("encryption_key", &"51".repeat(32)).unwrap();
    let now = chrono::Utc::now();
    for (id, role, enabled) in [
        ("admin", "admin", true),
        ("user", "user", true),
        ("disabled", "admin", false),
    ] {
        db.insert_user(&reposync_core::models::User {
            id: id.into(),
            username: id.into(),
            display_name: id.into(),
            email: format!("{id}@example.invalid"),
            password_hash: "fixture".into(),
            role: role.into(),
            enabled,
            created_at: (now - chrono::Duration::minutes(2)).to_rfc3339(),
            updated_at: now.to_rfc3339(),
        })
        .unwrap();
        db.insert_session(&reposync_core::models::Session {
            token: format!("token-{id}"),
            user_id: id.into(),
            created_at: (now - chrono::Duration::minutes(1)).to_rfc3339(),
            expires_at: (now + chrono::Duration::hours(1)).to_rfc3339(),
        })
        .unwrap();
    }
    db.insert_session(&reposync_core::models::Session {
        token: "token-expired".into(),
        user_id: "admin".into(),
        created_at: (now - chrono::Duration::hours(2)).to_rfc3339(),
        expires_at: (now - chrono::Duration::hours(1)).to_rfc3339(),
    })
    .unwrap();
    let git = root.join("git");
    git2::Repository::init(&git).unwrap();
    let actual = engine(
        &config,
        Database::new(root.join("reposync.db")).unwrap(),
        &git,
    );
    let routed = if wrong_engine {
        engine(
            &config,
            Database::new(root.join("reposync.db")).unwrap(),
            &git,
        )
    } else {
        actual.clone()
    };
    let (tx, _rx) = tokio::sync::mpsc::channel(1);
    let state = WebServer::new(
        config,
        db,
        actual,
        tx,
        root.join("config.toml"),
        Arc::new(tokio::sync::RwLock::new(ImportProgress::default())),
    )
    .app_state();
    state.sessions.write().await.insert(
        "legacy-memory-token".into(),
        now + chrono::Duration::hours(1),
    );
    let router = api::pair_refresh::routes(routed).with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    Fixture {
        state,
        url,
        server,
        _temp: temp,
    }
}

fn state(f: &Fixture) -> Vec<(String, String, String)> {
    f.state
        .db
        .conn()
        .prepare("SELECT key,value,updated_at FROM kv_state ORDER BY key")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[tokio::test]
async fn v2_requires_current_enabled_database_administrator_before_preparation() {
    let f = fixture(false).await;
    let saved = state(&f);
    let client = reqwest::Client::new();
    for token in [
        "token-user",
        "token-disabled",
        "token-expired",
        "legacy-memory-token",
        "unknown",
    ] {
        let response = client
            .post(format!("{}/api/v2/repos/child/refresh/prepare", f.url))
            .bearer_auth(token)
            .json(&serde_json::json!({"request_id":"request"}))
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            reqwest::StatusCode::UNAUTHORIZED,
            "{token}"
        );
        assert_eq!(state(&f), saved);
    }
    let response = client
        .post(format!("{}/api/v2/repos/child/refresh/prepare", f.url))
        .json(&serde_json::json!({"request_id":"request"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert_eq!(state(&f), saved);
}

#[tokio::test]
async fn v2_rejects_unknown_fields_reanchor_empty_request_and_fabricated_handle() {
    let f = fixture(false).await;
    let saved = state(&f);
    let client = reqwest::Client::new();
    let prepare = format!("{}/api/v2/repos/child/refresh/prepare", f.url);
    for body in [
        serde_json::json!({"request_id":"request","execute":true}),
        serde_json::json!({"request_id":"request","operation":"reanchor"}),
    ] {
        let response = client
            .post(&prepare)
            .bearer_auth("token-admin")
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
    }
    let response = client
        .post(&prepare)
        .bearer_auth("token-admin")
        .json(&serde_json::json!({"request_id":""}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
    let action = format!("{}/api/v2/repos/child/refresh/action", f.url);
    let response = client.post(&action).bearer_auth("token-admin").json(&serde_json::json!({
        "request_id":"request","preparation_id":uuid::Uuid::new_v4(),"approval_digest":"fake","action":"execute"
    })).send().await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
    let response = client.post(&action).bearer_auth("token-admin").json(&serde_json::json!({
        "request_id":"request","preparation_id":uuid::Uuid::new_v4(),"approval_digest":"fake","action":"reanchor"
    })).send().await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(state(&f), saved);
}

#[tokio::test]
async fn v2_refuses_an_equivalent_but_different_engine_instance() {
    let f = fixture(true).await;
    let saved = state(&f);
    let response = reqwest::Client::new()
        .post(format!("{}/api/v2/repos/child/refresh/prepare", f.url))
        .bearer_auth("token-admin")
        .json(&serde_json::json!({"request_id":"request"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
    assert_eq!(state(&f), saved);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recovery_server_preserves_journals_and_exposes_only_authenticated_recovery() {
    let f = fixture(false).await;
    f.state.db.ensure_repository_scope_schema_upgrade().unwrap();
    f.state
        .db
        .set_state(
            "pair_refresh_execution_v3:authority:retained",
            "corrupt-retained-evidence",
        )
        .unwrap();
    let saved = state(&f);
    let root = &f.state.config.daemon.data_dir;
    let lock = Arc::new(reposync_core::data_dir::acquire(root).unwrap());
    let db = Database::open_existing_for_pair_refresh_recovery(root.join("reposync.db")).unwrap();
    let server = WebServer::for_pair_refresh_recovery(
        f.state.config.clone(),
        db,
        f.state.sync_engine.clone(),
        root.join("config.toml"),
        lock,
    )
    .unwrap();
    assert_eq!(state(&f), saved);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    drop(listener);
    let url = format!("http://{addr}");
    let task = tokio::spawn(async move {
        server.start(&addr).await.unwrap();
    });
    let client = reqwest::Client::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if client
            .get(format!("{url}/api/auth/info"))
            .send()
            .await
            .is_ok()
        {
            break;
        }
        assert!(std::time::Instant::now() < deadline && !task.is_finished());
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    for path in [
        "/api/v2/repos/child/refresh/prepare",
        "/api/repos",
        "/api/config",
        "/api/sync",
        "/",
    ] {
        let response = client
            .post(format!("{url}{path}"))
            .bearer_auth("token-admin")
            .json(&serde_json::json!({"request_id":"new"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND, "{path}");
    }
    let endpoint = format!("{url}/api/v2/repos/child/refresh/recovery/prepare");
    let response = client
        .post(&endpoint)
        .json(&serde_json::json!({"request_id":"held"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    let response = client
        .post(&endpoint)
        .bearer_auth("token-admin")
        .json(&serde_json::json!({"request_id":"held"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
    assert_eq!(state(&f), saved);
    task.abort();
    let _ = task.await;
}
#[tokio::test]
async fn review_routes_require_native_admin_and_actual_engine_without_writes() {
    let f = fixture(false).await;
    let before = state(&f);
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    for token in [
        None,
        Some("token-user"),
        Some("token-disabled"),
        Some("token-expired"),
        Some("legacy-memory-token"),
        Some("unknown"),
    ] {
        let mut request = client
            .post(format!("{}/api/v2/repos/child/refresh/review", f.url))
            .json(&serde_json::json!({}));
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        assert_eq!(
            request.send().await.unwrap().status(),
            reqwest::StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(state(&f), before);
    let other = fixture(true).await;
    let before = state(&other);
    let response = client
        .post(format!("{}/api/v2/repos/child/refresh/review", other.url))
        .bearer_auth("token-admin")
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
    assert_eq!(state(&other), before);
}

#[tokio::test]
async fn review_routes_reject_authority_fields_and_remain_absent_from_recovery() {
    let f = fixture(false).await;
    let before = state(&f);
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    for body in [
        serde_json::json!({"execute":true}),
        serde_json::json!({"actor_id":"admin"}),
        serde_json::json!({"historical_operation_id":uuid::Uuid::new_v4()}),
    ] {
        let response = client
            .post(format!("{}/api/v2/repos/child/refresh/review", f.url))
            .bearer_auth("token-admin")
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
    }
    let body = serde_json::json!({"review_pin":"0".repeat(64),"resolution":{"schema":1,"policy":"unsupported","manifest_digest":"0".repeat(64),"choices":[]}});
    let response = client
        .post(format!(
            "{}/api/v2/repos/child/refresh/review/candidate",
            f.url
        ))
        .bearer_auth("token-admin")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
    assert_eq!(state(&f), before);
    let service =
        reposync_core::pair_refresh::service::RefreshService::new(f.state.sync_engine.clone());
    let router = api::pair_refresh::recovery_routes(service.clone()).with_state(f.state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let root = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    for suffix in ["review", "review/candidate"] {
        let response = client
            .post(format!("{root}/api/v2/repos/child/refresh/{suffix}"))
            .bearer_auth("token-admin")
            .json(&serde_json::json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);
    }
    assert_eq!(state(&f), before);
    service.shutdown().await;
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}

// Selected separately with --features reliability-fixture; default CI is not
// evidence that this real-source HTTP journey ran.
#[cfg(all(feature = "reliability-fixture", unix))]
#[path = "../../core/tests/fixtures/owned_process_group.rs"]
mod review_fixture_process;

#[cfg(all(feature = "reliability-fixture", unix))]
mod authenticated_review_http_journey {
    use super::review_fixture_process as fixture_process;
    use git2::{Oid, Repository, Signature, Time};
    use reposync_core::db::pair_refresh_operations::{PairRefreshOperation, PairRefreshPhase};
    use reposync_core::db::Database;
    use reposync_core::pair_refresh::service::{RefreshAction, RefreshResponse, RefreshService};
    use reposync_web::{api, WebServer};
    use std::io::{BufRead, BufReader};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::Arc;
    use uuid::Uuid;

    // Same real SCM fixture/bootstrap as the qualified core source tests. Only
    // the transport client gains its disposable CA; actual proofs stay native.
    fn run(command: &mut Command) -> String {
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    fn svn(args: &[&str], directory: Option<&Path>) {
        let mut command = Command::new("svn");
        command
            .args(args)
            .args(["--non-interactive", "--no-auth-cache"]);
        if let Some(directory) = directory {
            command.current_dir(directory);
        }
        run(&mut command);
    }

    fn commit(repository: &Repository, bytes: &[u8], parents: &[Oid]) -> Oid {
        let mut tree = repository.treebuilder(None).unwrap();
        tree.insert("file.txt", repository.blob(bytes).unwrap(), 0o100644)
            .unwrap();
        let tree_id = tree.write().unwrap();
        let tree = repository.find_tree(tree_id).unwrap();
        let signature = Signature::new(
            "Disposable preparation fixture",
            "fixture@example.invalid",
            &Time::new(1_790_000_000, 0),
        )
        .unwrap();
        let parents = parents
            .iter()
            .map(|id| repository.find_commit(*id).unwrap())
            .collect::<Vec<_>>();
        let parents = parents.iter().collect::<Vec<_>>();
        repository
            .commit(None, &signature, &signature, "fixture", &tree, &parents)
            .unwrap()
    }

    struct Fixture {
        db: Database,
        _server: fixture_process::OwnedProcessGroup,
        target: Repository,
        _parent_wc: PathBuf,
        root: String,
        baseline: Oid,
        parent: Oid,
        client: reqwest::Client,
        log: PathBuf,
        temp: tempfile::TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let db = Database::new(temp.path().join("reposync.db")).unwrap();
            db.initialize().unwrap();
            let svn_repository = temp.path().join("svn-repository");
            run(Command::new("svnadmin").arg("create").arg(&svn_repository));
            let root = reqwest::Url::from_file_path(svn_repository.canonicalize().unwrap())
                .unwrap()
                .to_string();
            svn(
                &[
                    "mkdir",
                    &format!("{root}/trunk"),
                    &format!("{root}/branches"),
                    "-m",
                    "layout",
                ],
                None,
            );
            let parent_wc = temp.path().join("parent-wc");
            svn(
                &[
                    "checkout",
                    &format!("{root}/trunk"),
                    parent_wc.to_str().unwrap(),
                ],
                None,
            );
            std::fs::write(parent_wc.join("file.txt"), b"baseline\n").unwrap();
            svn(&["add", "file.txt"], Some(&parent_wc));
            svn(&["commit", "-m", "baseline"], Some(&parent_wc));
            svn(
                &[
                    "copy",
                    &format!("{root}/trunk@2"),
                    &format!("{root}/branches/f"),
                    "-m",
                    "direct copy",
                ],
                None,
            );
            std::fs::write(parent_wc.join("file.txt"), b"advanced\n").unwrap();
            svn(&["commit", "-m", "parent advance"], Some(&parent_wc));

            let target = Repository::init_bare(temp.path().join("target.git")).unwrap();
            let baseline = commit(&target, b"baseline\n", &[]);
            let parent = commit(&target, b"advanced\n", &[baseline]);
            target
                .reference("refs/heads/feature", baseline, false, "fixture")
                .unwrap();
            target
                .reference("refs/heads/main", parent, false, "fixture")
                .unwrap();
            let cert = temp.path().join("cert.pem");
            let key = temp.path().join("key.pem");
            let config = temp.path().join("cert.cnf");
            std::fs::write(&config, "[req]\ndistinguished_name=dn\nx509_extensions=ext\nprompt=no\n[dn]\nCN=localhost\n[ext]\nsubjectAltName=DNS:localhost,IP:127.0.0.1\nkeyUsage=digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\nbasicConstraints=CA:false\n").unwrap();
            run(Command::new("openssl")
                .args([
                    "req", "-x509", "-newkey", "rsa:2048", "-days", "1", "-nodes", "-keyout",
                ])
                .arg(&key)
                .arg("-out")
                .arg(&cert)
                .arg("-config")
                .arg(&config));
            let der = temp.path().join("cert.der");
            run(Command::new("openssl")
                .arg("x509")
                .arg("-in")
                .arg(&cert)
                .args(["-outform", "DER", "-out"])
                .arg(&der));
            let client = reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(std::time::Duration::from_secs(10))
                .add_root_certificate(
                    reqwest::Certificate::from_der(&std::fs::read(der).unwrap()).unwrap(),
                )
                .build()
                .unwrap();
            let log = temp.path().join("requests.jsonl");
            let script = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../core/tests/fixtures/pair_refresh_http_server.py");
            let mut command = Command::new("python3");
            command
                .arg(script)
                .arg(temp.path())
                .arg("--cert")
                .arg(&cert)
                .arg("--key")
                .arg(&key)
                .arg("--token")
                .arg("disposable-preparation-token")
                .arg("--request-log")
                .arg(&log)
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit());
            let mut server = fixture_process::OwnedProcessGroup::spawn(&mut command).unwrap();
            let mut line = String::new();
            BufReader::new(server.stdout.take().unwrap())
                .read_line(&mut line)
                .unwrap();
            let ready: serde_json::Value = serde_json::from_str(&line).unwrap();
            let url = ready["url"].as_str().unwrap();
            db.set_state("encryption_key", &"41".repeat(32)).unwrap();
            for (id, parent_id, sha, revision, git_branch, svn_branch) in [
                ("parent", None, parent, 4, "main", "trunk"),
                (
                    "child",
                    Some("parent"),
                    baseline,
                    3,
                    "feature",
                    "branches/f",
                ),
            ] {
                let scope = Uuid::new_v4().to_string();
                db.conn().execute("INSERT INTO repositories(id,name,svn_url,svn_branch,svn_username,git_provider,git_api_url,git_repo,git_branch,sync_mode,poll_interval_secs,lfs_threshold_mb,auto_merge,enabled,created_at,updated_at,last_svn_rev,last_git_sha,sync_status,total_syncs,total_errors,parent_id,scope_uuid) VALUES(?1,?1,?2,?3,'','gitea',?4,'target',?5,'team',5,0,0,1,'t','t',?6,?7,'idle',0,0,?8,?9)", rusqlite::params![id, root, svn_branch, format!("{}/api/v1", url.trim_end_matches("/target.git")), git_branch, revision, sha.to_string(), parent_id, scope]).unwrap();
                db.set_state(&format!("last_svn_rev_{id}"), &revision.to_string())
                    .unwrap();
                db.set_state(&format!("last_git_sha_{scope}"), &sha.to_string())
                    .unwrap();
                db.set_state(
                    &format!("secret_git_token_{id}"),
                    "disposable-preparation-token",
                )
                .unwrap();
            }
            Self {
                db,
                _server: server,
                target,
                _parent_wc: parent_wc,
                root,
                baseline,
                parent,
                client,
                log,
                temp,
            }
        }

        fn engine(&self) -> reposync_core::sync_engine::SyncEngine {
            let mut config: reposync_core::config::AppConfig = toml::from_str(
                r#"
            [daemon]
            data_dir = "/unused"
            [svn]
            url = "file:///unused/svn"
            username = ""
            password_env = ""
            [github]
            repo = "fixture/repo"
            token_env = ""
        "#,
            )
            .unwrap();
            config.daemon.data_dir = self.temp.path().to_owned();
            let created = (chrono::Utc::now() - chrono::Duration::minutes(2)).to_rfc3339();
            self.db
                .insert_user(&reposync_core::models::User {
                    id: "admin".into(),
                    username: "admin".into(),
                    display_name: "Admin".into(),
                    email: "admin@example.invalid".into(),
                    password_hash: "fixture".into(),
                    role: "admin".into(),
                    enabled: true,
                    created_at: created.clone(),
                    updated_at: created,
                })
                .unwrap();
            self.db
                .insert_session(&reposync_core::models::Session {
                    token: "disposable-workspace-session".into(),
                    user_id: "admin".into(),
                    created_at: (chrono::Utc::now() - chrono::Duration::minutes(1)).to_rfc3339(),
                    expires_at: (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
                })
                .unwrap();
            let git_dir = self.temp.path().join("engine-git");
            Repository::init(&git_dir).unwrap();
            reposync_core::sync_engine::SyncEngine::new(
                config,
                Database::new(self.temp.path().join("reposync.db")).unwrap(),
                reposync_core::svn::SvnClient::new("file:///unused/svn", "", ""),
                reposync_core::git::client::GitClient::new(git_dir).unwrap(),
                std::sync::Arc::new(
                    reposync_core::identity::mapper::IdentityMapper::new(
                        &reposync_core::config::IdentityConfig::default(),
                    )
                    .unwrap(),
                ),
            )
        }

        fn state(&self) -> Vec<(String, String, String)> {
            self.db
                .conn()
                .prepare("SELECT key,value,updated_at FROM kv_state ORDER BY key")
                .unwrap()
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        }
    }

    fn svn_revision(url: &str) -> i64 {
        let mut command = Command::new("svn");
        command
            .args(["info", "--show-item", "revision", url])
            .args(["--non-interactive", "--no-auth-cache"]);
        let revision: i64 = run(&mut command)
            .trim()
            .parse()
            .expect("SVN revision query returned malformed stdout");
        assert!(revision > 0, "SVN revision must be positive");
        revision
    }

    fn producer_engine(f: &Fixture, id: &str) -> (reposync_core::sync_engine::SyncEngine, PathBuf) {
        let branch = if id == "child" { "feature" } else { "main" };
        let path = f.temp.path().join(format!("m2d-{id}-bridge"));
        let repo = if path.exists() {
            let existing = Repository::open(&path).unwrap();
            assert_eq!(existing.head().unwrap().shorthand(), Some(branch));
            assert_eq!(
                existing.find_remote("origin").unwrap().url(),
                f.target.path().to_str()
            );
            existing
        } else {
            git2::build::RepoBuilder::new()
                .branch(branch)
                .clone(f.target.path().to_str().unwrap(), &path)
                .unwrap()
        };
        repo.config()
            .unwrap()
            .set_str("user.name", "Fixture")
            .unwrap();
        repo.config()
            .unwrap()
            .set_str("user.email", "fixture@example.invalid")
            .unwrap();
        drop(repo);
        let mut config: reposync_core::config::AppConfig = toml::from_str(
            r#"
            [daemon]
            data_dir = "/unused"
            [svn]
            url = "file:///unused/svn"
            username = ""
            password_env = ""
            [github]
            repo = "fixture/repo"
            token_env = ""
            "#,
        )
        .unwrap();
        config.daemon.data_dir = f.temp.path().to_owned();
        let svn_branch = if id == "child" { "branches/f" } else { "trunk" };
        let svn_url = format!("{}/{svn_branch}", f.root);
        config.svn.url = svn_url.clone();
        config.svn.layout = reposync_core::config::SvnLayout::Custom;
        config.svn.password = Some(String::new());
        config.github.default_branch = branch.to_owned();
        config.github.token = Some(String::new());
        config.sync.auto_merge = false;
        let mut engine = reposync_core::sync_engine::SyncEngine::new(
            config,
            Database::new(f.temp.path().join("reposync.db")).unwrap(),
            reposync_core::svn::SvnClient::new(svn_url, "", ""),
            reposync_core::git::GitClient::new(&path).unwrap(),
            std::sync::Arc::new(
                reposync_core::identity::mapper::IdentityMapper::new(
                    &reposync_core::config::IdentityConfig {
                        email_domain: Some("example.invalid".into()),
                        ..Default::default()
                    },
                )
                .unwrap(),
            ),
        );
        engine.set_repo_id(id.to_owned());
        engine.set_git_credential_apply_clean(true);
        (engine, path)
    }

    fn parent_install_bootstrap(f: &Fixture, history: &PairRefreshOperation) {
        use reposync_core::db::repo_scope_identity as identity;

        let parent = &history.pins.parent;
        assert_eq!(parent.repo_id, "parent");
        assert_eq!(parent.last_git_sha, f.parent.to_string());
        assert_eq!(
            f.db.get_repo_watermark("parent").unwrap(),
            (parent.last_svn_rev, parent.last_git_sha.clone())
        );
        assert_eq!(
            f.target.find_reference("refs/heads/main").unwrap().target(),
            Some(f.parent)
        );
        let scope = identity::repository_scope_uuid(&f.db.conn(), "parent").unwrap();
        assert_eq!(scope, parent.scope_uuid);
        let key = identity::last_git_sha_kv_key(&scope);
        assert_eq!(
            f.db.get_state(&key).unwrap().as_deref(),
            Some(parent.last_git_sha.as_str())
        );
        assert_eq!(f.db.get_state("last_git_sha_parent").unwrap(), None);
        let rows: i64 =
            f.db.conn()
                .query_row(
                    "SELECT count(*) FROM sync_records WHERE repo_id='parent'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
        assert_eq!(rows, 0, "bootstrap must precede all ordinary parent work");
        let no_target =
            identity::repository_has_stored_no_target_receipt_kv(&f.db.conn(), "parent").unwrap();
        assert!(!no_target);
        assert_eq!(f.db.get_state("handled_git_baseline_parent").unwrap(), None);
        f.db.delete_kv_state(&key).unwrap();
        assert_eq!(
            reposync_core::echo_receipt_scope::read_team_inbound_git_checkpoint_kv(&f.db, "parent")
                .unwrap(),
            None
        );
    }

    struct Ready {
        history: PairRefreshOperation,
        child_wc: PathBuf,
        parent_bridge: PathBuf,
        child_bridge: PathBuf,
    }

    fn local_source_url(f: &Fixture, branch: &str) -> String {
        reqwest::Url::from_file_path(f.temp.path().join("svn-repository").join(branch))
            .unwrap()
            .to_string()
    }

    async fn require_named_source_revision(f: &Fixture, branch: &str) -> i64 {
        let local_url = local_source_url(f, branch);
        let revision = svn_revision(&local_url);
        let local = reposync_core::svn::SvnClient::new(&local_url, "", "");
        let entries = local.log(revision, revision).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].revision, revision);
        assert_eq!(entries[0].author, "m2d-fixture-author");
        let registered = reposync_core::svn::SvnClient::new(format!("{}/{branch}", f.root), "", "");
        let local_info = local.info().await.unwrap();
        let registered_info = registered.info().await.unwrap();
        assert_eq!(local_info.uuid, registered_info.uuid);
        assert_eq!(local_info.latest_rev, registered_info.latest_rev);
        assert_eq!(local_info.latest_rev, revision);
        assert_eq!(
            registered_info.url.trim_end_matches('/'),
            format!("{}/{branch}", f.root)
        );
        revision
    }

    async fn ready(f: &Fixture, engine: &Arc<reposync_core::sync_engine::SyncEngine>) -> Ready {
        let fence = reposync_core::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
        let history = completed(f, engine, "applicability-genuine-history").await;
        parent_install_bootstrap(f, &history);
        let (parent_engine, parent_bridge) = producer_engine(f, "parent");
        let (child_engine, child_bridge) = producer_engine(f, "child");
        std::fs::write(f._parent_wc.join("file.txt"), b"m2d parent work\n").unwrap();
        std::fs::write(
            f._parent_wc.join("parent-only.txt"),
            b"preserve parent delta\n",
        )
        .unwrap();
        svn(&["add", "parent-only.txt"], Some(&f._parent_wc));
        svn(
            &[
                "commit",
                "--username",
                "m2d-fixture-author",
                "--no-auth-cache",
                "-m",
                "m2d parent source work",
            ],
            Some(&f._parent_wc),
        );
        let parent_source_revision = require_named_source_revision(f, "trunk").await;
        let parent_stats = parent_engine.run_sync_cycle().await.unwrap();
        assert_eq!(parent_stats.svn_to_git_count, 1);
        assert_eq!(parent_stats.git_to_svn_count, 0);
        // The actual ordinary cycle, not fixture SQL, must now create the
        // mapped handled baseline. Keep the emitted tip distinct from inbound KV.
        let (parent_revision, parent_sha) = f.db.get_repo_watermark("parent").unwrap();
        let baseline: serde_json::Value = serde_json::from_str(
            &f.db
                .get_state("handled_git_baseline_parent")
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(baseline["version"], 1);
        assert_eq!(baseline["repo_id"], "parent");
        assert_eq!(baseline["git_sha"].as_str(), Some(parent_sha.as_str()));
        assert_eq!(baseline["svn_rev"].as_i64(), Some(parent_revision));
        assert_eq!(parent_revision, parent_source_revision);
        let child_wc = f.temp.path().join("m2d-child-wc");
        svn(
            &[
                "checkout",
                &local_source_url(f, "branches/f"),
                child_wc.to_str().unwrap(),
            ],
            None,
        );
        std::fs::write(child_wc.join("file.txt"), b"m2d child work\n").unwrap();
        std::fs::write(child_wc.join("child-only.txt"), b"preserve child delta\n").unwrap();
        svn(&["add", "child-only.txt"], Some(&child_wc));
        svn(
            &[
                "commit",
                "--username",
                "m2d-fixture-author",
                "--no-auth-cache",
                "-m",
                "m2d child source work",
            ],
            Some(&child_wc),
        );
        let child_source_revision = require_named_source_revision(f, "branches/f").await;
        let child_stats = child_engine.run_sync_cycle().await.unwrap();
        assert_eq!(child_stats.svn_to_git_count, 1);
        assert_eq!(child_stats.git_to_svn_count, 0);
        assert_eq!(
            f.db.get_repo_watermark("child").unwrap().0,
            child_source_revision
        );
        drop(fence);
        let current =
            f.db.capture_pair_refresh_execution_configuration("child")
                .unwrap();
        for registration in [&current.pin().child, &current.pin().parent] {
            let rows: i64 = f.db.conn().query_row(
                "SELECT count(*) FROM sync_records WHERE repo_id=?1 AND svn_rev=?2 AND git_sha=?3 AND direction='svn_to_git' AND status='applied'",
                rusqlite::params![registration.repo_id, registration.last_svn_rev, registration.last_git_sha],
                |row| row.get(0),
            ).unwrap();
            assert_eq!(
                rows, 1,
                "actual ordinary producer mapping for {}",
                registration.repo_id
            );
            assert_eq!(
                f.target
                    .find_reference(&format!(
                        "refs/heads/{}",
                        if registration.repo_id == "child" {
                            "feature"
                        } else {
                            "main"
                        }
                    ))
                    .unwrap()
                    .target()
                    .unwrap()
                    .to_string(),
                registration.last_git_sha,
            );
        }
        assert_eq!(f.db.pair_refresh_operation(history.id).unwrap(), history);
        Ready {
            history,
            child_wc,
            parent_bridge,
            child_bridge,
        }
    }

    async fn completed(
        f: &Fixture,
        engine: &Arc<reposync_core::sync_engine::SyncEngine>,
        request: &str,
    ) -> PairRefreshOperation {
        let service = RefreshService::with_review_fixture_client(engine.clone(), f.client.clone());
        let approval = match service
            .prepare(
                "disposable-workspace-session".into(),
                "child".into(),
                request.into(),
            )
            .await
            .unwrap()
        {
            RefreshResponse::Approval(approval) => approval,
            RefreshResponse::Receipt(_) => {
                panic!("new fixture request unexpectedly already completed")
            }
        };
        assert_eq!(approval.kind, "integration");
        let receipt = service
            .act(
                "disposable-workspace-session".into(),
                "child",
                request,
                approval.preparation_id,
                approval.approval_digest.clone(),
                RefreshAction::Execute,
            )
            .await
            .unwrap();
        assert_eq!(receipt.phase, "completed");
        service.shutdown().await;
        let history = f.db.pair_refresh_operation(approval.operation_id).unwrap();
        assert_eq!(history.phase, PairRefreshPhase::Completed);
        history
    }

    fn install_bridges(f: &Fixture) {
        let pin =
            f.db.capture_pair_refresh_execution_configuration("child")
                .unwrap();
        for (id, branch) in [("child", "feature"), ("parent", "main")] {
            let path = f.temp.path().join("repos").join(id).join("git-repo");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let repository = git2::build::RepoBuilder::new()
                .branch(branch)
                .clone(f.target.path().to_str().unwrap(), &path)
                .unwrap();
            repository
                .remote_set_url("origin", &pin.pin().git_remote_url)
                .unwrap();
        }
    }

    fn candidate_body(review: &serde_json::Value, choice: &str) -> serde_json::Value {
        let manifest = &review["applicability"];
        let choices = manifest["graph"]["paths"].as_array().unwrap().iter()
            .filter(|row| row["disposition"]["kind"] == "conflict")
            .map(|row| serde_json::json!({"path":row["path"],"baseline":row["baseline"],"child":row["child"],"parent":row["parent"],"choice":choice}))
            .collect::<Vec<_>>();
        assert_eq!(choices.len(), 1);
        serde_json::json!({"review_pin":review["review_pin"],"resolution":{"schema":1,"policy":reposync_core::pair_refresh::preparation::CHANGED_CHILD_RESOLUTION_CANDIDATE_POLICY,"manifest_digest":manifest["manifest_digest"],"choices":choices}})
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn authenticated_http_review_real_sources_choices_staleness_and_no_approval() {
        let f = Fixture::new();
        assert_ne!(f.baseline, f.parent);
        let engine = Arc::new(f.engine());
        let _lock = reposync_core::data_dir::acquire(f.temp.path()).unwrap();
        let ready = ready(&f, &engine).await;
        assert_eq!(ready.history.phase, PairRefreshPhase::Completed);
        assert!(
            ready.parent_bridge.is_dir() && ready.child_bridge.is_dir() && ready.child_wc.is_dir()
        );
        install_bridges(&f);
        let service = RefreshService::with_review_fixture_client(engine.clone(), f.client.clone());
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let state = WebServer::new(
            engine.config().clone(),
            Database::new(f.temp.path().join("reposync.db")).unwrap(),
            engine.clone(),
            tx,
            f.temp.path().join("config.toml"),
            Arc::new(tokio::sync::RwLock::new(
                reposync_core::import::ImportProgress::default(),
            )),
        )
        .app_state();
        let router = api::pair_refresh::review_fixture_routes(service.clone()).with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let root = format!(
            "http://{}/api/v2/repos/child/refresh",
            listener.local_addr().unwrap()
        );
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .unwrap();
        let before = f.state();
        let source_before = fixture_files(f.target.path());
        let svn_before = svn_revision(&f.root);
        let log_before = std::fs::read_to_string(&f.log).unwrap();
        let refs = ["refs/heads/feature", "refs/heads/main"]
            .map(|name| f.target.find_reference(name).unwrap().target().unwrap());
        let response = client
            .post(format!("{root}/review"))
            .bearer_auth("disposable-workspace-session")
            .json(&serde_json::json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let review: serde_json::Value = response.json().await.unwrap();
        assert_eq!(review["state"], "ready");
        assert_eq!(review["purpose"], "inspection_only");
        assert_eq!(review["actor_approval"], false);
        assert_eq!(review["execution_eligible"], false);
        assert_eq!(review["pending"]["child"]["git"]["pending_count"], 0);
        assert_eq!(review["pending"]["parent"]["git"]["pending_count"], 0);
        for (choice, role) in [
            ("base", "integrated_child"),
            ("child", "current_child"),
            ("parent", "current_parent"),
        ] {
            let response = client
                .post(format!("{root}/review/candidate"))
                .bearer_auth("disposable-workspace-session")
                .json(&candidate_body(&review, choice))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), reqwest::StatusCode::OK);
            let candidate: serde_json::Value = response.json().await.unwrap();
            assert_eq!(candidate["state"], "candidate");
            assert_eq!(
                candidate["candidate"]["graph"]["candidate_tree"],
                expected_candidate_tree(&f, &review["applicability"], role)
            );
            assert_eq!(candidate["actor_approval"], false);
            assert_eq!(candidate["execution_eligible"], false);
        }
        // Stale pins/triples and incomplete/duplicate/unknown choices are
        // refused through the actual HTTP parser + live registered factory.
        let original = candidate_body(&review, "child");
        let mut invalid = Vec::new();
        let mut body = original.clone();
        body["review_pin"] = "0".repeat(64).into();
        invalid.push(body);
        let mut body = original.clone();
        body["resolution"]["manifest_digest"] = "0".repeat(64).into();
        invalid.push(body);
        let mut body = original.clone();
        body["resolution"]["choices"] = serde_json::json!([]);
        invalid.push(body);
        let mut body = original.clone();
        let duplicate = body["resolution"]["choices"][0].clone();
        body["resolution"]["choices"]
            .as_array_mut()
            .unwrap()
            .push(duplicate);
        invalid.push(body);
        let mut body = original.clone();
        body["resolution"]["choices"][0]["path"] = "unknown.txt".into();
        invalid.push(body);
        let mut body = original;
        body["resolution"]["choices"][0]["baseline"] = serde_json::json!({"kind":"absent"});
        invalid.push(body);
        for body in invalid {
            let response = client
                .post(format!("{root}/review/candidate"))
                .bearer_auth("disposable-workspace-session")
                .json(&body)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
        }
        let response = client.post(format!("{root}/action")).bearer_auth("disposable-workspace-session").json(&serde_json::json!({"request_id":"view-not-approval","preparation_id":Uuid::new_v4(),"approval_digest":review["review_pin"],"action":"execute"})).send().await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
        assert_eq!(f.state(), before);
        assert_eq!(
            ["refs/heads/feature", "refs/heads/main"].map(|name| f
                .target
                .find_reference(name)
                .unwrap()
                .target()
                .unwrap()),
            refs
        );
        let log = std::fs::read_to_string(&f.log).unwrap();
        // Setup deliberately published once; subsequent checks above compare
        // actual source refs and DB, not an HTTP-only publication counter.
        assert!(log.contains("git-upload-pack"));
        let path = f.temp.path().join("repos/child/git-repo/late.txt");
        std::fs::write(&path, b"preserved pending work").unwrap();
        let response = client
            .post(format!("{root}/review/candidate"))
            .bearer_auth("disposable-workspace-session")
            .json(&candidate_body(&review, "child"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let blocked: serde_json::Value = response.json().await.unwrap();
        assert_eq!(blocked["state"], "blocked");
        assert!(blocked["candidate"].is_null() && blocked["review_pin"].is_null());
        assert_eq!(std::fs::read(&path).unwrap(), b"preserved pending work");
        assert_eq!(f.state(), before);
        assert_eq!(fixture_files(f.target.path()), source_before);
        assert_eq!(svn_revision(&f.root), svn_before);
        let log_after = std::fs::read_to_string(&f.log).unwrap();
        assert!(log_after.starts_with(&log_before));
        assert!(
            !log_after[log_before.len()..].contains("git-receive-pack"),
            "read phase must not attempt ordinary Git publication"
        );
        service.shutdown().await;
        server.abort();
        assert!(server.await.unwrap_err().is_cancelled());
    }
    fn expected_candidate_tree(f: &Fixture, manifest: &serde_json::Value, role: &str) -> String {
        let roles = &manifest["graph"]["roles"];
        let mut bytes = Vec::new();
        for (name, source_role) in [
            ("child-only.txt", "current_child"),
            ("file.txt", role),
            ("parent-only.txt", "current_parent"),
        ] {
            let tree = f
                .target
                .find_tree(Oid::from_str(roles[source_role]["tree"].as_str().unwrap()).unwrap())
                .unwrap();
            let entry = tree.get_name(name).unwrap();
            assert_eq!(entry.filemode(), 0o100644);
            bytes.extend_from_slice(format!("100644 {name}\0").as_bytes());
            bytes.extend_from_slice(entry.id().as_bytes());
        }
        // Hash canonical tree bytes only. Do not write an expected object into
        // the source ODB while testing a read-only operation.
        Oid::hash_object(git2::ObjectType::Tree, &bytes)
            .unwrap()
            .to_string()
    }

    fn fixture_files(root: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
        fn visit(
            root: &Path,
            directory: &Path,
            out: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>,
        ) {
            for entry in std::fs::read_dir(directory).unwrap() {
                let entry = entry.unwrap();
                let kind = entry.file_type().unwrap();
                assert!(!kind.is_symlink());
                if kind.is_dir() {
                    visit(root, &entry.path(), out);
                } else {
                    assert!(kind.is_file());
                    out.insert(
                        entry.path().strip_prefix(root).unwrap().to_owned(),
                        std::fs::read(entry.path()).unwrap(),
                    );
                }
            }
        }
        let mut files = std::collections::BTreeMap::new();
        visit(root, root, &mut files);
        files
    }
}
