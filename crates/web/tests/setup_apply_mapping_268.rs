//! #268 follow-up: what `POST /api/setup/apply` may write for identity
//! mappings. It writes only the configured mapping file (or the default file
//! beside the configuration), in the format the identity mapper reads, and a
//! requested path never chooses where it writes; naming another file only
//! produces a warning.
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
    let warnings = json["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 1, "{json}");
    assert!(
        warnings[0]
            .as_str()
            .unwrap()
            .contains("take effect only once"),
        "the default file is not read until configured: {json}"
    );

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
    // The same file spelled through a symlinked directory is the same file.
    let link = f.tmp.path().join("link");
    std::os::unix::fs::symlink(f.tmp.path(), &link).unwrap();
    let via_link = link.join("authors.toml");
    for requested in [
        None,
        Some(configured.to_str().unwrap()),
        Some("authors.toml"),
        Some("sub/../authors.toml"),
        Some(via_link.to_str().unwrap()),
    ] {
        std::fs::create_dir_all(f.tmp.path().join("sub")).unwrap();
        let (status, json) = apply(&f, apply_body(requested, hostile())).await;
        assert_eq!(status, 200, "{requested:?}: {json}");
        assert_eq!(json["warnings"], json!([]), "{requested:?}: {json}");
    }
    let mapper = IdentityMapper::new(&f.state.config.identity).unwrap();
    let identity = mapper.svn_to_git("jdoe").unwrap();
    assert_eq!(identity.email, "jdoe@example.invalid");
    assert!(!f.tmp.path().join("identity-mappings.toml").exists());
}

#[tokio::test]
async fn other_paths_are_never_written_and_are_reported() {
    let f = fixture(Some("authors.toml")).await;
    let configured = f.tmp.path().join("authors.toml");
    let outside = f.tmp.path().join("elsewhere.toml");
    std::fs::create_dir(f.tmp.path().join("sub")).unwrap();
    let candidates: Vec<PathBuf> = vec![
        outside.clone(),
        PathBuf::from("../authors.toml"),
        PathBuf::from("sub/../../authors.toml"),
        PathBuf::from("config.toml"),
        PathBuf::from("/etc/reposync/authors.toml"),
    ];
    for requested in &candidates {
        std::fs::remove_file(&configured).ok();
        let (status, json) =
            apply(&f, apply_body(Some(requested.to_str().unwrap()), hostile())).await;
        assert_eq!(status, 200, "{}: {json}", requested.display());
        let warnings = json["warnings"].as_array().unwrap();
        assert!(
            warnings
                .iter()
                .any(|w| w.as_str().unwrap().contains("was not used")),
            "{}: {json}",
            requested.display()
        );
        // Only the configured file was written.
        assert!(MappingFile::load(&configured).unwrap().contains_key("jdoe"));
    }
    assert!(!outside.exists());
    assert!(!f.tmp.path().join("config.toml").exists());
    assert!(!f.tmp.path().parent().unwrap().join("authors.toml").exists());
    assert!(!f.tmp.path().join("identity-mappings.toml").exists());

    // Naming a path without sending mappings warns nothing and writes nothing.
    std::fs::remove_file(&configured).unwrap();
    let (status, json) = apply(&f, apply_body(Some("elsewhere.toml"), json!([]))).await;
    assert_eq!(status, 200, "{json}");
    assert_eq!(json["warnings"], json!([]), "{json}");
    assert!(!configured.exists());
}

#[tokio::test]
async fn existing_entries_in_the_configured_file_are_kept() {
    let f = fixture(Some("authors.toml")).await;
    let configured = f.tmp.path().join("authors.toml");
    std::fs::write(
        &configured,
        "[authors]\nkeep = { name = \"Kept\", email = \"keep@example.invalid\" }\njdoe = { name = \"Old\", email = \"old@example.invalid\" }\n",
    )
    .unwrap();

    let (status, json) = apply(&f, apply_body(None, hostile())).await;
    assert_eq!(status, 200, "{json}");
    let authors = MappingFile::load(&configured).unwrap();
    assert_eq!(authors.len(), 3, "{authors:?}");
    assert_eq!(authors["keep"].email, "keep@example.invalid");
    assert_eq!(
        authors["jdoe"].email, "jdoe@example.invalid",
        "the wizard's entry wins"
    );

    // Rows with only blank usernames leave the file exactly as it was.
    let before = std::fs::read(&configured).unwrap();
    let blank = json!([{"svn_username": " ", "name": "x", "email": "x@example.invalid"}]);
    let (status, json) = apply(&f, apply_body(None, blank)).await;
    assert_eq!(status, 200, "{json}");
    assert_eq!(std::fs::read(&configured).unwrap(), before);
}

#[tokio::test]
async fn an_unreadable_existing_file_is_left_unchanged() {
    let f = fixture(Some("authors.toml")).await;
    let configured = f.tmp.path().join("authors.toml");
    std::fs::write(&configured, "this is not toml = = =").unwrap();

    let (status, json) = apply(&f, apply_body(None, hostile())).await;
    assert_eq!(status, 200, "{json}");
    let warnings = json["warnings"].as_array().unwrap();
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().unwrap().contains("could not be read")),
        "{json}"
    );
    assert_eq!(
        std::fs::read_to_string(&configured).unwrap(),
        "this is not toml = = ="
    );
}
