//! Real owned preparation enrollment controls; no publication is authorized.
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use git2::{Oid, Repository, Signature, Time};
use uuid::Uuid;

#[path = "../../tests/fixtures/owned_process_group.rs"]
#[allow(clippy::duplicate_mod)]
mod fixture_process;

use super::*;

pub(super) struct SharedWriterTestGuard {
    _guard: std::sync::MutexGuard<'static, ()>,
}

pub(super) struct WriterTestAdmission;
static SERIAL: WriterTestAdmission = WriterTestAdmission;

impl WriterTestAdmission {
    pub(super) async fn lock(&self) -> SharedWriterTestGuard {
        SharedWriterTestGuard {
            _guard: crate::writer_fence::tests::TEST_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        }
    }
}

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
            .join("tests/fixtures/pair_refresh_http_server.py");
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

    fn engine(&self) -> crate::sync_engine::SyncEngine {
        let mut config: crate::config::AppConfig = toml::from_str(
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
            .insert_user(&crate::models::User {
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
            .insert_session(&crate::models::Session {
                token: "disposable-workspace-session".into(),
                user_id: "admin".into(),
                created_at: (chrono::Utc::now() - chrono::Duration::minutes(1)).to_rfc3339(),
                expires_at: (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
            })
            .unwrap();
        let git_dir = self.temp.path().join("engine-git");
        Repository::init(&git_dir).unwrap();
        crate::sync_engine::SyncEngine::new(
            config,
            Database::new(self.temp.path().join("reposync.db")).unwrap(),
            crate::svn::SvnClient::new("file:///unused/svn", "", ""),
            crate::git::client::GitClient::new(git_dir).unwrap(),
            std::sync::Arc::new(
                crate::identity::mapper::IdentityMapper::new(
                    &crate::config::IdentityConfig::default(),
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

fn key_table(f: &Fixture) -> bool {
    f.db.conn()
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='pair_refresh_journal_key_v3')",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

fn journal_state(f: &Fixture) -> Vec<(String, String, String)> {
    f.state()
        .into_iter()
        .filter(|(key, _, _)| key != "writer_fence_v1:lease")
        .collect()
}

async fn prepared<'engine>(
    f: &Fixture,
    engine: &'engine crate::sync_engine::SyncEngine,
) -> (
    super::super::actor_policy::LiveRefreshActorPolicy<'engine>,
    super::super::workspace::RefreshWorkspace,
    LivePreparedRefresh,
) {
    let actor = engine
        .capture_pair_refresh_actor_policy("disposable-workspace-session")
        .unwrap();
    let workspace = actor.create_workspace(engine).unwrap();
    let prepared = prepare_registered_in_workspace_for_test(
        &f.db,
        "child",
        workspace.clone(),
        f.client.clone(),
    )
    .await
    .unwrap();
    (actor, workspace, prepared)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_owned_preparation_enrolls_atomically_and_blocks_every_logical_writer() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = f.engine();
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let (actor, workspace, prepared) = prepared(&f, &engine).await;
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    let digest = actor.prepared_approval_digest(&engine, &prepared).unwrap();
    assert!(!key_table(&f));
    let op = actor
        .enroll_prepared(&engine, &prepared, "request", &digest, &busy)
        .unwrap();
    assert_eq!(op.version, 3);
    assert_eq!(
        op.phase,
        crate::db::pair_refresh_operations::PairRefreshPhase::Prepared
    );
    assert!(key_table(&f));
    let rows = journal_state(&f);
    assert_eq!(
        rows.iter()
            .filter(|(key, _, _)| key.starts_with("pair_refresh_execution_v3:"))
            .count(),
        5
    );
    assert!(!rows
        .iter()
        .any(|(_, value, _)| value.contains("disposable-workspace-session")));
    assert_eq!(f.db.pair_refresh_operation(op.id).unwrap(), op);
    assert_eq!(
        actor
            .enroll_prepared(&engine, &prepared, "request", &digest, &busy)
            .unwrap(),
        op
    );
    assert_eq!(journal_state(&f), rows);
    for scope in [
        None,
        Some("child"),
        Some("parent"),
        Some("unregistered-alias"),
    ] {
        assert!(
            crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&f.db.conn(), scope)
                .is_err()
        );
        assert!(f.db.begin_ordinary_writer_window(scope).is_err());
    }
    assert!(
        crate::db::pair_refresh_operations::require_no_pair_refresh_registration_mutation(
            &f.db.conn(),
            "unregistered-alias"
        )
        .is_err()
    );
    assert!(f
        .db
        .set_state("secret_git_token_unregistered-alias", "replacement")
        .is_err());
    assert_eq!(journal_state(&f), rows);
    drop(busy);
    drop(workspace);
    drop(prepared);
    assert_eq!(f.db.pair_refresh_operation(op.id).unwrap(), op);
    assert!(op.pins.svn.working_copy.join(".svn").is_dir());
    assert_eq!(
        f.target
            .find_reference("refs/heads/feature")
            .unwrap()
            .target(),
        Some(f.baseline)
    );
    assert_eq!(
        f.target.find_reference("refs/heads/main").unwrap().target(),
        Some(f.parent)
    );
    assert_eq!(
        crate::svn::SvnClient::new(format!("{}/trunk", f.root), "", "")
            .info()
            .await
            .unwrap()
            .latest_rev,
        4
    );
    assert!(!std::fs::read_to_string(&f.log)
        .unwrap()
        .contains("receive-pack"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn known_refusals_leave_key_job_request_and_holds_absent() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = f.engine();
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let (actor, _workspace, prepared) = prepared(&f, &engine).await;
    let digest = actor.prepared_approval_digest(&engine, &prepared).unwrap();
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    let original = journal_state(&f);
    for (request, approved) in [("", digest.as_str()), ("request", "invalid")] {
        assert!(actor
            .enroll_prepared(&engine, &prepared, request, approved, &busy)
            .is_err());
        assert_eq!(journal_state(&f), original);
        assert!(!key_table(&f));
    }
    let wrong_busy = crate::busy::try_acquire_pair("foreign-one", "foreign-two").unwrap();
    assert!(actor
        .enroll_prepared(&engine, &prepared, "request", &digest, &wrong_busy)
        .is_err());
    assert_eq!(journal_state(&f), original);
    assert!(!key_table(&f));
    f.db.delete_session("disposable-workspace-session").unwrap();
    assert!(actor
        .enroll_prepared(&engine, &prepared, "request", &digest, &busy)
        .is_err());
    assert_eq!(journal_state(&f), original);
    assert!(!key_table(&f));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unrelated_ordinary_window_refuses_authenticated_enrollment() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = f.engine();
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let (actor, _workspace, prepared) = prepared(&f, &engine).await;
    let digest = actor.prepared_approval_digest(&engine, &prepared).unwrap();
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    let window =
        f.db.begin_ordinary_writer_window(Some("unregistered-alias"))
            .unwrap();
    let original = journal_state(&f);
    assert!(actor
        .enroll_prepared(&engine, &prepared, "request", &digest, &busy)
        .is_err());
    assert_eq!(journal_state(&f), original);
    assert!(!key_table(&f));
    window.finish_preparation().unwrap();
    actor
        .enroll_prepared(&engine, &prepared, "request", &digest, &busy)
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unfinished_unregistered_journal_and_unknown_family_version_refuse_without_key_repair() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = f.engine();
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let (actor, _workspace, prepared) = prepared(&f, &engine).await;
    let digest = actor.prepared_approval_digest(&engine, &prepared).unwrap();
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    for key in [
        "import_operation_v1:active:deleted-id",
        "import_operation_v1:latest:deleted-id",
        "import_operation_v99:document:deleted-id",
        "managed_remove_v1:tombstone:deleted-id",
    ] {
        f.db.set_state(key, "missing-or-corrupt").unwrap();
        let original = journal_state(&f);
        assert!(
            actor
                .enroll_prepared(&engine, &prepared, "request", &digest, &busy)
                .is_err(),
            "{key}"
        );
        assert_eq!(journal_state(&f), original);
        assert!(!key_table(&f));
        f.db.conn()
            .execute("DELETE FROM kv_state WHERE key=?1", [key])
            .unwrap();
    }
    actor
        .enroll_prepared(&engine, &prepared, "request", &digest, &busy)
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn altered_missing_and_orphan_authority_cannot_downgrade_the_global_hold() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = f.engine();
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let (actor, _workspace, prepared) = prepared(&f, &engine).await;
    let digest = actor.prepared_approval_digest(&engine, &prepared).unwrap();
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    let op = actor
        .enroll_prepared(&engine, &prepared, "request", &digest, &busy)
        .unwrap();
    let saved = journal_state(&f);
    let authority_key = format!("pair_refresh_execution_v3:authority:{}", op.id);
    let document_key = format!("pair_refresh_execution_v3:document:{}", op.id);
    let authority = f.db.get_state(&authority_key).unwrap().unwrap();
    let document = f.db.get_state(&document_key).unwrap().unwrap();
    for (key, value) in [
        (&authority_key, None),
        (&authority_key, Some("{}")),
        (&document_key, Some("{}")),
    ] {
        match value {
            Some(value) => {
                f.db.conn()
                    .execute(
                        "UPDATE kv_state SET value=?2 WHERE key=?1",
                        rusqlite::params![key, value],
                    )
                    .unwrap();
            }
            None => {
                f.db.conn()
                    .execute("DELETE FROM kv_state WHERE key=?1", [key])
                    .unwrap();
            }
        }
        let broken = journal_state(&f);
        assert!(f.db.pair_refresh_operation(op.id).is_err());
        assert!(f.db.begin_ordinary_writer_window(Some("alias")).is_err());
        assert_eq!(journal_state(&f), broken);
        f.db.conn().execute("INSERT INTO kv_state(key,value,updated_at) VALUES(?1,?2,'restore') ON CONFLICT(key) DO UPDATE SET value=excluded.value", rusqlite::params![authority_key,authority]).unwrap();
        f.db.conn()
            .execute(
                "UPDATE kv_state SET value=?2 WHERE key=?1",
                rusqlite::params![document_key, document],
            )
            .unwrap();
    }
    let mut tampered: serde_json::Value = serde_json::from_str(&document).unwrap();
    tampered["admitted_writer_epoch"] = serde_json::json!(op.admitted_writer_epoch + 1);
    let mut typed: crate::db::pair_refresh_operations::PairRefreshOperation =
        serde_json::from_value(tampered).unwrap();
    // Canonical valid structure still cannot replace signed phase/owner fields.
    typed.updated_at = chrono::Utc::now().to_rfc3339();
    f.db.conn()
        .execute(
            "UPDATE kv_state SET value=?2 WHERE key=?1",
            rusqlite::params![document_key, serde_json::to_string(&typed).unwrap()],
        )
        .unwrap();
    assert!(f.db.pair_refresh_operation(op.id).is_err());
    f.db.conn()
        .execute(
            "UPDATE kv_state SET value=?2 WHERE key=?1",
            rusqlite::params![document_key, document],
        )
        .unwrap();
    assert_eq!(f.db.pair_refresh_operation(op.id).unwrap(), op);
    assert!(f.db.record_pair_refresh_svn_intent(op.id).is_err());
    assert!(f
        .db
        .record_pair_refresh_svn_intent_with_registered_sources(op.id, &busy)
        .is_err());
    assert!(f.db.cancel_unpublished_pair_refresh(op.id).is_err());
    assert!(f
        .db
        .hold_pair_refresh_reconciliation(op.id, "legacy bypass")
        .is_err());
    assert_eq!(f.db.pair_refresh_operation(op.id).unwrap(), op);
    assert_eq!(
        saved
            .iter()
            .filter(|(key, _, _)| key.starts_with("pair_refresh_execution_v3:"))
            .count(),
        5
    );
    f.db.conn()
        .execute(
            "DELETE FROM kv_state WHERE key LIKE 'pair_refresh_execution_v3:%'",
            [],
        )
        .unwrap();
    assert!(f.db.begin_ordinary_writer_window(Some("alias")).is_err());
    assert!(key_table(&f));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn independent_journal_key_survives_session_and_fingerprint_rotation_but_never_repairs_itself(
) {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = f.engine();
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let (actor, _workspace, prepared) = prepared(&f, &engine).await;
    let digest = actor.prepared_approval_digest(&engine, &prepared).unwrap();
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    let op = actor
        .enroll_prepared(&engine, &prepared, "request", &digest, &busy)
        .unwrap();
    f.db.delete_session("disposable-workspace-session").unwrap();
    f.db.conn()
        .execute(
            "UPDATE kv_state SET value=?1 WHERE key='encryption_key'",
            ["42".repeat(32)],
        )
        .unwrap();
    let saved = journal_state(&f);
    assert_eq!(f.db.pair_refresh_operation(op.id).unwrap(), op);
    assert!(actor.require_current(&engine).is_err());
    assert_eq!(journal_state(&f), saved);
    f.db.conn()
        .execute("DELETE FROM pair_refresh_journal_key_v3", [])
        .unwrap();
    let broken = journal_state(&f);
    assert!(f.db.pair_refresh_operation(op.id).is_err());
    assert!(f.db.begin_ordinary_writer_window(Some("alias")).is_err());
    let count: i64 =
        f.db.conn()
            .query_row(
                "SELECT count(*) FROM pair_refresh_journal_key_v3",
                [],
                |row| row.get(0),
            )
            .unwrap();
    assert_eq!(count, 0);
    assert_eq!(journal_state(&f), broken);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_authority_insert_rolls_back_key_and_every_enrollment_row() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = f.engine();
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let (actor, workspace, prepared) = prepared(&f, &engine).await;
    let digest = actor.prepared_approval_digest(&engine, &prepared).unwrap();
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    let original = journal_state(&f);
    f.db.conn().execute_batch("CREATE TEMP TRIGGER enrollment_authority_abort BEFORE INSERT ON kv_state WHEN new.key LIKE 'pair_refresh_execution_v3:authority:%' BEGIN SELECT RAISE(ABORT,'disposable enrollment interruption'); END;").unwrap();
    // The engine owns a distinct connection, so install the fault there too.
    engine.db().conn().execute_batch("CREATE TEMP TRIGGER enrollment_authority_abort BEFORE INSERT ON kv_state WHEN new.key LIKE 'pair_refresh_execution_v3:authority:%' BEGIN SELECT RAISE(ABORT,'disposable enrollment interruption'); END;").unwrap();
    let error = actor
        .enroll_prepared(&engine, &prepared, "request", &digest, &busy)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("disposable enrollment interruption"),
        "{error}"
    );
    assert!(!key_table(&f));
    assert_eq!(journal_state(&f), original);
    assert!(
        crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&f.db.conn(), None)
            .is_ok()
    );
    let wc = prepared.pins().svn.working_copy.clone();
    drop(workspace);
    drop(prepared);
    assert!(wc.join(".svn").is_dir());
    assert!(!std::fs::read_to_string(&f.log)
        .unwrap()
        .contains("receive-pack"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exact_copied_authenticated_storage_cannot_inherit_the_original_hold_authority() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = f.engine();
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let (actor, _workspace, prepared) = prepared(&f, &engine).await;
    let digest = actor.prepared_approval_digest(&engine, &prepared).unwrap();
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    let op = actor
        .enroll_prepared(&engine, &prepared, "request", &digest, &busy)
        .unwrap();
    let other = tempfile::tempdir().unwrap();
    let copied = other.path().join("copied.db");
    f.db.conn()
        .execute("VACUUM INTO ?1", [copied.to_str().unwrap()])
        .unwrap();
    let foreign = Database::new(copied).unwrap();
    let rows: Vec<(String, String, String)> = foreign
        .conn()
        .prepare("SELECT key,value,updated_at FROM kv_state ORDER BY key")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(rows, f.state());
    let original_key: Vec<u8> =
        f.db.conn()
            .query_row(
                "SELECT secret FROM pair_refresh_journal_key_v3",
                [],
                |row| row.get(0),
            )
            .unwrap();
    let copied_key: Vec<u8> = foreign
        .conn()
        .query_row(
            "SELECT secret FROM pair_refresh_journal_key_v3",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(copied_key, original_key);
    assert!(foreign.pair_refresh_operation(op.id).is_err());
    assert!(foreign.begin_ordinary_writer_window(Some("alias")).is_err());
    assert_eq!(f.db.pair_refresh_operation(op.id).unwrap(), op);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn authenticated_pre_intent_cancellation_returns_a_signed_durable_idempotent_receipt() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = f.engine();
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let (actor, _workspace, prepared) = prepared(&f, &engine).await;
    let digest = actor.prepared_approval_digest(&engine, &prepared).unwrap();
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    let op = actor
        .enroll_prepared(&engine, &prepared, "request", &digest, &busy)
        .unwrap();
    let cancelled = actor
        .cancel_prepared_enrollment(&engine, &prepared, &busy)
        .unwrap();
    assert_eq!(cancelled.id, op.id);
    assert_eq!(
        cancelled.phase,
        crate::db::pair_refresh_operations::PairRefreshPhase::CancelledBeforePublication
    );
    assert_eq!(cancelled.pins, op.pins);
    assert_eq!(f.db.pair_refresh_operation(op.id).unwrap(), cancelled);
    assert_eq!(
        actor
            .enroll_prepared(&engine, &prepared, "request", &digest, &busy)
            .unwrap(),
        cancelled
    );
    let saved = journal_state(&f);
    assert!(actor
        .cancel_prepared_enrollment(&engine, &prepared, &busy)
        .is_err());
    assert!(actor
        .enroll_prepared(&engine, &prepared, "different-request", &digest, &busy)
        .is_err());
    assert_eq!(journal_state(&f), saved);
    assert!(
        crate::db::pair_refresh_operations::require_no_pair_refresh_hold(
            &f.db.conn(),
            Some("alias")
        )
        .is_ok()
    );
    assert_eq!(
        saved
            .iter()
            .filter(|(key, _, _)| key.starts_with("pair_refresh_execution_v3:"))
            .count(),
        3
    );
    let window = f.db.begin_ordinary_writer_window(Some("alias")).unwrap();
    window.finish_preparation().unwrap();
    assert_eq!(
        f.target.find_reference("refs/heads/main").unwrap().target(),
        Some(f.parent)
    );
    assert_eq!(
        f.target
            .find_reference("refs/heads/feature")
            .unwrap()
            .target(),
        Some(f.baseline)
    );
    assert_eq!(
        crate::svn::SvnClient::new(format!("{}/trunk", f.root), "", "")
            .info()
            .await
            .unwrap()
            .latest_rev,
        4
    );
    assert!(!std::fs::read_to_string(&f.log)
        .unwrap()
        .contains("receive-pack"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owned_svn_stage_commits_once_and_keeps_global_exclusion_for_git_completion() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = f.engine();
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let (actor, _workspace, prepared) = prepared(&f, &engine).await;
    let approved = actor.prepared_approval_digest(&engine, &prepared).unwrap();
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    let before = f.state();
    let observed = super::super::execution::publish_owned_svn_stage_with_client(
        &engine,
        &actor,
        &prepared,
        "svn-stage",
        &approved,
        &busy,
        f.client.clone(),
    )
    .await
    .unwrap();
    assert_eq!(
        observed.phase,
        crate::db::pair_refresh_operations::PairRefreshPhase::SvnObserved
    );
    let receipt = observed.svn_receipt.as_ref().unwrap();
    assert_eq!(receipt.revision, 5);
    assert_eq!(receipt.git_tree, prepared.pins().git.tree_sha);
    assert_eq!(receipt.mergeinfo, "/trunk:3-4");
    assert!(observed.git_receipt_sha.is_none());
    assert_eq!(f.db.pair_refresh_operation(observed.id).unwrap(), observed);
    assert!(f
        .db
        .begin_ordinary_writer_window(Some("unregistered-alias"))
        .is_err());
    assert_eq!(
        f.target
            .find_reference("refs/heads/feature")
            .unwrap()
            .target(),
        Some(f.baseline)
    );
    assert_eq!(
        f.target.find_reference("refs/heads/main").unwrap().target(),
        Some(f.parent)
    );
    assert_eq!(
        f.state()
            .into_iter()
            .filter(|(key, _, _)| !key.starts_with("pair_refresh_execution_")
                && key != "writer_fence_v1:lease")
            .collect::<Vec<_>>(),
        before
            .into_iter()
            .filter(|(key, _, _)| key != "writer_fence_v1:lease")
            .collect::<Vec<_>>()
    );
    let repeated = super::super::execution::publish_owned_svn_stage_with_client(
        &engine,
        &actor,
        &prepared,
        "svn-stage",
        &approved,
        &busy,
        f.client.clone(),
    )
    .await
    .unwrap();
    assert_eq!(repeated, observed);
    assert_eq!(
        crate::svn::SvnClient::new(format!("{}/branches/f", f.root), "", "")
            .info()
            .await
            .unwrap()
            .latest_rev,
        5
    );
    assert!(!std::fs::read_to_string(&f.log)
        .unwrap()
        .contains("receive-pack"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unhandled_svn_drift_before_first_intent_keeps_a_cancellable_prepared_job() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = f.engine();
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let (actor, _workspace, prepared) = prepared(&f, &engine).await;
    let approved = actor.prepared_approval_digest(&engine, &prepared).unwrap();
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    std::fs::write(
        f._parent_wc.join("file.txt"),
        b"external unhandled advance\n",
    )
    .unwrap();
    svn(
        &["commit", "-m", "unhandled external fixture advance"],
        Some(&f._parent_wc),
    );
    assert!(
        super::super::execution::publish_owned_svn_stage_with_client(
            &engine,
            &actor,
            &prepared,
            "drift-stage",
            &approved,
            &busy,
            f.client.clone(),
        )
        .await
        .is_err()
    );
    let op =
        f.db.pair_refresh_operation(prepared.pins().git.operation_id)
            .unwrap();
    assert_eq!(
        op.phase,
        crate::db::pair_refresh_operations::PairRefreshPhase::Prepared
    );
    assert!(op.svn_receipt.is_none());
    assert!(op.git_receipt_sha.is_none());
    assert!(f.db.begin_ordinary_writer_window(Some("alias")).is_err());
    let cancelled = actor
        .cancel_prepared_enrollment(&engine, &prepared, &busy)
        .unwrap();
    assert_eq!(
        cancelled.phase,
        crate::db::pair_refresh_operations::PairRefreshPhase::CancelledBeforePublication
    );
    assert_eq!(
        crate::svn::SvnClient::new(format!("{}/branches/f", f.root), "", "")
            .info()
            .await
            .unwrap()
            .latest_rev,
        5
    );
    assert_eq!(
        f.target
            .find_reference("refs/heads/feature")
            .unwrap()
            .target(),
        Some(f.baseline)
    );
    assert_eq!(
        f.target.find_reference("refs/heads/main").unwrap().target(),
        Some(f.parent)
    );
    assert!(!std::fs::read_to_string(&f.log)
        .unwrap()
        .contains("receive-pack"));
}

fn parent_row(f: &Fixture) -> Vec<rusqlite::types::Value> {
    f.db.conn()
        .query_row("SELECT * FROM repositories WHERE id='parent'", [], |row| {
            (0..row.as_ref().column_count())
                .map(|index| row.get(index))
                .collect()
        })
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn complete_owned_refresh_preserves_histories_and_atomically_advances_only_child() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = f.engine();
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let (actor, _workspace, prepared) = prepared(&f, &engine).await;
    let approved = actor.prepared_approval_digest(&engine, &prepared).unwrap();
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    let parent_before = parent_row(&f);
    let echoes_before: Vec<_> = f
        .state()
        .into_iter()
        .filter(|(key, _, _)| key.starts_with("repo_echo_generation_"))
        .collect();
    let completed = super::super::execution::complete_owned_refresh_with_client(
        &engine,
        &actor,
        &prepared,
        "complete-first",
        &approved,
        &busy,
        f.client.clone(),
    )
    .await
    .unwrap();
    assert_eq!(
        completed.phase,
        crate::db::pair_refresh_operations::PairRefreshPhase::Completed
    );
    assert_eq!(completed.svn_receipt.as_ref().unwrap().revision, 5);
    assert_eq!(
        completed.git_receipt_sha.as_deref(),
        Some(prepared.pins().git.prepared_sha.as_str())
    );
    assert_eq!(parent_row(&f), parent_before);
    assert_eq!(
        f.state()
            .into_iter()
            .filter(|(key, _, _)| key.starts_with("repo_echo_generation_"))
            .collect::<Vec<_>>(),
        echoes_before
    );
    assert_eq!(
        f.target.find_reference("refs/heads/main").unwrap().target(),
        Some(f.parent)
    );
    let integrated = f
        .target
        .find_commit(Oid::from_str(&prepared.pins().git.prepared_sha).unwrap())
        .unwrap();
    assert_eq!(
        f.target
            .find_reference("refs/heads/feature")
            .unwrap()
            .target(),
        Some(integrated.id())
    );
    assert_eq!(
        integrated.parent_ids().collect::<Vec<_>>(),
        vec![f.baseline, f.parent]
    );
    assert_eq!(
        integrated.tree_id().to_string(),
        prepared.pins().svn.result_git_tree
    );
    assert!(f
        .target
        .graph_descendant_of(integrated.id(), f.baseline)
        .unwrap());
    assert!(f
        .target
        .graph_descendant_of(integrated.id(), f.parent)
        .unwrap());
    let (sha, rev): (String, i64) =
        f.db.conn()
            .query_row(
                "SELECT last_git_sha,last_svn_rev FROM repositories WHERE id='child'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
    assert_eq!(sha, integrated.id().to_string());
    assert_eq!(rev, 5);
    assert_eq!(
        f.db.get_state(&format!("last_git_sha_{}", completed.pins.child.scope_uuid))
            .unwrap()
            .as_deref(),
        Some(sha.as_str())
    );
    assert_eq!(
        f.db.get_state("last_svn_rev_child").unwrap().as_deref(),
        Some("5")
    );
    let correspondence: i64 = f.db.conn().query_row("SELECT count(*) FROM commit_map WHERE repo_id='child' AND svn_rev=5 AND git_sha=?1 AND direction='git_to_svn'", [&sha], |row| row.get(0)).unwrap();
    let sync_records: i64 = f.db.conn().query_row("SELECT count(*) FROM sync_records WHERE id=?1 AND repo_id='child' AND svn_rev=5 AND git_sha=?2", rusqlite::params![completed.id.to_string(),sha], |row| row.get(0)).unwrap();
    assert_eq!(correspondence, 1);
    assert_eq!(sync_records, 1);
    assert_eq!(
        f.db.pair_refresh_operation(completed.id).unwrap(),
        completed
    );
    assert!(
        crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&f.db.conn(), None)
            .is_ok()
    );
    let state_before_repeat = journal_state(&f);
    let repeated = super::super::execution::complete_owned_refresh_with_client(
        &engine,
        &actor,
        &prepared,
        "complete-first",
        &approved,
        &busy,
        f.client.clone(),
    )
    .await
    .unwrap();
    assert_eq!(repeated, completed);
    assert_eq!(journal_state(&f), state_before_repeat);
    assert_eq!(
        crate::svn::SvnClient::new(format!("{}/branches/f", f.root), "", "")
            .info()
            .await
            .unwrap()
            .latest_rev,
        5
    );
    assert_eq!(
        f.target
            .find_reference("refs/heads/feature")
            .unwrap()
            .target(),
        Some(integrated.id())
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn next_owned_refresh_uses_its_signed_completed_baseline_and_preserves_copy_origin() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = f.engine();
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let (actor, _workspace, first) = prepared(&f, &engine).await;
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    let first_approval = actor.prepared_approval_digest(&engine, &first).unwrap();
    let completed_first = super::super::execution::complete_owned_refresh_with_client(
        &engine,
        &actor,
        &first,
        "first-before-next",
        &first_approval,
        &busy,
        f.client.clone(),
    )
    .await
    .unwrap();
    let first_sha = Oid::from_str(completed_first.git_receipt_sha.as_ref().unwrap()).unwrap();
    // Model an independently handled parent advance in this preparation fixture.
    // Full ordinary SyncEngine initialization/cycle qualification is separate.
    std::fs::write(f._parent_wc.join("file.txt"), b"second parent advance\n").unwrap();
    svn(
        &["commit", "-m", "second fixture parent advance"],
        Some(&f._parent_wc),
    );
    let advanced = commit(&f.target, b"second parent advance\n", &[f.parent]);
    assert!(f.target.graph_descendant_of(advanced, f.parent).unwrap());
    f.target
        .find_reference("refs/heads/main")
        .unwrap()
        .set_target(advanced, "fixture parent fast-forward")
        .unwrap();
    f.db.insert_commit_map_with_repo(
        6,
        &advanced.to_string(),
        "svn_to_git",
        "fixture",
        "fixture",
        Some("parent"),
    )
    .unwrap();
    f.db.update_repo_watermark("parent", 6, &advanced.to_string())
        .unwrap();
    crate::echo_receipt_scope::write_scoped_last_git_sha_kv(
        &f.db.conn(),
        "parent",
        &advanced.to_string(),
        &chrono::Utc::now().to_rfc3339(),
    )
    .unwrap();
    let parent_before = parent_row(&f);
    let second_workspace = actor.create_workspace(&engine).unwrap();
    let second = prepare_registered_in_workspace_for_test(
        &f.db,
        "child",
        second_workspace.clone(),
        f.client.clone(),
    )
    .await
    .unwrap();
    assert_eq!(second.pins().git.pins.child_tip, first_sha.to_string());
    assert_eq!(
        second.pins().git.pins.parent_baseline_tip,
        f.parent.to_string()
    );
    assert_eq!(
        second
            .pins()
            .svn
            .pins
            .lineage
            .pins
            .child
            .parent_baseline_revision,
        4
    );
    assert_eq!(
        second.pins().svn.pins.lineage.child_copy.copy_from_revision,
        2
    );
    assert_eq!(
        second.pins().svn.pins.lineage.child_copy.created_revision,
        3
    );
    let approved = actor.prepared_approval_digest(&engine, &second).unwrap();
    let completed_second = super::super::execution::complete_owned_refresh_with_client(
        &engine,
        &actor,
        &second,
        "next-refresh",
        &approved,
        &busy,
        f.client.clone(),
    )
    .await
    .unwrap();
    assert_eq!(
        completed_second.phase,
        crate::db::pair_refresh_operations::PairRefreshPhase::Completed
    );
    let receipt = completed_second.svn_receipt.as_ref().unwrap();
    assert_eq!(receipt.revision, 7);
    assert_eq!(receipt.mergeinfo, "/trunk:3-6");
    assert_eq!(receipt.child_copy.copy_from_revision, 2);
    assert_eq!(receipt.child_copy.created_revision, 3);
    assert_eq!(parent_row(&f), parent_before);
    let tip = f
        .target
        .find_reference("refs/heads/feature")
        .unwrap()
        .target()
        .unwrap();
    let integrated = f.target.find_commit(tip).unwrap();
    assert_eq!(
        integrated.parent_ids().collect::<Vec<_>>(),
        vec![first_sha, advanced]
    );
    assert_eq!(integrated.tree_id().to_string(), receipt.git_tree);
    assert_eq!(
        f.target.find_reference("refs/heads/main").unwrap().target(),
        Some(advanced)
    );
    assert!(f.target.graph_descendant_of(tip, f.baseline).unwrap());
    assert!(f.target.graph_descendant_of(tip, f.parent).unwrap());
    assert_eq!(
        f.db.pair_refresh_operation(completed_first.id).unwrap(),
        completed_first
    );
    assert_eq!(
        f.db.pair_refresh_operation(completed_second.id).unwrap(),
        completed_second
    );
    assert!(
        crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&f.db.conn(), None)
            .is_ok()
    );
}
