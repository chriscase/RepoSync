//! #268 follow-up: what `POST /api/setup/apply` may write for identity
//! mappings. It writes only the configured mapping file (or the default file
//! beside the configuration), in the format the identity mapper reads, and a
//! refused path leaves no configuration behind.
use axum::Router;
use reposync_core::{
    config::{AppConfig, IdentityConfig},
    db::Database,
    git::GitClient,
    identity::{mapping_file::MappingFile, IdentityMapper},
    import::ImportProgress,
    svn::SvnClient,
    sync_engine::SyncEngine,
};
use reposync_web::{api, AppState};
use serde_json::{json, Value};
use std::{collections::HashMap, net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

const TOKEN: &str = "legacy-admin";

struct Fixture {
    addr: SocketAddr,
    state: Arc<AppState>,
    server: tokio::task::JoinHandle<()>,
    tmp: tempfile::TempDir,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

async fn fixture(mapping_file: Option<&str>) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let git_repo_path = tmp.path().join("git-repo");
    git2::Repository::init(&git_repo_path).unwrap();
    let toml_str = format!(
        "[daemon]\ndata_dir = \"{}\"\n[svn]\nurl = \"https://svn.test.invalid/repo\"\nusername = \"u\"\npassword_env = \"\"\n[github]\nrepo = \"test/repo\"\ntoken_env = \"\"\n",
        tmp.path().display().to_string().replace('\\', "/")
    );
    let mut config: AppConfig = toml::from_str(&toml_str).unwrap();
    config.web.admin_password = Some("test-admin-pass".into());
    config.identity.mapping_file = mapping_file.map(|name| tmp.path().join(name));

    let db = Database::in_memory().unwrap();
    db.initialize().unwrap();
    let engine_db = Database::in_memory().unwrap();
    engine_db.initialize().unwrap();
    let engine = Arc::new(SyncEngine::new(
        config.clone(),
        engine_db,
        SvnClient::new("https://svn.test.invalid/repo", "u", ""),
        GitClient::new(&git_repo_path).unwrap(),
        Arc::new(IdentityMapper::new(&IdentityConfig::default()).unwrap()),
    ));
    let (sync_tx, _rx) = tokio::sync::mpsc::channel(1);
    let (ws_tx, _) = tokio::sync::broadcast::channel(16);
    let state = Arc::new(AppState {
        db,
        sync_engine: engine,
        config,
        sync_trigger: sync_tx,
        ws_broadcast: ws_tx,
        sessions: tokio::sync::RwLock::new(HashMap::new()),
        import_progress: Arc::new(tokio::sync::RwLock::new(ImportProgress::default())),
        config_path: tmp.path().join("config.toml"),
        prev_net_snapshot: std::sync::Mutex::new(None),
        repo_import_progress: tokio::sync::RwLock::new(HashMap::new()),
        login_attempts: std::sync::Mutex::new(HashMap::new()),
        import_handles: tokio::sync::Mutex::new(Vec::new()),
        scheduler_service: std::sync::Mutex::new(reposync_web::SchedulerServiceState::running()),
    });
    state.sessions.write().await.insert(
        TOKEN.to_string(),
        chrono::Utc::now() + chrono::Duration::hours(1),
    );
    let app = Router::new()
        .merge(api::setup::routes())
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .ok();
    });
    Fixture {
        addr,
        state,
        server,
        tmp,
    }
}

fn apply_body(mapping_file: Option<&str>, mappings: Value) -> Value {
    let mut body = json!({
        "svn_url": "", "svn_username": "", "git_api_url": "", "git_repo": "fixture/repo",
        "web_admin_password": "new-admin-pass",
        "identity_mappings": mappings,
    });
    if let Some(path) = mapping_file {
        body["identity_mapping_file"] = json!(path);
    }
    body
}

async fn apply(f: &Fixture, body: Value) -> (u16, Value) {
    let resp = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
        .post(format!("http://{}/api/setup/apply", f.addr))
        .bearer_auth(TOKEN)
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    (status, resp.json().await.unwrap_or(Value::Null))
}

fn hostile() -> Value {
    json!([
        {"svn_username": "jdoe", "name": "John \"JD\" Doe\n[web]\nlisten = \"0.0.0.0:1\"", "email": "jdoe@example.invalid"},
        {"svn_username": "evil\"]\n[x", "name": "E", "email": "e@example.invalid"},
        {"svn_username": "  ", "name": "blank", "email": "blank@example.invalid"},
    ])
}

#[tokio::test]
async fn default_target_is_written_in_the_mapper_format_without_injection() {
    let f = fixture(None).await;
    let (status, json) = apply(&f, apply_body(None, hostile())).await;
    assert_eq!(status, 200, "{json}");
    assert_eq!(json["warnings"], json!([]), "{json}");

    let written = f.tmp.path().join("identity-mappings.toml");
    let authors = MappingFile::load(&written).unwrap();
    assert_eq!(authors.len(), 2, "blank usernames are skipped: {authors:?}");
    assert_eq!(
        authors["jdoe"].name,
        "John \"JD\" Doe\n[web]\nlisten = \"0.0.0.0:1\""
    );
    assert_eq!(authors["evil\"]\n[x"].email, "e@example.invalid");
    // Nothing escaped into a top-level table.
    let raw: toml::Value = toml::from_str(&std::fs::read_to_string(&written).unwrap()).unwrap();
    let keys: Vec<_> = raw.as_table().unwrap().keys().cloned().collect();
    assert_eq!(keys, vec!["authors".to_string()]);
}

#[tokio::test]
async fn configured_mapping_file_is_the_one_the_mapper_reads() {
    let f = fixture(Some("authors.toml")).await;
    let configured = f.tmp.path().join("authors.toml");
    for requested in [
        None,
        Some(configured.to_str().unwrap()),
        Some("authors.toml"),
    ] {
        let (status, json) = apply(&f, apply_body(requested, hostile())).await;
        assert_eq!(status, 200, "{requested:?}: {json}");
    }
    let mapper = IdentityMapper::new(&f.state.config.identity).unwrap();
    let identity = mapper.svn_to_git("jdoe").unwrap();
    assert_eq!(identity.email, "jdoe@example.invalid");
    assert!(!f.tmp.path().join("identity-mappings.toml").exists());
}

#[tokio::test]
async fn other_paths_are_refused_before_anything_is_stored() {
    let f = fixture(None).await;
    let outside = f.tmp.path().join("elsewhere.toml");
    let nested = f.tmp.path().join("sub");
    std::fs::create_dir(&nested).unwrap();
    let candidates: Vec<PathBuf> = vec![
        outside.clone(),
        PathBuf::from("../identity-mappings.toml"),
        PathBuf::from("sub/../identity-mappings.toml"),
        PathBuf::from("config.toml"),
        PathBuf::from("/etc/reposync/authors.toml"),
    ];
    for requested in &candidates {
        let (status, json) =
            apply(&f, apply_body(Some(requested.to_str().unwrap()), hostile())).await;
        assert_eq!(status, 400, "{}: {json}", requested.display());
    }
    assert!(!outside.exists());
    assert!(!f.tmp.path().join("config.toml").exists());
    assert!(!f.tmp.path().join("identity-mappings.toml").exists());
    assert!(f.state.db.list_repositories().unwrap().is_empty());
    assert!(f
        .state
        .db
        .get_state("secret_admin_password_hash")
        .unwrap()
        .is_none());
    assert!(f.state.db.get_state("identity_mappings").unwrap().is_none());
}
