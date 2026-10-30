//! Real import, publisher and ordinary SyncEngine regression fixture.
use super::{fixture_process, run, svn};
use crate::config::{AppConfig, IdentityConfig};
use crate::db::Database;
use crate::git::GitClient;
use crate::identity::IdentityMapper;
use crate::import::{ImportConfig, ImportOutcome, ImportProgress, ImportRunState};
use crate::svn::SvnClient;
use crate::sync_engine::SyncEngine;
use anyhow::{Context, Result};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

struct Fixture {
    db: Database,
    target: git2::Repository,
    svn_root: String,
    parent_wc: PathBuf,
    client: reqwest::Client,
    _server: fixture_process::OwnedProcessGroup,
    parent_engine: SyncEngine,
    child_engine: SyncEngine,
    baseline: String,
    _lock: crate::data_dir::LockGuard,
    _fence: crate::writer_fence::WriterFenceGuard,
    temp: tempfile::TempDir,
}

fn engine(
    config: &AppConfig,
    path: &Path,
    svn: &str,
    git: &Path,
    id: &str,
    identity: &Arc<IdentityMapper>,
) -> Result<SyncEngine> {
    let mut config = config.clone();
    config.svn.url = svn.into();
    config.github.default_branch = if id == "child" { "feature" } else { "main" }.into();
    let mut result = SyncEngine::new(
        config,
        Database::new(path.join("reposync.db"))?,
        SvnClient::new(svn, "", ""),
        GitClient::new(git)?,
        identity.clone(),
    );
    result.set_repo_id(id.into());
    Ok(result)
}

async fn fixture() -> Result<Fixture> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    let db = Database::new(root.join("reposync.db"))?;
    db.initialize()?;
    let lock = crate::data_dir::acquire(&root)?;
    let fence = crate::writer_fence::claim(&db, &root)?;
    let svn_repository = root.join("svn-repository");
    run(Command::new("svnadmin").arg("create").arg(&svn_repository));
    let hook = svn_repository.join("hooks/pre-revprop-change");
    std::fs::write(&hook, "#!/bin/sh\nexit 0\n")?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(hook, std::fs::Permissions::from_mode(0o755))?;
    let svn_root = reqwest::Url::from_file_path(svn_repository.canonicalize()?)
        .map_err(|_| anyhow::anyhow!("SVN file URL"))?
        .to_string();
    svn(
        &[
            "mkdir",
            &format!("{svn_root}/trunk"),
            &format!("{svn_root}/branches"),
            "-m",
            "layout",
        ],
        None,
    );
    let parent_wc = root.join("parent-wc");
    svn(
        &[
            "checkout",
            &format!("{svn_root}/trunk"),
            parent_wc.to_str().unwrap(),
        ],
        None,
    );
    std::fs::write(parent_wc.join("file.txt"), b"baseline\n")?;
    svn(&["add", "file.txt"], Some(&parent_wc));
    svn(&["commit", "-m", "baseline"], Some(&parent_wc));
    let bare = root.join("target.git");
    let target = git2::Repository::init_bare(&bare)?;
    target.set_head("refs/heads/main")?;
    target.config()?.set_bool("http.receivepack", true)?;
    let cert = root.join("cert.pem");
    let key = root.join("key.pem");
    let cnf = root.join("cert.cnf");
    std::fs::write(&cnf,"[req]\ndistinguished_name=dn\nx509_extensions=ext\nprompt=no\n[dn]\nCN=localhost\n[ext]\nsubjectAltName=DNS:localhost,IP:127.0.0.1\nkeyUsage=digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\nbasicConstraints=CA:false\n")?;
    run(Command::new("openssl")
        .args([
            "req", "-x509", "-newkey", "rsa:2048", "-days", "1", "-nodes", "-keyout",
        ])
        .arg(&key)
        .arg("-out")
        .arg(&cert)
        .arg("-config")
        .arg(&cnf));
    let trust_config = PathBuf::from(std::env::var("REPOSYNC_PROBE_GIT_CONFIG")?);
    anyhow::ensure!(
        trust_config.file_name().and_then(|x| x.to_str()) == Some("pair-refresh-probe-git-config"),
        "unexpected probe trust config"
    );
    std::fs::write(
        trust_config,
        format!("[http]\n\tsslCAInfo = {}\n", cert.display()),
    )?;
    let der = root.join("cert.der");
    run(Command::new("openssl")
        .arg("x509")
        .arg("-in")
        .arg(&cert)
        .args(["-outform", "DER", "-out"])
        .arg(&der));
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(30))
        .add_root_certificate(reqwest::Certificate::from_der(&std::fs::read(der)?)?)
        .build()?;
    let mut command = Command::new("python3");
    command
        .arg(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/pair_refresh_http_server.py"),
        )
        .arg(&root)
        .arg("--cert")
        .arg(cert)
        .arg("--key")
        .arg(key)
        .arg("--token")
        .arg("disposable-engine-token")
        .arg("--request-log")
        .arg(root.join("requests.jsonl"))
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let mut server = fixture_process::OwnedProcessGroup::spawn(&mut command)?;
    let mut line = String::new();
    BufReader::new(server.stdout.take().context("server stdout")?).read_line(&mut line)?;
    let ready: serde_json::Value = serde_json::from_str(&line)?;
    let url = ready["url"].as_str().context("fixture URL")?;
    let now = chrono::Utc::now().to_rfc3339();
    let parent = crate::models::Repository {
        id: "parent".into(),
        name: "Real engine parent".into(),
        svn_url: svn_root.clone(),
        svn_branch: "trunk".into(),
        svn_username: String::new(),
        git_provider: "gitea".into(),
        git_api_url: format!("{}/api/v1", url.trim_end_matches("/target.git")),
        git_repo: "target".into(),
        git_branch: "main".into(),
        sync_mode: "team".into(),
        poll_interval_secs: 5,
        lfs_threshold_mb: 0,
        auto_merge: false,
        enabled: true,
        created_by: None,
        parent_id: None,
        created_at: now.clone(),
        updated_at: now,
        last_svn_rev: 0,
        last_git_sha: String::new(),
        last_sync_at: None,
        sync_status: "idle".into(),
        total_syncs: 0,
        total_errors: 0,
        allowed_paths: None,
        blocked_patterns: None,
        consecutive_errors: 0,
        teams_webhook_url: None,
    };
    db.insert_repository(&parent)?;
    db.set_state("encryption_key", &"51".repeat(32))?;
    db.set_state("secret_git_token_parent", "disposable-engine-token")?;
    let parent_git = root.join("repos/parent/git-repo");
    std::fs::create_dir_all(parent_git.parent().unwrap())?;
    let git = GitClient::init(&parent_git)?;
    git.ensure_head_on_branch("main")?;
    git2::Repository::open(&parent_git)?.remote("origin", bare.to_str().unwrap())?;
    let svn_author =
        run(Command::new("svn").args(["propget", "--revprop", "-r", "2", "svn:author", &svn_root]))
            .trim()
            .to_owned();
    anyhow::ensure!(!svn_author.is_empty(), "fixture SVN author missing");
    let mappings = std::collections::HashMap::from([(
        svn_author,
        crate::identity::mapping_file::AuthorEntry {
            name: "Fixture SVN author".into(),
            email: "svn-author@example.invalid".into(),
        },
    )]);
    let mapping_file = root.join("authors.toml");
    crate::identity::mapping_file::MappingFile::save(&mapping_file, &mappings)?;
    let identity = Arc::new(IdentityMapper::new(&IdentityConfig {
        mapping_file: Some(mapping_file),
        ..IdentityConfig::default()
    })?);
    let op = db.create_import_operation(
        "parent",
        "admin",
        "real-engine-initial-import",
        &crate::db::import_operations::import_target_fingerprint(&parent, &parent_git),
    )?;
    db.start_import_operation("parent", &op.id)?;
    let imported = crate::import::run_full_import(
        &SvnClient::new(format!("{svn_root}/trunk"), "", ""),
        &Arc::new(Mutex::new(git)),
        &identity,
        &db,
        &crate::file_policy::FilePolicy::new(0, Vec::new()),
        &ImportConfig {
            committer_name: "Fixture".into(),
            committer_email: "fixture@example.invalid".into(),
            remote_name: "origin".into(),
            branch: "main".into(),
            push_token: None,
            message_prefix: None,
            trunk_path: "trunk".into(),
        },
        ImportRunState {
            progress: Arc::new(tokio::sync::RwLock::new(ImportProgress::default())),
            ws_broadcast: None,
            repo_id: Some("parent".into()),
            operation_id: Some(op.id.clone()),
            cancel_signal: None,
        },
    )
    .await
    .context("actual managed full import")?;
    let baseline = match imported {
        ImportOutcome::Completed {
            commits,
            svn_rev,
            git_sha,
        } => {
            assert_eq!(commits, 2);
            assert_eq!(svn_rev, 2);
            db.complete_import_operation("parent", &op.id, svn_rev, &git_sha)?;
            println!("PROBE import commits={commits} svn={svn_rev} git={git_sha}");
            git_sha
        }
        other => anyhow::bail!("actual import incomplete: {other:?}"),
    };
    target.reference(
        "refs/heads/feature",
        git2::Oid::from_str(&baseline)?,
        false,
        "fixture developer creates child Git branch",
    )?;
    let child_git = root.join("repos/child/git-repo");
    std::fs::create_dir_all(child_git.parent().unwrap())?;
    GitClient::clone_repo(bare.to_str().unwrap(), &child_git, None)?;
    run(Command::new("git").arg("-C").arg(&child_git).args([
        "checkout",
        "-B",
        "feature",
        "origin/feature",
    ]));
    let mut config: AppConfig = toml::from_str(
        r#"[daemon]
data_dir="/unused"
[svn]
url="file:///unused"
username=""
password_env=""
layout="custom"
[github]
repo="fixture/repo"
default_branch="main"
token_env=""
"#,
    )?;
    config.daemon.data_dir = root.clone();
    let parent = db.get_repository("parent")?.context("parent row")?;
    let mappings = crate::late_pair::collect_verified_mappings(&db, "parent")?;
    let request = crate::late_pair::LatePairRequest {
        parent_id: "parent".into(),
        git_branch: "feature".into(),
        svn_branch: "branches/f".into(),
        skip_import: false,
        compatibility_skip_import: false,
        dry_run: false,
    };
    let plan =
        crate::late_pair::evaluate_admission(&mappings, Some(&parent_git), None, &request, None)
            .map_err(|e| anyhow::anyhow!(e.error_message()))?;
    let source = format!("{svn_root}/trunk");
    let destination = format!("{svn_root}/branches/f");
    let (probe, _) = crate::late_pair::probe_svn_target(
        &SvnClient::new(&source, "", ""),
        &SvnClient::new(&destination, "", ""),
        &source,
        &destination,
        plan.svn_source_revision,
    )
    .await;
    let published = crate::late_pair_publish::publish_admitted_late_pair(
        &db,
        &config,
        &crate::late_pair_publish::PublishCredentials {
            svn_password: String::new(),
        },
        &parent,
        &request,
        &plan,
        &probe,
        "child",
        "admin",
        "real-engine-child-create",
        &identity,
    )
    .await
    .map_err(|e| anyhow::anyhow!(e.error_message()))?;
    assert!(published.published);
    assert_eq!(published.svn_target_revision, Some(3));
    let created = (chrono::Utc::now() - chrono::Duration::minutes(2)).to_rfc3339();
    db.insert_user(&crate::models::User {
        id: "admin".into(),
        username: "admin".into(),
        display_name: "Fixture".into(),
        email: "fixture@example.invalid".into(),
        password_hash: "fixture".into(),
        role: "admin".into(),
        enabled: true,
        created_at: created.clone(),
        updated_at: created,
    })?;
    db.insert_session(&crate::models::Session {
        token: "real-engine-session".into(),
        user_id: "admin".into(),
        created_at: (chrono::Utc::now() - chrono::Duration::minutes(1)).to_rfc3339(),
        expires_at: (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
    })?;
    let parent_engine = engine(&config, &root, &source, &parent_git, "parent", &identity)?;
    let child_engine = engine(&config, &root, &destination, &child_git, "child", &identity)?;
    Ok(Fixture {
        db,
        target,
        svn_root,
        parent_wc,
        client,
        _server: server,
        parent_engine,
        child_engine,
        baseline,
        _lock: lock,
        _fence: fence,
        temp,
    })
}

fn quiet(stats: &crate::sync_engine::SyncStats) {
    assert_eq!(stats.svn_to_git_count, 0);
    assert_eq!(stats.git_to_svn_count, 0);
    assert_eq!(stats.conflicts_detected, 0);
    assert!(!stats.git_replay_has_more);
    assert!(!stats.deferred_mixed_pending);
}

fn publications(f: &Fixture) -> Result<usize> {
    Ok(
        std::fs::read_to_string(f.temp.path().join("requests.jsonl"))?
            .lines()
            .filter(|line| line.contains("/git-receive-pack"))
            .count(),
    )
}

fn checkpoint(f: &Fixture, id: &str, revision: i64, sha: &str) -> Result<()> {
    assert_eq!(f.db.get_repo_watermark(id)?, (revision, sha.to_owned()));
    assert_eq!(
        f.db.get_state(&format!("last_svn_rev_{id}"))?,
        Some(revision.to_string())
    );
    assert_eq!(
        f.db.get_state(&f.db.scoped_last_git_sha_kv_key_for_repo(id)?)?,
        Some(sha.to_owned())
    );
    Ok(())
}

#[test]
fn qualified_engine_driver() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("pair-refresh-probe-git-config");
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!(
                "{}::qualified_real_engine_worker",
                module_path!().split_once("::").unwrap().1
            ),
            "--nocapture",
            "--test-threads=1",
        ])
        .env("REPOSYNC_REAL_ENGINE_FIXTURE", "1")
        .env("GIT_CONFIG_GLOBAL", &config)
        .env("REPOSYNC_PROBE_GIT_CONFIG", &config)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8(output.stdout)
        .unwrap()
        .contains("REPOSYNC_REAL_ENGINE_TWO_REFRESHES_VERIFIED"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn qualified_real_engine_worker() -> Result<()> {
    if std::env::var("REPOSYNC_REAL_ENGINE_FIXTURE")
        .ok()
        .as_deref()
        != Some("1")
    {
        return Ok(());
    }
    let f = fixture().await?;
    checkpoint(&f, "parent", 2, &f.baseline)?;
    checkpoint(&f, "child", 3, &f.baseline)?;
    quiet(&f.child_engine.run_sync_cycle().await?);
    assert_eq!(publications(&f)?, 0);
    let child_git = f.temp.path().join("repos/child/git-repo");
    let child_remote = git2::Repository::open(&child_git)?
        .find_remote("origin")?
        .url()
        .unwrap()
        .to_owned();
    // Both ordinary local transport and the isolated HTTPS publication fixture
    // address this exact physical bare repository, not separate lookalikes.
    assert_eq!(
        Path::new(&child_remote).canonicalize()?,
        f.target.path().canonicalize()?
    );
    let mut prior_child = f.baseline.clone();
    let mut prior_parent = f.baseline.clone();
    for iteration in 1..=2 {
        let parent_revision = 2 + iteration * 2;
        let child_revision = parent_revision + 1;
        let previous_coordinate = f.db.get_repo_watermark("parent")?;
        assert_eq!(previous_coordinate.1, prior_parent);
        // Private actor-policy capture proves this actual engine is unfiltered;
        // derive the expected projection here, never from the proof being tested.
        let policy = f.parent_engine.capture_pair_refresh_actor_policy("real-engine-session")?;
        policy.require_current(&f.parent_engine)?;
        drop(policy);
        let projection = serde_json::json!({
            "allowed_paths": Vec::<String>::new(),
            "blocked_patterns": Vec::<String>::new(),
        }).to_string();
        let name = format!("parent-{iteration}.txt");
        std::fs::write(f.parent_wc.join(&name), format!("advance {iteration}\n"))?;
        svn(&["add", &name], Some(&f.parent_wc));
        svn(
            &["commit", "-m", &format!("advance {iteration}")],
            Some(&f.parent_wc),
        );
        let stats = f.parent_engine.run_sync_cycle().await?;
        assert_eq!(stats.svn_to_git_count, 1);
        assert_eq!(stats.git_to_svn_count, 0);
        assert_eq!(stats.conflicts_detected, 0);
        let parent_tip = f.db.get_repo_watermark("parent")?.1;
        assert_ne!(parent_tip, prior_parent);
        // Same-cycle convergence is legitimate only with the real durable proof,
        // its completed producer/mapping dependencies and coherent checkpoints.
        {
            let conn = f.db.conn();
            assert!(crate::db::ordinary_writer_windows::OrdinaryWriterWindow::verified_emitted_git_frontier(
                &conn, &f.db, "parent", &parent_tip, &projection,
            )?);
        }
        let scope = crate::db::repo_scope_identity::repository_scope_uuid(&f.db.conn(), "parent")?;
        let generation = crate::echo_receipt_scope::repo_echo_generation(&f.db, "parent")?;
        let proof_key = format!("handled_git_emitted_frontier_v1:{scope}:g{generation}:{parent_tip}");
        let proof_raw = f.db.get_state(&proof_key)?.context("actual parent convergence proof missing")?;
        let proof: serde_json::Value = serde_json::from_str(&proof_raw)?;
        assert_eq!(proof["payload"]["repo_id"], "parent");
        assert_eq!(proof["payload"]["scope_uuid"], scope);
        assert_eq!(proof["payload"]["generation"], generation);
        assert_eq!(proof["payload"]["projection"], projection);
        assert_eq!(proof["payload"]["old_frontier"], prior_parent);
        assert_eq!(proof["payload"]["old_inbound"], prior_parent);
        assert_eq!(proof["payload"]["old_svn_revision"], previous_coordinate.0);
        assert_eq!(proof["payload"]["new_git_sha"], parent_tip);
        assert_eq!(proof["payload"]["new_svn_revision"], parent_revision);
        assert_eq!(
            proof["payload"]["new_git_tree"],
            f.target.find_commit(git2::Oid::from_str(&parent_tip)?)?.tree_id().to_string()
        );
        checkpoint(&f, "parent", parent_revision, &parent_tip)?;
        let legacy_key = crate::db::repo_scope_identity::legacy_last_git_sha_kv_key("parent");
        let legacy_before = f.db.get_state(&legacy_key)?;
        // Authority observation must never revoke a latch or sweep receipts.
        // This fixture has exactly these two registrations and must already
        // have an explicit revoked flag as produced by its real setup/cycle.
        let authority_key = crate::db::repo_scope_identity::LEGACY_REPO_ID_KV_READS_KEY;
        let kv_state = |conn: &rusqlite::Connection| -> Result<String> {
            Ok(conn.query_row(
                "SELECT json_group_array(json_array(key,value,updated_at)) FROM (SELECT key,value,updated_at FROM kv_state ORDER BY key)",
                [], |row| row.get(0),
            )?)
        };
        let observe_authority = |conn: &rusqlite::Connection| -> Result<serde_json::Value> {
            use rusqlite::OptionalExtension;
            let mut query = conn.prepare("SELECT id FROM repositories ORDER BY id")?;
            let repositories = query
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            anyhow::ensure!(
                repositories == vec!["child".to_owned(), "parent".to_owned()],
                "expected exactly the registered parent/child pair"
            );
            let flag: Option<(String, String)> = conn
                .query_row(
                    "SELECT value,updated_at FROM kv_state WHERE key=?1",
                    [authority_key],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            anyhow::ensure!(
                flag.as_ref().is_some_and(|(value, _)| value == "0"),
                "two repositories require explicitly revoked legacy authority"
            );
            let legacy_receipts: String = conn.query_row(
                "SELECT json_group_array(json_array(key,value,updated_at)) FROM (SELECT key,value,updated_at FROM kv_state WHERE key GLOB 'handled_git_no_target_*' OR key GLOB 'handled_svn_no_target_*' OR key GLOB 'reposync_v13_quarantined:*' ORDER BY key)",
                [], |row| row.get(0),
            )?;
            Ok(serde_json::json!({"repositories": repositories, "flag": flag, "legacy_receipts": legacy_receipts}))
        };
        if iteration == 1 {
            // Isolated read-only-observer controls, not new production authority.
            let isolated = rusqlite::Connection::open_in_memory()?;
            isolated.execute_batch(
                "CREATE TABLE repositories(id TEXT PRIMARY KEY,scope_uuid TEXT);
                 CREATE TABLE kv_state(key TEXT PRIMARY KEY,value TEXT,updated_at TEXT);
                 INSERT INTO repositories VALUES('child','aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa');
                 INSERT INTO repositories VALUES('parent','bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb');",
            )?;
            let orphan_key = format!("handled_git_no_target_retired_1_{}", "a".repeat(40));
            isolated.execute(
                "INSERT INTO kv_state VALUES(?1,?2,?3)",
                rusqlite::params![orphan_key, "{\"repo_id\":\"retired\",\"version\":1}", "2026-10-30T00:00:00Z"],
            )?;
            for value in [Some("0"), Some("1"), None, Some("unknown")] {
                isolated.execute("DELETE FROM kv_state WHERE key=?1", [authority_key])?;
                if let Some(value) = value {
                    isolated.execute(
                        "INSERT INTO kv_state VALUES(?1,?2,?3)",
                        rusqlite::params![authority_key, value, "2026-10-30T00:00:00Z"],
                    )?;
                }
                let before = kv_state(&isolated)?;
                let observed = observe_authority(&isolated);
                if value == Some("0") {
                    assert!(observed.is_ok(), "valid revoked state must be observable");
                } else {
                    let error = observed.err().context("inconsistent authority was accepted")?;
                    assert!(error.to_string().contains("explicitly revoked legacy authority"));
                }
                // Exact flag value/timestamp/absence, orphan and every other KV
                // row must survive either success or refusal without quarantine.
                assert_eq!(kv_state(&isolated)?, before);
                assert_eq!(isolated.query_row(
                    "SELECT count(*) FROM kv_state WHERE key=?1", [&orphan_key], |row| row.get::<_, i64>(0),
                )?, 1);
                assert_eq!(isolated.query_row(
                    "SELECT count(*) FROM kv_state WHERE key GLOB 'reposync_v13_quarantined:*'", [], |row| row.get::<_, i64>(0),
                )?, 0);
            }
        }
        let authority_before = {
            let conn = f.db.conn();
            let before = kv_state(&conn)?;
            let observed = observe_authority(&conn)?;
            assert_eq!(kv_state(&conn)?, before, "observation must not repair producer state");
            observed
        };
        // Observe the physical bare target and journal/mapping identities.
        // HTTP receive-pack counts alone cannot see ordinary local Git pushes.
        let target_state = || -> Result<serde_json::Value> {
            let mut references = Vec::new();
            let mut walk = f.target.revwalk()?;
            for reference in f.target.references()? {
                let reference = reference?;
                let name = reference.name().context("non-UTF8 target reference")?.to_owned();
                walk.push(reference.peel_to_commit()?.id())?;
                references.push((
                    name,
                    reference.target().map(|id| id.to_string()),
                    reference.symbolic_target().map(str::to_owned),
                ));
            }
            assert!(!references.is_empty());
            references.sort();
            let mut commits = walk
                .collect::<std::result::Result<Vec<_>, _>>()?
                .into_iter()
                .map(|id| id.to_string())
                .collect::<Vec<_>>();
            commits.sort();
            let head = f.target.find_reference("HEAD")?;
            Ok(serde_json::json!({
                "head": (head.target().map(|id| id.to_string()), head.symbolic_target()),
                "references": references,
                "reachable_commits": commits,
            }))
        };
        let publication_state = || -> Result<(String, String)> {
            let conn = f.db.conn();
            let operations = conn.query_row(
                "SELECT json_group_array(json_array(key,value,updated_at)) FROM (SELECT key,value,updated_at FROM kv_state WHERE key GLOB 'svn_to_git_push_v1:document:*' OR key GLOB 'git_to_svn_commit_v1:document:*' ORDER BY key)",
                [], |row| row.get(0),
            )?;
            let mappings = conn.query_row(
                "SELECT json_group_array(json_array(id,repo_id,svn_rev,git_sha,direction,author,message,timestamp,synced_at,status)) FROM (SELECT * FROM sync_records ORDER BY id)",
                [], |row| row.get(0),
            )?;
            Ok((operations, mappings))
        };
        let target_before = target_state()?;
        let publication_before = publication_state()?;
        let svn_before = SvnClient::new(&f.svn_root, "", "").info().await?.latest_rev;
        let http_before = publications(&f)?;
        quiet(&f.parent_engine.run_sync_cycle().await?);
        checkpoint(&f, "parent", parent_revision, &parent_tip)?;
        assert_eq!(target_state()?, target_before);
        assert_eq!(publication_state()?, publication_before);
        assert_eq!(SvnClient::new(&f.svn_root, "", "").info().await?.latest_rev, svn_before);
        assert_eq!(publications(&f)?, http_before);
        assert_eq!(f.db.get_state(&proof_key)?.as_deref(), Some(proof_raw.as_str()));
        assert_eq!(f.db.get_state(&legacy_key)?, legacy_before);
        let authority_after = {
            let conn = f.db.conn();
            let before = kv_state(&conn)?;
            let observed = observe_authority(&conn)?;
            assert_eq!(kv_state(&conn)?, before, "observation must remain read-only");
            observed
        };
        assert_eq!(authority_after, authority_before);
        // These checks reject new history/durable publication; they do not count
        // an idempotent same-tip local transport attempt with no durable change.
        let parent_before = serde_json::to_value(f.db.get_repository("parent")?.unwrap())?;
        let actor = f
            .parent_engine
            .capture_pair_refresh_actor_policy("real-engine-session")?;
        let workspace = actor.create_workspace(&f.parent_engine)?;
        let prepared = super::super::prepare_registered_in_workspace_for_test(
            &f.db,
            "child",
            workspace,
            f.client.clone(),
        )
        .await?;
        assert_eq!(prepared.pins().git.pins.child_tip, prior_child);
        assert_eq!(prepared.pins().git.pins.parent_tip, parent_tip);
        assert_eq!(prepared.pins().git.pins.parent_baseline_tip, prior_parent);
        assert_eq!(
            prepared
                .pins()
                .svn
                .pins
                .lineage
                .child_copy
                .copy_from_revision,
            2
        );
        assert_eq!(
            prepared.pins().svn.pins.lineage.child_copy.created_revision,
            3
        );
        let digest = actor.prepared_approval_digest(&f.parent_engine, &prepared)?;
        let busy = crate::busy::try_acquire_pair("child", "parent").context("pair busy")?;
        let completed = crate::pair_refresh::execution::complete_owned_refresh_with_client(
            &f.parent_engine,
            &actor,
            &prepared,
            &format!("engine-refresh-{iteration}"),
            &digest,
            &busy,
            f.client.clone(),
        )
        .await?;
        assert_eq!(
            completed.phase,
            crate::db::pair_refresh_operations::PairRefreshPhase::Completed
        );
        assert_eq!(
            completed.svn_receipt.as_ref().unwrap().revision,
            child_revision
        );
        let merged = completed.pins.git.prepared_sha.clone();
        let commit = f.target.find_commit(git2::Oid::from_str(&merged)?)?;
        assert_eq!(commit.parent_count(), 2);
        assert_eq!(commit.parent_id(0)?.to_string(), prior_child);
        assert_eq!(commit.parent_id(1)?.to_string(), parent_tip);
        assert_eq!(
            commit.tree_id(),
            f.target
                .find_commit(git2::Oid::from_str(&parent_tip)?)?
                .tree_id()
        );
        assert_eq!(
            f.target.find_reference("refs/heads/feature")?.target(),
            Some(commit.id())
        );
        assert_eq!(
            f.target
                .find_reference("refs/heads/main")?
                .target()
                .unwrap()
                .to_string(),
            parent_tip
        );
        assert_eq!(publications(&f)?, iteration as usize);
        checkpoint(&f, "child", child_revision, &merged)?;
        assert_eq!(
            serde_json::to_value(f.db.get_repository("parent")?.unwrap())?,
            parent_before
        );
        assert!(f
            .db
            .has_repo_emitted_svn_revision("child", child_revision)?);
        assert!(!f
            .db
            .has_repo_emitted_svn_revision("parent", child_revision)?);
        assert_eq!(
            crate::svn::SvnClient::new(&f.svn_root, "", "")
                .info()
                .await?
                .latest_rev,
            child_revision
        );
        let repeated = crate::pair_refresh::execution::complete_owned_refresh_with_client(
            &f.parent_engine,
            &actor,
            &prepared,
            &format!("engine-refresh-{iteration}"),
            &digest,
            &busy,
            f.client.clone(),
        )
        .await?;
        assert_eq!(repeated, completed);
        assert_eq!(publications(&f)?, iteration as usize);
        assert_eq!(
            crate::svn::SvnClient::new(&f.svn_root, "", "")
                .info()
                .await?
                .latest_rev,
            child_revision
        );
        drop(busy);
        assert_eq!(GitClient::new(&child_git)?.head_sha()?, prior_child);
        // Signed completion must not permit destruction of dirty bridge data.
        let original = std::fs::read(child_git.join("file.txt"))?;
        std::fs::write(child_git.join("file.txt"), b"unpublished child data\n")?;
        let refused = f
            .child_engine
            .run_sync_cycle()
            .await
            .unwrap_err()
            .to_string();
        assert!(refused.contains("local_dirty"), "{refused}");
        assert_eq!(
            std::fs::read(child_git.join("file.txt"))?,
            b"unpublished child data\n"
        );
        assert_eq!(GitClient::new(&child_git)?.head_sha()?, prior_child);
        checkpoint(&f, "child", child_revision, &merged)?;
        std::fs::write(child_git.join("file.txt"), original)?;
        quiet(
            &f.child_engine
                .run_sync_cycle()
                .await
                .context("clean child cycle after restored file")?,
        );
        assert_eq!(GitClient::new(&child_git)?.head_sha()?, merged);
        for index in 1..=iteration {
            assert_eq!(
                std::fs::read_to_string(child_git.join(format!("parent-{index}.txt")))?,
                format!("advance {index}\n")
            );
        }
        assert_eq!(publications(&f)?, iteration as usize);
        let child_before = f.db.get_repo_watermark("child")?;
        let workspace = actor.create_workspace(&f.parent_engine)?;
        let noop = crate::pair_refresh::noop::prepare_registered_noop_with_client(
            &f.db,
            "child",
            workspace,
            f.client.clone(),
        )
        .await?;
        let digest = actor.noop_approval_digest(&f.parent_engine, &noop)?;
        let busy = crate::busy::try_acquire_pair("child", "parent").context("noop busy")?;
        let receipt = crate::pair_refresh::noop::complete_owned_noop_with_client(
            &f.parent_engine,
            &actor,
            &noop,
            &format!("engine-noop-{iteration}"),
            &digest,
            &busy,
            f.client.clone(),
        )
        .await?;
        assert_ne!(receipt.id, completed.id);
        assert_eq!(receipt.pins.baseline_operation_id, Some(completed.id));
        assert_eq!(f.db.get_repo_watermark("child")?, child_before);
        assert_eq!(
            serde_json::to_value(f.db.get_repository("parent")?.unwrap())?,
            parent_before
        );
        assert_eq!(publications(&f)?, iteration as usize);
        assert_eq!(
            crate::svn::SvnClient::new(&f.svn_root, "", "")
                .info()
                .await?
                .latest_rev,
            child_revision
        );
        assert!(
            crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&f.db.conn(), None)
                .is_ok()
        );
        prior_child = merged;
        prior_parent = parent_tip;
    }
    println!("REPOSYNC_REAL_ENGINE_TWO_REFRESHES_VERIFIED");
    Ok(())
}

#[test]
fn qualified_git_origin_engine_driver() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("pair-refresh-probe-git-config");
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!(
                "{}::qualified_git_origin_engine_worker",
                module_path!().split_once("::").unwrap().1
            ),
            "--nocapture",
            "--test-threads=1",
        ])
        .env("REPOSYNC_GIT_ORIGIN_ENGINE_FIXTURE", "1")
        .env("GIT_CONFIG_GLOBAL", &config)
        .env("REPOSYNC_PROBE_GIT_CONFIG", &config)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8(output.stdout)
        .unwrap()
        .contains("REPOSYNC_REAL_ENGINE_GIT_ORIGIN_VERIFIED"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn qualified_git_origin_engine_worker() -> Result<()> {
    if std::env::var("REPOSYNC_GIT_ORIGIN_ENGINE_FIXTURE")
        .ok()
        .as_deref()
        != Some("1")
    {
        return Ok(());
    }
    let f = fixture().await?;
    checkpoint(&f, "parent", 2, &f.baseline)?;
    checkpoint(&f, "child", 3, &f.baseline)?;
    quiet(&f.child_engine.run_sync_cycle().await?);
    let mut prior_parent = f.baseline.clone();
    let mut prior_child = f.baseline.clone();
    for iteration in 1..=2 {
        let parent_revision = 2 + iteration * 2;
        let child_revision = parent_revision + 1;
        // A real user changes the remote. Only the ordinary production engine
        // establishes the SVN publication and its database correspondence.
        let name = format!("git-origin-{iteration}.txt");
        let bytes = format!("Git advance {iteration}\n");
        let parent = f.target.find_commit(git2::Oid::from_str(&prior_parent)?)?;
        let mut builder = f.target.treebuilder(Some(&parent.tree()?))?;
        builder.insert(&name, f.target.blob(bytes.as_bytes())?, 0o100644)?;
        let tree = f.target.find_tree(builder.write()?)?;
        let signature = git2::Signature::now("Fixture SVN author", "svn-author@example.invalid")?;
        let parent_tip = f
            .target
            .commit(
                Some("refs/heads/main"),
                &signature,
                &signature,
                &format!("Git parent advance {iteration}"),
                &tree,
                &[&parent],
            )?
            .to_string();
        let stats = f.parent_engine.run_sync_cycle().await?;
        assert_eq!(stats.svn_to_git_count, 0);
        assert_eq!(stats.git_to_svn_count, 1);
        assert_eq!(stats.conflicts_detected, 0);
        assert_eq!(
            f.db.get_repo_watermark("parent")?,
            (parent_revision - 2, parent_tip.clone())
        );
        assert!(f
            .db
            .has_repo_emitted_svn_revision("parent", parent_revision)?);
        quiet(&f.parent_engine.run_sync_cycle().await?);
        checkpoint(&f, "parent", parent_revision, &parent_tip)?;
        let parent_before = serde_json::to_value(f.db.get_repository("parent")?.unwrap())?;
        let actor = f
            .parent_engine
            .capture_pair_refresh_actor_policy("real-engine-session")?;
        let workspace = actor.create_workspace(&f.parent_engine)?;
        let prepared = super::super::prepare_registered_in_workspace_for_test(
            &f.db,
            "child",
            workspace,
            f.client.clone(),
        )
        .await?;
        assert_eq!(prepared.pins().git.pins.parent_tip, parent_tip);
        assert_eq!(prepared.pins().git.pins.parent_baseline_tip, prior_parent);
        assert_eq!(prepared.pins().git.pins.child_tip, prior_child);
        let digest = actor.prepared_approval_digest(&f.parent_engine, &prepared)?;
        let busy = crate::busy::try_acquire_pair("child", "parent").context("pair busy")?;
        let completed = crate::pair_refresh::execution::complete_owned_refresh_with_client(
            &f.parent_engine,
            &actor,
            &prepared,
            &format!("git-engine-refresh-{iteration}"),
            &digest,
            &busy,
            f.client.clone(),
        )
        .await?;
        assert_eq!(
            completed.phase,
            crate::db::pair_refresh_operations::PairRefreshPhase::Completed
        );
        assert_eq!(
            completed.svn_receipt.as_ref().unwrap().revision,
            child_revision
        );
        let merged = completed.pins.git.prepared_sha.clone();
        let commit = f.target.find_commit(git2::Oid::from_str(&merged)?)?;
        assert_eq!(commit.parent_count(), 2);
        assert_eq!(commit.parent_id(0)?.to_string(), prior_child);
        assert_eq!(commit.parent_id(1)?.to_string(), parent_tip);
        assert_eq!(commit.tree_id(), tree.id());
        checkpoint(&f, "child", child_revision, &merged)?;
        assert_eq!(
            serde_json::to_value(f.db.get_repository("parent")?.unwrap())?,
            parent_before
        );
        assert_eq!(publications(&f)?, iteration as usize);
        let retried = crate::pair_refresh::execution::complete_owned_refresh_with_client(
            &f.parent_engine,
            &actor,
            &prepared,
            &format!("git-engine-refresh-{iteration}"),
            &digest,
            &busy,
            f.client.clone(),
        )
        .await?;
        assert_eq!(retried, completed);
        assert_eq!(publications(&f)?, iteration as usize);
        drop(busy);
        quiet(&f.child_engine.run_sync_cycle().await?);
        assert_eq!(
            std::fs::read_to_string(f.temp.path().join("repos/child/git-repo").join(&name))?,
            bytes
        );
        assert_eq!(
            crate::svn::SvnClient::new(&f.svn_root, "", "")
                .info()
                .await?
                .latest_rev,
            child_revision
        );
        prior_parent = parent_tip;
        prior_child = merged;
    }
    // A revision elsewhere in the SVN repository is never a parent cursor.
    svn(
        &[
            "mkdir",
            &format!("{}/unrelated", f.svn_root),
            "-m",
            "unrelated revision",
        ],
        None,
    );
    quiet(&f.parent_engine.run_sync_cycle().await?);
    checkpoint(&f, "parent", 6, &prior_parent)?;
    checkpoint(&f, "child", 7, &prior_child)?;
    assert_eq!(
        crate::svn::SvnClient::new(&f.svn_root, "", "")
            .info()
            .await?
            .latest_rev,
        8
    );
    assert_eq!(publications(&f)?, 2);
    println!("REPOSYNC_REAL_ENGINE_GIT_ORIGIN_VERIFIED");
    Ok(())
}

#[path = "real_engine_restart_tests.rs"]
mod restart;

#[path = "real_engine_receipt_restart_tests.rs"]
mod receipt_restart;
