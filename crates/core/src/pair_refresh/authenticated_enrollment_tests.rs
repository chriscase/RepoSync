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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn original_owner_observes_lost_svn_and_git_receipts_without_republication() {
    use crate::db::pair_refresh_operations::PairRefreshPhase;

    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = f.engine();
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let (actor, _workspace, prepared) = prepared(&f, &engine).await;
    let approved = actor.prepared_approval_digest(&engine, &prepared).unwrap();
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    let parent_before = parent_row(&f);
    f.db.conn().execute_batch("CREATE TRIGGER interrupt_svn_receipt BEFORE INSERT ON kv_state WHEN NEW.key LIKE 'pair_refresh_execution_v3:document:%' AND json_extract(NEW.value,'$.phase')='svn_observed' BEGIN SELECT RAISE(ABORT,'disposable SVN receipt interruption'); END;").unwrap();
    let error = super::super::execution::publish_owned_svn_stage_with_client(
        &engine,
        &actor,
        &prepared,
        "lost-receipts",
        &approved,
        &busy,
        f.client.clone(),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("disposable SVN receipt interruption"),
        "{error}"
    );
    let intent =
        f.db.pair_refresh_operation(prepared.pins().git.operation_id)
            .unwrap();
    assert_eq!(intent.phase, PairRefreshPhase::SvnIntent);
    assert!(intent.svn_receipt.is_none());
    let svn_client = crate::svn::SvnClient::new(format!("{}/branches/f", f.root), "", "");
    assert_eq!(svn_client.info().await.unwrap().latest_rev, 5);
    assert_eq!(
        f.target
            .find_reference("refs/heads/feature")
            .unwrap()
            .target(),
        Some(f.baseline)
    );
    assert!(
        crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&f.db.conn(), None)
            .is_err()
    );
    assert!(
        super::super::execution::publish_owned_svn_stage_with_client(
            &engine,
            &actor,
            &prepared,
            "lost-receipts",
            &approved,
            &busy,
            f.client.clone(),
        )
        .await
        .is_err()
    );
    assert_eq!(svn_client.info().await.unwrap().latest_rev, 5);
    assert_eq!(f.db.pair_refresh_operation(intent.id).unwrap(), intent);
    f.db.conn()
        .execute_batch("DROP TRIGGER interrupt_svn_receipt;")
        .unwrap();
    let observed = super::super::execution::reconcile_owned_refresh_intent_with_client(
        &engine,
        &actor,
        &prepared,
        &busy,
        f.client.clone(),
    )
    .await
    .unwrap();
    assert_eq!(observed.phase, PairRefreshPhase::SvnObserved);
    assert_eq!(observed.svn_receipt.as_ref().unwrap().revision, 5);
    assert_eq!(svn_client.info().await.unwrap().latest_rev, 5);
    f.db.conn().execute_batch("CREATE TRIGGER interrupt_git_receipt BEFORE INSERT ON kv_state WHEN NEW.key LIKE 'pair_refresh_execution_v3:document:%' AND json_extract(NEW.value,'$.phase')='git_observed' BEGIN SELECT RAISE(ABORT,'disposable Git receipt interruption'); END;").unwrap();
    let error = super::super::execution::complete_owned_refresh_with_client(
        &engine,
        &actor,
        &prepared,
        "lost-receipts",
        &approved,
        &busy,
        f.client.clone(),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("disposable Git receipt interruption"),
        "{error}"
    );
    let intent = f.db.pair_refresh_operation(intent.id).unwrap();
    assert_eq!(intent.phase, PairRefreshPhase::GitIntent);
    assert!(intent.git_receipt_sha.is_none());
    assert_eq!(
        f.target
            .find_reference("refs/heads/feature")
            .unwrap()
            .target()
            .unwrap()
            .to_string(),
        prepared.pins().git.prepared_sha
    );
    let writes_before_reconciliation = std::fs::read_to_string(&f.log)
        .unwrap()
        .lines()
        .filter(|line| line.contains("/git-receive-pack"))
        .count();
    assert!(writes_before_reconciliation > 0);
    assert!(super::super::execution::complete_owned_refresh_with_client(
        &engine,
        &actor,
        &prepared,
        "lost-receipts",
        &approved,
        &busy,
        f.client.clone(),
    )
    .await
    .is_err());
    assert_eq!(f.db.pair_refresh_operation(intent.id).unwrap(), intent);
    f.db.conn()
        .execute_batch("DROP TRIGGER interrupt_git_receipt;")
        .unwrap();
    let observed = super::super::execution::reconcile_owned_refresh_intent_with_client(
        &engine,
        &actor,
        &prepared,
        &busy,
        f.client.clone(),
    )
    .await
    .unwrap();
    assert_eq!(observed.phase, PairRefreshPhase::GitObserved);
    assert_eq!(
        observed.git_receipt_sha.as_deref(),
        Some(prepared.pins().git.prepared_sha.as_str())
    );
    assert!(
        crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&f.db.conn(), None)
            .is_err()
    );
    let completed = super::super::execution::complete_owned_refresh_with_client(
        &engine,
        &actor,
        &prepared,
        "lost-receipts",
        &approved,
        &busy,
        f.client.clone(),
    )
    .await
    .unwrap();
    assert_eq!(completed.phase, PairRefreshPhase::Completed);
    assert_eq!(
        std::fs::read_to_string(&f.log)
            .unwrap()
            .lines()
            .filter(|line| line.contains("/git-receive-pack"))
            .count(),
        writes_before_reconciliation
    );
    assert_eq!(svn_client.info().await.unwrap().latest_rev, 5);
    assert_eq!(parent_row(&f), parent_before);
    assert!(
        crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&f.db.conn(), None)
            .is_ok()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn denied_svn_intent_and_absence_observation_never_authorize_a_retry() {
    use crate::db::pair_refresh_operations::PairRefreshPhase;
    use std::os::unix::fs::PermissionsExt;

    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = f.engine();
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let (actor, _workspace, prepared) = prepared(&f, &engine).await;
    let approved = actor.prepared_approval_digest(&engine, &prepared).unwrap();
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    let parent_before = parent_row(&f);
    let hook = f.temp.path().join("svn-repository/hooks/pre-commit");
    std::fs::write(
        &hook,
        b"#!/bin/sh\nprintf '%s\\n' 'disposable SVN permission refusal' >&2\nexit 1\n",
    )
    .unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        super::super::execution::publish_owned_svn_stage_with_client(
            &engine,
            &actor,
            &prepared,
            "denied-svn",
            &approved,
            &busy,
            f.client.clone(),
        )
        .await
        .is_err()
    );
    let intent =
        f.db.pair_refresh_operation(prepared.pins().git.operation_id)
            .unwrap();
    assert_eq!(intent.phase, PairRefreshPhase::SvnIntent);
    assert!(intent.reconciliation_detail.is_some());
    assert!(intent.svn_receipt.is_none());
    let before = journal_state(&f);
    std::fs::remove_file(hook).unwrap();
    assert!(
        super::super::execution::reconcile_owned_refresh_intent_with_client(
            &engine,
            &actor,
            &prepared,
            &busy,
            f.client.clone(),
        )
        .await
        .is_err()
    );
    assert_eq!(journal_state(&f), before);
    assert!(
        super::super::execution::publish_owned_svn_stage_with_client(
            &engine,
            &actor,
            &prepared,
            "denied-svn",
            &approved,
            &busy,
            f.client.clone(),
        )
        .await
        .is_err()
    );
    assert!(actor
        .cancel_prepared_enrollment(&engine, &prepared, &busy)
        .is_err());
    assert_eq!(journal_state(&f), before);
    assert_eq!(
        crate::svn::SvnClient::new(format!("{}/branches/f", f.root), "", "")
            .info()
            .await
            .unwrap()
            .latest_rev,
        4
    );
    assert_eq!(
        f.target
            .find_reference("refs/heads/feature")
            .unwrap()
            .target(),
        Some(f.baseline)
    );
    assert_eq!(parent_row(&f), parent_before);
    assert!(!std::fs::read_to_string(&f.log)
        .unwrap()
        .contains("receive-pack"));
    assert!(
        crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&f.db.conn(), None)
            .is_err()
    );
}

struct SvnDiag {
    _path: std::path::PathBuf,
}

impl SvnDiag {
    fn arm(temp: &std::path::Path) -> Self {
        let path = temp.join("svn-diag.log");
        std::env::set_var("REPOSYNC_SVN_DIAG_LOG", &path);
        Self { _path: path }
    }
}

impl Drop for SvnDiag {
    fn drop(&mut self) {
        std::env::remove_var("REPOSYNC_SVN_DIAG_LOG");
    }
}

fn exec_witness(label: &str, path: &std::path::Path) -> String {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path)
        .map(|meta| format!("{:o}", meta.permissions().mode()))
        .unwrap_or_else(|error| format!("metadata {error}"));
    let test_x = std::process::Command::new("test")
        .arg("-x")
        .arg(path)
        .status()
        .map(|status| {
            status
                .code()
                .map(|code| code.to_string())
                .unwrap_or_else(|| "signal".into())
        })
        .unwrap_or_else(|error| format!("spawn {error}"));
    format!(
        "{label} path={} mode={mode} test_x={test_x}",
        path.display()
    )
}

fn file_text(path: &std::path::Path) -> String {
    match std::fs::read(path) {
        Ok(bytes) => format!(
            "{} bytes={}",
            path.display(),
            String::from_utf8_lossy(&bytes)
        ),
        Err(error) => format!("{} absent ({error})", path.display()),
    }
}

fn case_diag(temp: &std::path::Path, hook: &std::path::Path, ready: &std::path::Path) -> String {
    format!(
        "{} ready={} diag={} backend={}",
        exec_witness("hook", hook),
        file_text(ready),
        file_text(&temp.join("svn-diag.log")),
        file_text(&temp.join("http-backend-stderr.txt"))
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn protected_git_ref_retains_the_svn_result_without_retrying_a_push() {
    use crate::db::pair_refresh_operations::PairRefreshPhase;
    use std::os::unix::fs::PermissionsExt;

    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = f.engine();
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let (actor, _workspace, prepared) = prepared(&f, &engine).await;
    let approved = actor.prepared_approval_digest(&engine, &prepared).unwrap();
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    let parent_before = parent_row(&f);
    let hook = f.target.path().join("hooks/pre-receive");
    std::fs::write(
        &hook,
        b"#!/bin/sh\nprintf '%s\\n' 'disposable protected ref refusal' >&2\nexit 1\n",
    )
    .unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o700)).unwrap();
    let before = f
        .target
        .find_reference("refs/heads/feature")
        .unwrap()
        .target();
    let result = super::super::execution::complete_owned_refresh_with_client(
        &engine,
        &actor,
        &prepared,
        "protected-child",
        &approved,
        &busy,
        f.client.clone(),
    )
    .await;
    let after = f
        .target
        .find_reference("refs/heads/feature")
        .ok()
        .and_then(|reference| reference.target());
    let summary = match &result {
        Ok(_) => "Ok".to_string(),
        Err(error) => crate::errors::redact_vcs_error_detail(&format!("{error:#}")),
    };
    assert!(
        result.is_err(),
        "result={summary} before={before:?} after={after:?} {}",
        case_diag(f.temp.path(), &hook, &f.temp.path().join("git-hook-ready"))
    );
    let intent =
        f.db.pair_refresh_operation(prepared.pins().git.operation_id)
            .unwrap();
    assert_eq!(intent.phase, PairRefreshPhase::GitIntent);
    assert_eq!(intent.svn_receipt.as_ref().unwrap().revision, 5);
    assert!(intent.git_receipt_sha.is_none());
    assert_eq!(
        f.target
            .find_reference("refs/heads/feature")
            .unwrap()
            .target(),
        Some(f.baseline)
    );
    let requests = std::fs::read_to_string(&f.log)
        .unwrap()
        .lines()
        .filter(|line| line.contains("/git-receive-pack"))
        .count();
    assert!(requests > 0);
    let before = journal_state(&f);
    std::fs::remove_file(hook).unwrap();
    assert!(
        super::super::execution::reconcile_owned_refresh_intent_with_client(
            &engine,
            &actor,
            &prepared,
            &busy,
            f.client.clone(),
        )
        .await
        .is_err()
    );
    assert!(super::super::execution::complete_owned_refresh_with_client(
        &engine,
        &actor,
        &prepared,
        "protected-child",
        &approved,
        &busy,
        f.client.clone(),
    )
    .await
    .is_err());
    assert_eq!(journal_state(&f), before);
    assert_eq!(
        std::fs::read_to_string(&f.log)
            .unwrap()
            .lines()
            .filter(|line| line.contains("/git-receive-pack"))
            .count(),
        requests
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
    assert_eq!(parent_row(&f), parent_before);
    assert!(
        crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&f.db.conn(), None)
            .is_err()
    );
}

fn noop_projection(f: &Fixture) -> Vec<Vec<rusqlite::types::Value>> {
    let conn = f.db.conn();
    let mut result = Vec::new();
    for query in [
        "SELECT * FROM repositories ORDER BY id",
        "SELECT * FROM commit_map ORDER BY id",
        "SELECT * FROM sync_records ORDER BY id",
    ] {
        let mut statement = conn.prepare(query).unwrap();
        let columns = statement.column_count();
        let rows = statement
            .query_map([], |row| {
                (0..columns)
                    .map(|index| row.get(index))
                    .collect::<Result<Vec<rusqlite::types::Value>, _>>()
            })
            .unwrap();
        result.extend(rows.collect::<Result<Vec<_>, _>>().unwrap());
    }
    result
}

fn initial_noop_fixture() -> Fixture {
    // New branches and a separate fresh SVN repository do not rewrite any
    // existing fixture ref/history. These are source-observation fixtures;
    // ordinary-engine initialization qualification remains a separate control.
    let mut f = Fixture::new();
    let repository = f.temp.path().join("initial-noop-svn");
    run(Command::new("svnadmin").arg("create").arg(&repository));
    let root = reqwest::Url::from_file_path(repository.canonicalize().unwrap())
        .unwrap()
        .to_string();
    svn(
        &[
            "mkdir",
            &format!("{root}/trunk"),
            &format!("{root}/branches"),
            "-m",
            "no-op layout",
        ],
        None,
    );
    let working = f.temp.path().join("initial-noop-parent-wc");
    svn(
        &[
            "checkout",
            &format!("{root}/trunk"),
            working.to_str().unwrap(),
        ],
        None,
    );
    std::fs::write(working.join("file.txt"), b"baseline\n").unwrap();
    svn(&["add", "file.txt"], Some(&working));
    svn(&["commit", "-m", "no-op baseline"], Some(&working));
    svn(
        &[
            "copy",
            &format!("{root}/trunk@2"),
            &format!("{root}/branches/f"),
            "-m",
            "no-op direct copy",
        ],
        None,
    );
    f.target
        .reference(
            "refs/heads/unchanged-parent",
            f.baseline,
            false,
            "fresh no-op fixture branch",
        )
        .unwrap();
    f.target
        .reference(
            "refs/heads/unchanged-child",
            f.baseline,
            false,
            "fresh no-op fixture branch",
        )
        .unwrap();
    for (id, branch, revision) in [
        ("parent", "unchanged-parent", 2),
        ("child", "unchanged-child", 3),
    ] {
        f.db.conn().execute("UPDATE repositories SET svn_url=?1,git_branch=?2,last_svn_rev=?3,last_git_sha=?4 WHERE id=?5",rusqlite::params![root,branch,revision,f.baseline.to_string(),id]).unwrap();
        f.db.set_state(&format!("last_svn_rev_{id}"), &revision.to_string())
            .unwrap();
        let scope: String =
            f.db.conn()
                .query_row(
                    "SELECT scope_uuid FROM repositories WHERE id=?1",
                    [id],
                    |row| row.get(0),
                )
                .unwrap();
        f.db.set_state(&format!("last_git_sha_{scope}"), &f.baseline.to_string())
            .unwrap();
    }
    f.root = root;
    f.parent = f.baseline;
    f._parent_wc = working;
    f
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unchanged_parent_after_completion_returns_a_distinct_signed_noop_without_publication() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = f.engine();
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let (actor, _workspace, first) = prepared(&f, &engine).await;
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    let approved = actor.prepared_approval_digest(&engine, &first).unwrap();
    let completed = super::super::execution::complete_owned_refresh_with_client(
        &engine,
        &actor,
        &first,
        "before-noop",
        &approved,
        &busy,
        f.client.clone(),
    )
    .await
    .unwrap();
    let before = noop_projection(&f);
    let original_kv = journal_state(&f);
    let before_writes = std::fs::read_to_string(&f.log)
        .unwrap()
        .lines()
        .filter(|line| line.contains("/git-receive-pack"))
        .count();
    let workspace = actor.create_workspace(&engine).unwrap();
    let path = workspace.pin().operation_directory.path.clone();
    let noop = super::super::noop::prepare_registered_noop_with_client(
        &f.db,
        "child",
        workspace.clone(),
        f.client.clone(),
    )
    .await
    .unwrap();
    assert_eq!(noop.pins().baseline_operation_id, Some(completed.id));
    assert_eq!(noop.pins().parent_baseline_sha, f.parent.to_string());
    let approved = actor.noop_approval_digest(&engine, &noop).unwrap();
    let receipt = super::super::noop::complete_owned_noop_with_client(
        &engine,
        &actor,
        &noop,
        "unchanged-parent",
        &approved,
        &busy,
        f.client.clone(),
    )
    .await
    .unwrap();
    assert_eq!(receipt.version, 1);
    assert_eq!(receipt.id, noop.pins().operation_id);
    assert_ne!(receipt.id, completed.id);
    assert_eq!(receipt.pins.child.last_svn_rev, 5);
    assert_eq!(receipt.pins.parent.last_svn_rev, 4);
    assert_eq!(noop_projection(&f), before);
    assert_eq!(
        journal_state(&f)
            .into_iter()
            .filter(|(key, _, _)| !key.starts_with("pair_refresh_noop_"))
            .collect::<Vec<_>>(),
        original_kv
    );
    assert_eq!(
        f.db.pair_refresh_operation(completed.id).unwrap(),
        completed
    );
    assert!(f.db.pair_refresh_operation(receipt.id).is_err());
    assert_eq!(
        actor
            .noop_request_receipt(&engine, "unchanged-parent", &receipt.pins.child.scope_uuid)
            .unwrap(),
        Some(receipt.clone())
    );
    let original = journal_state(&f);
    let repeated = super::super::noop::complete_owned_noop_with_client(
        &engine,
        &actor,
        &noop,
        "unchanged-parent",
        &approved,
        &busy,
        f.client.clone(),
    )
    .await
    .unwrap();
    assert_eq!(repeated, receipt);
    assert_eq!(journal_state(&f), original);
    assert_eq!(noop_projection(&f), before);
    assert_eq!(
        std::fs::read_to_string(&f.log)
            .unwrap()
            .lines()
            .filter(|line| line.contains("/git-receive-pack"))
            .count(),
        before_writes
    );
    assert_eq!(
        crate::svn::SvnClient::new(format!("{}/branches/f", f.root), "", "")
            .info()
            .await
            .unwrap()
            .latest_rev,
        5
    );
    assert!(
        crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&f.db.conn(), None)
            .is_ok()
    );
    drop(noop);
    drop(workspace);
    assert!(!path.exists());
    assert_eq!(
        actor
            .noop_request_receipt(&engine, "unchanged-parent", &receipt.pins.child.scope_uuid)
            .unwrap(),
        Some(receipt)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_copy_noop_is_atomic_authenticated_and_cannot_become_a_publication_job() {
    let _serial = SERIAL.lock().await;
    let f = initial_noop_fixture();
    let engine = f.engine();
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let actor = engine
        .capture_pair_refresh_actor_policy("disposable-workspace-session")
        .unwrap();
    let workspace = actor.create_workspace(&engine).unwrap();
    let noop = super::super::noop::prepare_registered_noop_with_client(
        &f.db,
        "child",
        workspace.clone(),
        f.client.clone(),
    )
    .await
    .unwrap();
    assert_eq!(noop.pins().baseline_operation_id, None);
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    let before = noop_projection(&f);
    let original = journal_state(&f);
    assert!(!key_table(&f));
    assert!(super::super::noop::complete_owned_noop_with_client(
        &engine,
        &actor,
        &noop,
        "first-noop",
        &"0".repeat(64),
        &busy,
        f.client.clone()
    )
    .await
    .is_err());
    assert!(!key_table(&f));
    assert_eq!(journal_state(&f), original);
    let approved = actor.noop_approval_digest(&engine, &noop).unwrap();
    f.db.conn().execute_batch("CREATE TRIGGER interrupt_noop BEFORE INSERT ON kv_state WHEN NEW.key LIKE 'pair_refresh_noop_v1:request:%' BEGIN SELECT RAISE(ABORT,'disposable no-op interruption'); END;").unwrap();
    assert!(super::super::noop::complete_owned_noop_with_client(
        &engine,
        &actor,
        &noop,
        "first-noop",
        &approved,
        &busy,
        f.client.clone()
    )
    .await
    .is_err());
    assert!(!key_table(&f));
    assert_eq!(journal_state(&f), original);
    f.db.conn()
        .execute_batch("DROP TRIGGER interrupt_noop;")
        .unwrap();
    let receipt = super::super::noop::complete_owned_noop_with_client(
        &engine,
        &actor,
        &noop,
        "first-noop",
        &approved,
        &busy,
        f.client.clone(),
    )
    .await
    .unwrap();
    assert!(key_table(&f));
    assert_eq!(receipt.pins.child.last_git_sha, f.baseline.to_string());
    assert_eq!(receipt.pins.parent.last_git_sha, f.baseline.to_string());
    assert_eq!(receipt.pins.child.last_svn_rev, 3);
    assert_eq!(receipt.pins.parent.last_svn_rev, 2);
    assert_eq!(noop_projection(&f), before);
    assert!(!std::fs::read_to_string(&f.log)
        .unwrap()
        .contains("receive-pack"));
    assert_eq!(
        crate::svn::SvnClient::new(format!("{}/branches/f", f.root), "", "")
            .info()
            .await
            .unwrap()
            .latest_rev,
        3
    );
    assert!(
        crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&f.db.conn(), None)
            .is_ok()
    );
    let key = format!("pair_refresh_noop_v1:document:{}", receipt.id);
    let raw = f.db.get_state(&key).unwrap().unwrap();
    let mut altered: serde_json::Value = serde_json::from_str(&raw).unwrap();
    altered["receipt"]["pins"]["git_tree"] = serde_json::Value::String("a".repeat(40));
    f.db.conn()
        .execute(
            "UPDATE kv_state SET value=?1 WHERE key=?2",
            rusqlite::params![serde_json::to_string(&altered).unwrap(), key],
        )
        .unwrap();
    assert!(
        crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&f.db.conn(), None)
            .is_err()
    );
    assert!(actor
        .noop_request_receipt(&engine, "first-noop", &receipt.pins.child.scope_uuid)
        .is_err());
    f.db.conn()
        .execute(
            "UPDATE kv_state SET value=?1 WHERE key=?2",
            rusqlite::params![raw, key],
        )
        .unwrap();
    f.db.conn()
        .execute(
            "DELETE FROM kv_state WHERE key LIKE 'pair_refresh_noop_v1:request:%'",
            [],
        )
        .unwrap();
    assert!(
        crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&f.db.conn(), None)
            .is_err()
    );
    assert_eq!(noop_projection(&f), before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn noop_refuses_git_and_svn_drift_without_establishing_receipt_authority() {
    let _serial = SERIAL.lock().await;
    for git_drift in [false, true] {
        let f = initial_noop_fixture();
        let engine = f.engine();
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
        let actor = engine
            .capture_pair_refresh_actor_policy("disposable-workspace-session")
            .unwrap();
        let workspace = actor.create_workspace(&engine).unwrap();
        let noop = super::super::noop::prepare_registered_noop_with_client(
            &f.db,
            "child",
            workspace,
            f.client.clone(),
        )
        .await
        .unwrap();
        let approved = actor.noop_approval_digest(&engine, &noop).unwrap();
        let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
        if git_drift {
            let advanced = commit(&f.target, b"external Git parent advance\n", &[f.baseline]);
            f.target
                .find_reference("refs/heads/unchanged-parent")
                .unwrap()
                .set_target(advanced, "external fixture fast-forward")
                .unwrap();
            assert!(f.target.graph_descendant_of(advanced, f.baseline).unwrap());
        } else {
            std::fs::write(
                f._parent_wc.join("file.txt"),
                b"external SVN parent advance\n",
            )
            .unwrap();
            svn(
                &["commit", "-m", "external fixture parent advance"],
                Some(&f._parent_wc),
            );
        }
        let before = noop_projection(&f);
        let original = journal_state(&f);
        assert!(super::super::noop::complete_owned_noop_with_client(
            &engine,
            &actor,
            &noop,
            "drifted-noop",
            &approved,
            &busy,
            f.client.clone()
        )
        .await
        .is_err());
        assert!(!key_table(&f));
        assert_eq!(journal_state(&f), original);
        assert_eq!(noop_projection(&f), before);
        assert!(!std::fs::read_to_string(&f.log)
            .unwrap()
            .contains("receive-pack"));
        assert_eq!(
            crate::svn::SvnClient::new(format!("{}/branches/f", f.root), "", "")
                .info()
                .await
                .unwrap()
                .latest_rev,
            if git_drift { 3 } else { 4 }
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn noop_refuses_revocation_session_loss_competing_window_unknown_history_and_foreign_busy() {
    let _serial = SERIAL.lock().await;
    for case in ["credential", "session", "window", "unknown-history", "busy"] {
        let f = initial_noop_fixture();
        let engine = f.engine();
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
        let actor = engine
            .capture_pair_refresh_actor_policy("disposable-workspace-session")
            .unwrap();
        let workspace = actor.create_workspace(&engine).unwrap();
        let noop = super::super::noop::prepare_registered_noop_with_client(
            &f.db,
            "child",
            workspace,
            f.client.clone(),
        )
        .await
        .unwrap();
        let approved = actor.noop_approval_digest(&engine, &noop).unwrap();
        let mut window = None;
        match case {
            "credential" => {
                f.db.set_state("secret_git_token_child", "").unwrap();
            }
            "session" => {
                f.db.delete_session("disposable-workspace-session").unwrap();
            }
            "window" => {
                window = Some(
                    f.db.begin_ordinary_writer_window(Some("unregistered-alias"))
                        .unwrap(),
                );
            }
            "unknown-history" => {
                f.db.set_state("pair_refresh_noop_v99:document:unknown", "corrupt")
                    .unwrap();
            }
            "busy" => {}
            _ => unreachable!(),
        }
        let busy = if case == "busy" {
            crate::busy::try_acquire_pair("foreign-one", "foreign-two").unwrap()
        } else {
            crate::busy::try_acquire_pair("child", "parent").unwrap()
        };
        let before = noop_projection(&f);
        let original = journal_state(&f);
        assert!(
            super::super::noop::complete_owned_noop_with_client(
                &engine,
                &actor,
                &noop,
                "refused-noop",
                &approved,
                &busy,
                f.client.clone()
            )
            .await
            .is_err(),
            "{case}"
        );
        assert!(!key_table(&f), "{case}");
        assert_eq!(journal_state(&f), original, "{case}");
        assert_eq!(noop_projection(&f), before, "{case}");
        assert!(
            !std::fs::read_to_string(&f.log)
                .unwrap()
                .contains("receive-pack"),
            "{case}"
        );
        assert_eq!(
            crate::svn::SvnClient::new(format!("{}/branches/f", f.root), "", "")
                .info()
                .await
                .unwrap()
                .latest_rev,
            3,
            "{case}"
        );
        if let Some(window) = window {
            window.finish_preparation().unwrap();
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn signed_noop_history_does_not_replace_the_copy_baseline_for_a_later_real_integration() {
    let _serial = SERIAL.lock().await;
    let f = initial_noop_fixture();
    let engine = f.engine();
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let actor = engine
        .capture_pair_refresh_actor_policy("disposable-workspace-session")
        .unwrap();
    let workspace = actor.create_workspace(&engine).unwrap();
    let noop = super::super::noop::prepare_registered_noop_with_client(
        &f.db,
        "child",
        workspace.clone(),
        f.client.clone(),
    )
    .await
    .unwrap();
    let approved = actor.noop_approval_digest(&engine, &noop).unwrap();
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    let receipt = super::super::noop::complete_owned_noop_with_client(
        &engine,
        &actor,
        &noop,
        "noop-before-advance",
        &approved,
        &busy,
        f.client.clone(),
    )
    .await
    .unwrap();
    let original_noop =
        f.db.get_state(&format!("pair_refresh_noop_v1:document:{}", receipt.id))
            .unwrap();
    drop(noop);
    drop(workspace);
    // Parent handling is modeled with production checkpoint helpers in this
    // source fixture. Full ordinary-engine qualification remains separate.
    std::fs::write(f._parent_wc.join("file.txt"), b"advance after no-op\n").unwrap();
    svn(
        &["commit", "-m", "fixture parent advance after no-op"],
        Some(&f._parent_wc),
    );
    let advanced = commit(&f.target, b"advance after no-op\n", &[f.baseline]);
    f.target
        .find_reference("refs/heads/unchanged-parent")
        .unwrap()
        .set_target(advanced, "fixture parent fast-forward")
        .unwrap();
    f.db.insert_commit_map_with_repo(
        4,
        &advanced.to_string(),
        "svn_to_git",
        "fixture",
        "fixture",
        Some("parent"),
    )
    .unwrap();
    f.db.update_repo_watermark("parent", 4, &advanced.to_string())
        .unwrap();
    crate::echo_receipt_scope::write_scoped_last_git_sha_kv(
        &f.db.conn(),
        "parent",
        &advanced.to_string(),
        &chrono::Utc::now().to_rfc3339(),
    )
    .unwrap();
    let parent_before = parent_row(&f);
    let workspace = actor.create_workspace(&engine).unwrap();
    let prepared =
        prepare_registered_in_workspace_for_test(&f.db, "child", workspace, f.client.clone())
            .await
            .unwrap();
    assert_eq!(
        prepared.pins().git.pins.parent_baseline_tip,
        f.baseline.to_string()
    );
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
    let approved = actor.prepared_approval_digest(&engine, &prepared).unwrap();
    let completed = super::super::execution::complete_owned_refresh_with_client(
        &engine,
        &actor,
        &prepared,
        "advance-after-noop",
        &approved,
        &busy,
        f.client.clone(),
    )
    .await
    .unwrap();
    assert_ne!(completed.id, receipt.id);
    assert_eq!(
        completed.phase,
        crate::db::pair_refresh_operations::PairRefreshPhase::Completed
    );
    assert_eq!(completed.svn_receipt.as_ref().unwrap().revision, 5);
    assert_eq!(parent_row(&f), parent_before);
    let tip = f
        .target
        .find_reference("refs/heads/unchanged-child")
        .unwrap()
        .target()
        .unwrap();
    assert_eq!(
        f.target
            .find_commit(tip)
            .unwrap()
            .parent_ids()
            .collect::<Vec<_>>(),
        vec![f.baseline, advanced]
    );
    assert!(f.target.graph_descendant_of(tip, f.baseline).unwrap());
    assert!(f.target.graph_descendant_of(tip, advanced).unwrap());
    assert_eq!(
        actor
            .noop_request_receipt(
                &engine,
                "noop-before-advance",
                &receipt.pins.child.scope_uuid
            )
            .unwrap(),
        Some(receipt.clone())
    );
    assert_eq!(
        f.db.get_state(&format!("pair_refresh_noop_v1:document:{}", receipt.id))
            .unwrap(),
        original_noop
    );
    assert!(
        crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&f.db.conn(), None)
            .is_ok()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn new_epoch_and_unknown_or_live_native_owner_cannot_adopt_signed_resources() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = f.engine();
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let (actor, workspace, prepared) = prepared(&f, &engine).await;
    let approved = actor.prepared_approval_digest(&engine, &prepared).unwrap();
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    let operation = actor
        .enroll_prepared(&engine, &prepared, "native-owner-refusal", &approved, &busy)
        .unwrap();
    let old_epoch = fence.epoch();
    let prior_lease = f.db.get_state("writer_fence_v1:lease").unwrap().unwrap();
    drop(fence);
    let before_refused_claim = f.state();
    assert!(crate::writer_fence::claim(engine.db(), f.temp.path()).is_err());
    assert_eq!(f.state(), before_refused_claim);

    // Keep exercising the later recovery gate against a stale ordinary epoch,
    // even though ordinary startup now refuses to create that state. Acquire a
    // new epoch in a separate disposable database and copy only its lease rows
    // into this fixture. The signed journal and private key remain untouched.
    let held_journal: Vec<_> = before_refused_claim
        .iter()
        .filter(|(key, _, _)| key.starts_with("pair_refresh_execution_v3:"))
        .cloned()
        .collect();
    assert!(!held_journal.is_empty());
    let foreign_epoch_dir = tempfile::tempdir().unwrap();
    let foreign_epoch_db = Database::new(foreign_epoch_dir.path().join("reposync.db")).unwrap();
    foreign_epoch_db.initialize().unwrap();
    let mut stale_lease: serde_json::Value = serde_json::from_str(&prior_lease).unwrap();
    stale_lease["holder_pid"] = serde_json::json!(0);
    foreign_epoch_db
        .set_state(
            "writer_fence_v1:lease",
            &serde_json::to_string(&stale_lease).unwrap(),
        )
        .unwrap();
    let next = crate::writer_fence::claim(&foreign_epoch_db, foreign_epoch_dir.path()).unwrap();
    let foreign_rows: Vec<(String, String, String)> = foreign_epoch_db
        .conn()
        .prepare("SELECT key,value,updated_at FROM kv_state WHERE key IN ('writer_fence_v1:lease','writer_fence_v1:native') ORDER BY key")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    f.db.immediate_transaction(|tx| {
        for (key, value, updated_at) in &foreign_rows {
            tx.execute(
                "INSERT INTO kv_state(key,value,updated_at) VALUES(?1,?2,?3)",
                rusqlite::params![key, value, updated_at],
            )?;
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(
        f.state()
            .into_iter()
            .filter(|(key, _, _)| key.starts_with("pair_refresh_execution_v3:"))
            .collect::<Vec<_>>(),
        held_journal
    );
    assert!(next.epoch() > old_epoch);
    let state = journal_state(&f);
    assert!(super::super::recovery::prepare_owned_refresh_recovery(
        &engine,
        &actor,
        operation.id,
        &busy
    )
    .is_err());
    assert_eq!(journal_state(&f), state);
    assert_eq!(
        f.db.pair_refresh_operation(operation.id).unwrap(),
        operation
    );
    assert!(
        crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&f.db.conn(), None)
            .is_err()
    );
    let working_copy = prepared.pins().svn.working_copy.clone();
    drop(prepared);
    drop(workspace);
    assert!(working_copy.join(".svn").is_dir());
    assert!(!std::fs::read_to_string(&f.log)
        .unwrap()
        .contains("receive-pack"));
}

#[tokio::test(flavor = "current_thread")]
async fn native_recovery_fixture_worker() {
    let Ok(root) = std::env::var("REPOSYNC_NATIVE_RECOVERY_FIXTURE_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    assert_eq!(root.canonicalize().unwrap(), root);
    let stage = std::env::var("REPOSYNC_NATIVE_RECOVERY_FIXTURE_STAGE").unwrap();
    assert!(matches!(
        stage.as_str(),
        "prepared" | "svn_intent" | "git_intent"
    ));
    let db = Database::new(root.join("reposync.db")).unwrap();
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
    config.daemon.data_dir = root.clone();
    let engine = crate::sync_engine::SyncEngine::new(
        config,
        db,
        crate::svn::SvnClient::new("file:///unused/svn", "", ""),
        crate::git::client::GitClient::new(root.join("engine-git")).unwrap(),
        std::sync::Arc::new(
            crate::identity::mapper::IdentityMapper::new(&crate::config::IdentityConfig::default())
                .unwrap(),
        ),
    );
    let _lock = crate::data_dir::acquire(&root).unwrap();
    let _fence = crate::writer_fence::claim_with_identity(
        engine.db(),
        &root,
        "native-recovery-fixture",
        std::process::id(),
    )
    .unwrap();
    let actor = engine
        .capture_pair_refresh_actor_policy("disposable-workspace-session")
        .unwrap();
    let workspace = actor.create_workspace(&engine).unwrap();
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(10))
        .add_root_certificate(
            reqwest::Certificate::from_der(&std::fs::read(root.join("cert.der")).unwrap()).unwrap(),
        )
        .build()
        .unwrap();
    let prepared =
        prepare_registered_in_workspace_for_test(engine.db(), "child", workspace, client.clone())
            .await
            .unwrap();
    let approved = actor.prepared_approval_digest(&engine, &prepared).unwrap();
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    match stage.as_str() {
        "prepared" => {
            actor
                .enroll_prepared(&engine, &prepared, "native-worker", &approved, &busy)
                .unwrap();
        }
        "svn_intent" => {
            engine.db().conn().execute_batch("CREATE TRIGGER native_interrupt BEFORE INSERT ON kv_state WHEN NEW.key LIKE 'pair_refresh_execution_v3:document:%' AND json_extract(NEW.value,'$.phase')='svn_observed' BEGIN SELECT RAISE(ABORT,'native fixture lost SVN receipt'); END;").unwrap();
            let error = super::super::execution::publish_owned_svn_stage_with_client(
                &engine,
                &actor,
                &prepared,
                "native-worker",
                &approved,
                &busy,
                client.clone(),
            )
            .await
            .unwrap_err()
            .to_string();
            assert!(error.contains("native fixture lost SVN receipt"), "{error}");
            engine
                .db()
                .conn()
                .execute_batch("DROP TRIGGER native_interrupt;")
                .unwrap();
        }
        "git_intent" => {
            engine.db().conn().execute_batch("CREATE TRIGGER native_interrupt BEFORE INSERT ON kv_state WHEN NEW.key LIKE 'pair_refresh_execution_v3:document:%' AND json_extract(NEW.value,'$.phase')='git_observed' BEGIN SELECT RAISE(ABORT,'native fixture lost Git receipt'); END;").unwrap();
            let error = super::super::execution::complete_owned_refresh_with_client(
                &engine,
                &actor,
                &prepared,
                "native-worker",
                &approved,
                &busy,
                client.clone(),
            )
            .await
            .unwrap_err()
            .to_string();
            assert!(error.contains("native fixture lost Git receipt"), "{error}");
            engine
                .db()
                .conn()
                .execute_batch("DROP TRIGGER native_interrupt;")
                .unwrap();
        }
        _ => unreachable!(),
    }
    println!(
        "REPOSYNC_NATIVE_RECOVERY_READY {}",
        serde_json::json!({"id":prepared.pins().git.operation_id,"pid":std::process::id(),"stage":stage})
    );
    std::io::Write::flush(&mut std::io::stdout()).unwrap();
    // Deliberately skip Rust guard destruction. The parent verifies the durable
    // lease and signed resources survive, then proves this actual PID absent.
    std::process::exit(0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_worker_exit_recovers_prepared_and_both_uncertain_publication_intents() {
    use crate::db::pair_refresh_operations::PairRefreshPhase;
    let _serial = SERIAL.lock().await;
    for stage in ["prepared", "svn_intent", "git_intent"] {
        let f = Fixture::new();
        let engine = f.engine();
        let parent_before = parent_row(&f);
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args(["--exact","pair_refresh::live_preparation::authenticated_enrollment_tests::native_recovery_fixture_worker","--nocapture","--test-threads=1"])
            .env("REPOSYNC_NATIVE_RECOVERY_FIXTURE_ROOT",f.temp.path().canonicalize().unwrap())
            .env("REPOSYNC_NATIVE_RECOVERY_FIXTURE_STAGE",stage);
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        let marker = stdout
            .lines()
            .find_map(|line| {
                line.split_once("REPOSYNC_NATIVE_RECOVERY_READY ")
                    .map(|(_, value)| value)
            })
            .expect("worker did not reach its durable phase");
        let ready: serde_json::Value = serde_json::from_str(marker).unwrap();
        let id = Uuid::parse_str(ready["id"].as_str().unwrap()).unwrap();
        let operation = f.db.pair_refresh_operation(id).unwrap();
        assert_eq!(
            operation.phase,
            match stage {
                "prepared" => PairRefreshPhase::Prepared,
                "svn_intent" => PairRefreshPhase::SvnIntent,
                "git_intent" => PairRefreshPhase::GitIntent,
                _ => unreachable!(),
            }
        );
        let authority_key = format!("pair_refresh_execution_v3:authority:{id}");
        let raw = f.db.get_state(&authority_key).unwrap().unwrap();
        let original: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let native = original["evidence"]["resources"]["workspace"].get("native_owner");
        #[cfg(target_os = "linux")]
        assert!(
            native.is_some(),
            "Linux worker must record native owner identity"
        );
        if let Some(native) = native {
            assert_eq!(native["pid"], ready["pid"]);
        }
        let lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let actor = engine
            .capture_pair_refresh_actor_policy("disposable-workspace-session")
            .unwrap();
        let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
        let before = journal_state(&f);
        let claimed = super::super::recovery::claim_owned_refresh_recovery_writer(
            &engine, &actor, id, &busy, &lock,
        );
        if native.is_none() {
            assert!(claimed.is_err());
            assert_eq!(journal_state(&f), before);
            assert_eq!(f.db.pair_refresh_operation(id).unwrap(), operation);
            assert!(
                crate::db::pair_refresh_operations::require_no_pair_refresh_hold(
                    &f.db.conn(),
                    None
                )
                .is_err()
            );
            assert_eq!(parent_row(&f), parent_before);
            continue;
        }
        let fence = claimed.unwrap();
        assert!(fence.epoch() > operation.admitted_writer_epoch);
        let recovery =
            super::super::recovery::prepare_owned_refresh_recovery(&engine, &actor, id, &busy)
                .unwrap();
        let approved = recovery.approval_digest(&engine, &actor).unwrap();
        let recovered = recovery
            .transfer(&engine, &actor, &approved, &busy)
            .unwrap();
        assert_eq!(recovered.operation(), &operation);
        let transferred: serde_json::Value =
            serde_json::from_str(&f.db.get_state(&authority_key).unwrap().unwrap()).unwrap();
        assert_eq!(transferred["evidence"], original["evidence"]);
        assert_eq!(transferred["recoveries"].as_array().unwrap().len(), 1);
        assert_eq!(
            transferred["recoveries"][0]["writer_epoch"].as_u64(),
            Some(fence.epoch())
        );
        if stage != "prepared" {
            let observed = super::super::execution::reconcile_owned_refresh_intent_with_client(
                &engine,
                &actor,
                recovered.prepared_for_test(),
                &busy,
                f.client.clone(),
            )
            .await
            .unwrap();
            assert_eq!(
                observed.phase,
                if stage == "svn_intent" {
                    PairRefreshPhase::SvnObserved
                } else {
                    PairRefreshPhase::GitObserved
                }
            );
        }
        let completed = super::super::execution::complete_owned_refresh_with_client(
            &engine,
            &actor,
            recovered.prepared_for_test(),
            &operation.request_id,
            &operation.approval_digest,
            &busy,
            f.client.clone(),
        )
        .await
        .unwrap();
        assert_eq!(completed.phase, PairRefreshPhase::Completed);
        assert_eq!(completed.id, operation.id);
        assert_eq!(
            completed.admitted_writer_epoch,
            operation.admitted_writer_epoch
        );
        assert_eq!(completed.svn_receipt.as_ref().unwrap().revision, 5);
        assert_eq!(parent_row(&f), parent_before);
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
                .target()
                .unwrap()
                .to_string(),
            operation.pins.git.prepared_sha
        );
        assert_eq!(
            std::fs::read_to_string(&f.log)
                .unwrap()
                .lines()
                .filter(|line| line.contains("/git-receive-pack"))
                .count(),
            1
        );
        assert!(
            crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&f.db.conn(), None)
                .is_ok()
        );
        let final_authority: serde_json::Value =
            serde_json::from_str(&f.db.get_state(&authority_key).unwrap().unwrap()).unwrap();
        assert_eq!(final_authority["evidence"], original["evidence"]);
        assert_eq!(final_authority["recoveries"], transferred["recoveries"]);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn authenticated_recovery_claim_refuses_live_or_unknown_owner_and_foreign_lock() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = f.engine();
    let lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let (actor, workspace, prepared) = prepared(&f, &engine).await;
    let approved = actor.prepared_approval_digest(&engine, &prepared).unwrap();
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    let operation = actor
        .enroll_prepared(&engine, &prepared, "native-claim-refusal", &approved, &busy)
        .unwrap();
    let before = journal_state(&f);
    let lease = f.db.get_state("writer_fence_v1:lease").unwrap();
    assert!(super::super::recovery::claim_owned_refresh_recovery_writer(
        &engine,
        &actor,
        operation.id,
        &busy,
        &lock,
    )
    .is_err());
    assert_eq!(journal_state(&f), before);
    assert_eq!(f.db.get_state("writer_fence_v1:lease").unwrap(), lease);
    let foreign = tempfile::tempdir().unwrap();
    let foreign_lock = crate::data_dir::acquire(foreign.path()).unwrap();
    assert!(super::super::recovery::claim_owned_refresh_recovery_writer(
        &engine,
        &actor,
        operation.id,
        &busy,
        &foreign_lock,
    )
    .is_err());
    assert_eq!(journal_state(&f), before);
    assert_eq!(f.db.get_state("writer_fence_v1:lease").unwrap(), lease);
    assert_eq!(
        f.db.pair_refresh_operation(operation.id).unwrap(),
        operation
    );
    assert!(
        crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&f.db.conn(), None,)
            .is_err()
    );
    assert!(workspace.pin().operation_directory.path.is_dir());
    assert!(prepared.pins().svn.working_copy.is_dir());
    assert_eq!(
        f.target
            .find_reference("refs/heads/feature")
            .unwrap()
            .target(),
        Some(f.baseline)
    );
}

#[cfg(target_os = "linux")]
#[path = "native_claim_crash_tests.rs"]
mod native_claim_crash_tests;

#[path = "real_engine_refresh_tests.rs"]
mod real_engine_refresh_tests;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn authenticated_request_lookup_is_read_only_scoped_and_rejects_corruption_and_logout() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = f.engine();
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let (actor, _workspace, prepared) = prepared(&f, &engine).await;
    let scope = prepared.pins().child.scope_uuid.clone();
    let before = journal_state(&f);
    assert!(actor
        .refresh_request_receipt(&engine, "lookup-request", &scope)
        .unwrap()
        .is_none());
    assert_eq!(journal_state(&f), before);
    assert!(!key_table(&f));
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    let digest = actor.prepared_approval_digest(&engine, &prepared).unwrap();
    let admitted = actor
        .enroll_prepared(&engine, &prepared, "lookup-request", &digest, &busy)
        .unwrap();
    let saved = journal_state(&f);
    assert_eq!(
        actor
            .refresh_request_receipt(&engine, "lookup-request", &scope)
            .unwrap(),
        Some(admitted.clone())
    );
    assert!(actor
        .refresh_request_receipt(&engine, "other-request", &scope)
        .unwrap()
        .is_none());
    assert!(actor
        .refresh_request_receipt(&engine, "lookup-request", &Uuid::new_v4().to_string())
        .unwrap()
        .is_none());
    assert_eq!(journal_state(&f), saved);
    let cancelled = actor
        .cancel_prepared_enrollment(&engine, &prepared, &busy)
        .unwrap();
    let saved = journal_state(&f);
    assert_eq!(
        actor
            .refresh_request_receipt(&engine, "lookup-request", &scope)
            .unwrap(),
        Some(cancelled)
    );
    assert_eq!(journal_state(&f), saved);
    let authority = format!("pair_refresh_execution_v3:authority:{}", admitted.id);
    let original = f.db.get_state(&authority).unwrap().unwrap();
    f.db.conn()
        .execute("UPDATE kv_state SET value='{}' WHERE key=?1", [&authority])
        .unwrap();
    let corrupt = journal_state(&f);
    assert!(actor
        .refresh_request_receipt(&engine, "lookup-request", &scope)
        .is_err());
    assert_eq!(journal_state(&f), corrupt);
    f.db.conn()
        .execute(
            "UPDATE kv_state SET value=?1 WHERE key=?2",
            rusqlite::params![original, authority],
        )
        .unwrap();
    f.db.delete_session("disposable-workspace-session").unwrap();
    let logged_out = journal_state(&f);
    assert!(actor
        .refresh_request_receipt(&engine, "lookup-request", &scope)
        .is_err());
    assert_eq!(journal_state(&f), logged_out);
    assert_eq!(
        f.target
            .find_reference("refs/heads/feature")
            .unwrap()
            .target(),
        Some(f.baseline)
    );
    assert!(!std::fs::read_to_string(&f.log)
        .unwrap()
        .contains("receive-pack"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owned_service_retains_preparation_and_returns_exact_publication_and_noop_receipts() {
    use crate::pair_refresh::service::{RefreshAction, RefreshResponse, RefreshService};
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = std::sync::Arc::new(f.engine());
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let service = RefreshService::with_fixture_client(engine.clone(), f.client.clone());
    let token = "disposable-workspace-session";
    let approval = match service
        .prepare(token.into(), "child".into(), "service-request".into())
        .await
        .unwrap()
    {
        RefreshResponse::Approval(approval) => approval,
        other => panic!("unexpected {other:?}"),
    };
    assert_eq!(approval.kind, "integration");
    assert_ne!(approval.preparation_id, approval.operation_id);
    assert!(f.db.pair_refresh_operation(approval.operation_id).is_err());
    let before = journal_state(&f);
    assert!(service
        .act(
            token.into(),
            "child",
            "service-request",
            approval.preparation_id,
            "wrong".into(),
            RefreshAction::Execute
        )
        .await
        .is_err());
    assert_eq!(journal_state(&f), before);
    let receipt = service
        .act(
            token.into(),
            "child",
            "service-request",
            approval.preparation_id,
            approval.approval_digest.clone(),
            RefreshAction::Execute,
        )
        .await
        .unwrap();
    assert_eq!(receipt.operation_id, approval.operation_id);
    assert_eq!(receipt.phase, "completed");
    assert_eq!(receipt.svn_revision, Some(5));
    assert_eq!(
        f.db.get_repo_watermark("child").unwrap(),
        (5, receipt.git_sha.clone().unwrap())
    );
    assert_eq!(
        f.db.get_repo_watermark("parent").unwrap(),
        (4, f.parent.to_string())
    );
    let publications = || {
        std::fs::read_to_string(&f.log)
            .unwrap()
            .lines()
            .filter(|line| line.contains("/git-receive-pack"))
            .count()
    };
    assert_eq!(publications(), 1);
    let retry = service
        .act(
            token.into(),
            "child",
            "service-request",
            approval.preparation_id,
            approval.approval_digest,
            RefreshAction::Execute,
        )
        .await
        .unwrap();
    assert_eq!(retry.operation_id, receipt.operation_id);
    assert_eq!(retry.git_sha, receipt.git_sha);
    assert_eq!(publications(), 1);
    match service
        .prepare(token.into(), "child".into(), "service-request".into())
        .await
        .unwrap()
    {
        RefreshResponse::Receipt(found) => assert_eq!(found.operation_id, receipt.operation_id),
        other => panic!("unexpected {other:?}"),
    }
    let noop = match service
        .prepare(token.into(), "child".into(), "service-noop".into())
        .await
        .unwrap()
    {
        RefreshResponse::Approval(approval) => approval,
        other => panic!("unexpected {other:?}"),
    };
    assert_eq!(noop.kind, "no_op");
    let before = f.db.get_repo_watermark("child").unwrap();
    let noop_receipt = service
        .act(
            token.into(),
            "child",
            "service-noop",
            noop.preparation_id,
            noop.approval_digest,
            RefreshAction::Execute,
        )
        .await
        .unwrap();
    assert_eq!(noop_receipt.phase, "no_op");
    assert_eq!(noop_receipt.operation_id, noop.operation_id);
    assert_ne!(noop_receipt.operation_id, receipt.operation_id);
    assert_eq!(f.db.get_repo_watermark("child").unwrap(), before);
    assert_eq!(publications(), 1);
    assert_eq!(
        crate::svn::SvnClient::new(&f.root, "", "")
            .info()
            .await
            .unwrap()
            .latest_rev,
        5
    );
    assert!(
        crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&f.db.conn(), None)
            .is_ok()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owned_service_finishes_a_disconnected_request_without_republication() {
    use crate::pair_refresh::service::{RefreshAction, RefreshResponse, RefreshService};
    use std::os::unix::fs::PermissionsExt;
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = std::sync::Arc::new(f.engine());
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let service = RefreshService::with_fixture_client(engine, f.client.clone());
    let token = "disposable-workspace-session";
    let approval = match service
        .prepare(token.into(), "child".into(), "disconnected-request".into())
        .await
        .unwrap()
    {
        RefreshResponse::Approval(approval) => approval,
        other => panic!("unexpected {other:?}"),
    };
    let hook = f.temp.path().join("svn-repository/hooks/pre-commit");
    std::fs::write(
        &hook,
        r#"#!/usr/bin/env python3
from pathlib import Path
import time
root=Path(__file__).resolve().parents[2]
(root/'http-disconnect-ready').write_text('ready')
for _ in range(600):
    if (root/'http-disconnect-release').exists():
        raise SystemExit(0)
    time.sleep(0.05)
raise SystemExit(1)
"#,
    )
    .unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    let _svn_diag = SvnDiag::arm(f.temp.path());
    let handle = approval.preparation_id;
    let digest = approval.approval_digest.clone();
    let diagnostic_digest = digest.clone();
    let request_service = service.clone();
    let mut request = tokio::spawn(async move {
        request_service
            .act(
                token.into(),
                "child",
                "disconnected-request",
                handle,
                digest,
                RefreshAction::Execute,
            )
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(45), async {
        while !f.temp.path().join("http-disconnect-ready").is_file() {
            assert!(
                !request.is_finished(),
                "request ended before entering actual SVN publication: {}",
                {
                    let detail = match (&mut request).await {
                        Ok(Err(error)) => format!("{error:#}"),
                        Ok(Ok(_)) => "success-before-barrier".to_owned(),
                        Err(error) => format!("request join failed: {error}"),
                    };
                    let mut detail = crate::errors::redact_vcs_error_detail(&detail);
                    for secret in [
                        token,
                        "disposable-preparation-token",
                        diagnostic_digest.as_str(),
                    ] {
                        detail = detail.replace(secret, "[REDACTED_FIXTURE_SECRET]");
                    }
                    detail.push(' ');
                    detail.push_str(&case_diag(
                        f.temp.path(),
                        &hook,
                        &f.temp.path().join("http-disconnect-ready"),
                    ));
                    detail
                }
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        f.db.pair_refresh_operation(approval.operation_id)
            .unwrap()
            .phase,
        crate::db::pair_refresh_operations::PairRefreshPhase::SvnIntent
    );
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());
    std::fs::write(f.temp.path().join("http-disconnect-release"), b"release").unwrap();
    let receipt = tokio::time::timeout(std::time::Duration::from_secs(45), async {
        loop {
            if let Some(receipt) = service
                .receipt(token, "child", "disconnected-request")
                .unwrap()
            {
                if receipt.phase == "completed" {
                    break receipt;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(receipt.operation_id, approval.operation_id);
    assert_eq!(receipt.svn_revision, Some(5));
    let retry = service
        .act(
            token.into(),
            "child",
            "disconnected-request",
            approval.preparation_id,
            approval.approval_digest,
            RefreshAction::Execute,
        )
        .await
        .unwrap();
    assert_eq!(retry.operation_id, receipt.operation_id);
    assert_eq!(
        std::fs::read_to_string(&f.log)
            .unwrap()
            .lines()
            .filter(|line| line.contains("/git-receive-pack"))
            .count(),
        1
    );
    assert_eq!(
        crate::svn::SvnClient::new(&f.root, "", "")
            .info()
            .await
            .unwrap()
            .latest_rev,
        5
    );
    assert_eq!(
        f.db.get_repo_watermark("child").unwrap(),
        (5, receipt.git_sha.unwrap())
    );
    assert!(
        crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&f.db.conn(), None)
            .is_ok()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owned_service_recovery_refuses_live_owner_without_claim_or_transfer() {
    use crate::pair_refresh::service::RefreshService;
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = std::sync::Arc::new(f.engine());
    let lock = std::sync::Arc::new(crate::data_dir::acquire(f.temp.path()).unwrap());
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let (actor, workspace, prepared) = prepared(&f, &engine).await;
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    let digest = actor.prepared_approval_digest(&engine, &prepared).unwrap();
    let op = actor
        .enroll_prepared(&engine, &prepared, "live-recovery-refusal", &digest, &busy)
        .unwrap();
    let saved = journal_state(&f);
    let lease = f.db.get_state("writer_fence_v1:lease").unwrap();
    let native = f.db.get_state("writer_fence_v1:native").unwrap();
    drop(busy);
    let ordinary = RefreshService::with_fixture_client(engine.clone(), f.client.clone());
    assert!(ordinary
        .prepare_recovery(
            "disposable-workspace-session".into(),
            "child".into(),
            "live-recovery-refusal".into()
        )
        .await
        .is_err());
    let recovery = RefreshService::recovery_only(engine.clone(), lock).unwrap();
    assert!(recovery
        .prepare_recovery(
            "disposable-workspace-session".into(),
            "child".into(),
            "live-recovery-refusal".into()
        )
        .await
        .is_err());
    assert!(recovery
        .prepare(
            "disposable-workspace-session".into(),
            "child".into(),
            "new-request".into()
        )
        .await
        .is_err());
    assert_eq!(journal_state(&f), saved);
    assert_eq!(f.db.get_state("writer_fence_v1:lease").unwrap(), lease);
    assert_eq!(f.db.get_state("writer_fence_v1:native").unwrap(), native);
    assert_eq!(f.db.pair_refresh_operation(op.id).unwrap(), op);
    assert!(workspace.pin().operation_directory.path.is_dir());
    assert!(prepared.pins().svn.working_copy.is_dir());
    assert!(
        crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&f.db.conn(), None)
            .is_err()
    );
    assert_eq!(
        f.target
            .find_reference("refs/heads/feature")
            .unwrap()
            .target(),
        Some(f.baseline)
    );
    assert!(!std::fs::read_to_string(&f.log)
        .unwrap()
        .contains("receive-pack"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owned_service_native_restart_handles_prepared_and_uncertain_svn_git() {
    use crate::pair_refresh::service::{RefreshAction, RefreshResponse, RefreshService};
    let _serial = SERIAL.lock().await;
    for stage in ["prepared", "svn_intent", "git_intent"] {
        let f = Fixture::new();
        let engine = std::sync::Arc::new(f.engine());
        let parent_before = parent_row(&f);
        let output = run(Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "pair_refresh::live_preparation::authenticated_enrollment_tests::native_recovery_fixture_worker", "--nocapture", "--test-threads=1"])
            .env("REPOSYNC_NATIVE_RECOVERY_FIXTURE_ROOT", f.temp.path().canonicalize().unwrap())
            .env("REPOSYNC_NATIVE_RECOVERY_FIXTURE_STAGE", stage));
        let ready: serde_json::Value = serde_json::from_str(
            output
                .lines()
                .find_map(|line| {
                    line.split_once("REPOSYNC_NATIVE_RECOVERY_READY ")
                        .map(|(_, value)| value)
                })
                .unwrap(),
        )
        .unwrap();
        let id = Uuid::parse_str(ready["id"].as_str().unwrap()).unwrap();
        let op = f.db.pair_refresh_operation(id).unwrap();
        let authority_key = format!("pair_refresh_execution_v3:authority:{id}");
        let authority: serde_json::Value =
            serde_json::from_str(&f.db.get_state(&authority_key).unwrap().unwrap()).unwrap();
        let native = authority["evidence"]["resources"]["workspace"].get("native_owner");
        #[cfg(target_os = "linux")]
        assert!(
            native.is_some(),
            "Linux restart qualification requires actual native identity"
        );
        let lock = std::sync::Arc::new(crate::data_dir::acquire(f.temp.path()).unwrap());
        let service =
            RefreshService::with_fixture_recovery_client(engine.clone(), lock, f.client.clone())
                .unwrap();
        let before = journal_state(&f);
        let token = "disposable-workspace-session";
        let result = service
            .prepare_recovery(token.into(), "child".into(), "native-worker".into())
            .await;
        if native.is_none() {
            assert!(result.is_err());
            assert_eq!(journal_state(&f), before);
            assert_eq!(f.db.pair_refresh_operation(id).unwrap(), op);
            assert!(
                crate::db::pair_refresh_operations::require_no_pair_refresh_hold(
                    &f.db.conn(),
                    None
                )
                .is_err()
            );
            continue;
        }
        let approval = match result.unwrap() {
            RefreshResponse::Approval(value) => value,
            other => panic!("unexpected {other:?}"),
        };
        assert_eq!(approval.kind, "recovery");
        assert_eq!(approval.operation_id, id);
        let before = journal_after_native_lease_claim(
            &f,
            &before,
            op.admitted_writer_epoch,
            std::process::id(),
        );
        assert_eq!(journal_state(&f), before);
        assert_eq!(f.db.pair_refresh_operation(id).unwrap(), op);
        assert!(service
            .act(
                token.into(),
                "child",
                "native-worker",
                approval.preparation_id,
                "wrong".into(),
                RefreshAction::Execute
            )
            .await
            .is_err());
        assert_eq!(journal_state(&f), before);
        if stage != "prepared" {
            assert!(service
                .act(
                    token.into(),
                    "child",
                    "native-worker",
                    approval.preparation_id,
                    approval.approval_digest.clone(),
                    RefreshAction::Execute
                )
                .await
                .is_err());
            assert_eq!(f.db.pair_refresh_operation(id).unwrap().phase, op.phase);
            let observed = service
                .act(
                    token.into(),
                    "child",
                    "native-worker",
                    approval.preparation_id,
                    approval.approval_digest.clone(),
                    RefreshAction::Reconcile,
                )
                .await
                .unwrap();
            assert_eq!(
                observed.phase,
                if stage == "svn_intent" {
                    "svn_observed"
                } else {
                    "git_observed"
                }
            );
        }
        let completed = service
            .act(
                token.into(),
                "child",
                "native-worker",
                approval.preparation_id,
                approval.approval_digest.clone(),
                RefreshAction::Execute,
            )
            .await
            .unwrap();
        assert_eq!(completed.phase, "completed");
        assert_eq!(completed.operation_id, id);
        assert_eq!(completed.svn_revision, Some(5));
        assert_eq!(
            completed.git_sha.as_deref(),
            Some(op.pins.git.prepared_sha.as_str())
        );
        let repeated = service
            .act(
                token.into(),
                "child",
                "native-worker",
                approval.preparation_id,
                approval.approval_digest,
                RefreshAction::Execute,
            )
            .await
            .unwrap();
        assert_eq!(repeated.operation_id, id);
        assert_eq!(repeated.phase, "completed");
        assert_eq!(parent_row(&f), parent_before);
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
                .target()
                .unwrap()
                .to_string(),
            op.pins.git.prepared_sha
        );
        assert_eq!(
            std::fs::read_to_string(&f.log)
                .unwrap()
                .lines()
                .filter(|line| line.contains("/git-receive-pack"))
                .count(),
            1
        );
        assert!(
            crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&f.db.conn(), None)
                .is_ok()
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while crate::busy::try_acquire_pair("child", "parent").is_none() {
            assert!(std::time::Instant::now() < deadline);
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owned_service_single_flights_requests_and_binds_terminal_receipts_to_approval() {
    use crate::pair_refresh::service::{RefreshAction, RefreshResponse, RefreshService};
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = std::sync::Arc::new(f.engine());
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let service = RefreshService::with_fixture_client(engine, f.client.clone());
    let token = "disposable-workspace-session";
    let (first, second) = tokio::join!(
        service.prepare(token.into(), "child".into(), "single-flight".into()),
        service.prepare(token.into(), "child".into(), "single-flight".into())
    );
    let approval = match (first, second) {
        (Ok(RefreshResponse::Approval(approval)), Err(error))
        | (Err(error), Ok(RefreshResponse::Approval(approval))) => {
            assert!(error.to_string().contains("already has a live preparation"));
            approval
        }
        other => panic!("duplicate request did not have one owner: {other:?}"),
    };
    assert!(service
        .prepare(token.into(), "child".into(), "single-flight".into())
        .await
        .is_err());
    let completed = service
        .act(
            token.into(),
            "child",
            "single-flight",
            approval.preparation_id,
            approval.approval_digest.clone(),
            RefreshAction::Execute,
        )
        .await
        .unwrap();
    assert_eq!(completed.operation_id, approval.operation_id);
    assert_eq!(completed.phase, "completed");
    let saved = journal_state(&f);
    for handle in [approval.preparation_id, Uuid::new_v4()] {
        assert!(service
            .act(
                token.into(),
                "child",
                "single-flight",
                handle,
                "other-approval".into(),
                RefreshAction::Execute
            )
            .await
            .is_err());
    }
    assert_eq!(journal_state(&f), saved);
    let again = service
        .act(
            token.into(),
            "child",
            "single-flight",
            approval.preparation_id,
            approval.approval_digest,
            RefreshAction::Execute,
        )
        .await
        .unwrap();
    assert_eq!(again.operation_id, completed.operation_id);
    let noop = match service
        .prepare(token.into(), "child".into(), "single-noop".into())
        .await
        .unwrap()
    {
        RefreshResponse::Approval(value) => value,
        other => panic!("unexpected {other:?}"),
    };
    assert_eq!(noop.kind, "no_op");
    assert!(service
        .act(
            token.into(),
            "child",
            "single-noop",
            noop.preparation_id,
            noop.approval_digest.clone(),
            RefreshAction::Reconcile
        )
        .await
        .is_err());
    assert!(crate::busy::try_acquire_pair("child", "parent").is_some());
    assert!(service
        .receipt(token, "child", "single-noop")
        .unwrap()
        .is_none());
    assert_eq!(journal_state(&f), saved);
    let no_op = service
        .act(
            token.into(),
            "child",
            "single-noop",
            noop.preparation_id,
            noop.approval_digest,
            RefreshAction::Execute,
        )
        .await
        .unwrap();
    assert_eq!(no_op.phase, "no_op");
    assert!(service
        .act(
            token.into(),
            "child",
            "single-noop",
            Uuid::new_v4(),
            "other-noop-approval".into(),
            RefreshAction::Execute
        )
        .await
        .is_err());
    assert_eq!(
        std::fs::read_to_string(&f.log)
            .unwrap()
            .lines()
            .filter(|line| line.contains("/git-receive-pack"))
            .count(),
        1
    );
    assert_eq!(
        crate::svn::SvnClient::new(format!("{}/branches/f", f.root), "", "")
            .info()
            .await
            .unwrap()
            .latest_rev,
        5
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owned_service_releases_busy_after_revoked_credentials_refuse_before_admission() {
    use crate::pair_refresh::service::{RefreshAction, RefreshResponse, RefreshService};
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = std::sync::Arc::new(f.engine());
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let service = RefreshService::with_fixture_client(engine, f.client.clone());
    let token = "disposable-workspace-session";
    let approval = match service
        .prepare(token.into(), "child".into(), "revoked-service".into())
        .await
        .unwrap()
    {
        RefreshResponse::Approval(value) => value,
        other => panic!("unexpected {other:?}"),
    };
    f.db.set_state("secret_git_token_child", "").unwrap();
    let saved = journal_state(&f);
    assert!(service
        .act(
            token.into(),
            "child",
            "revoked-service",
            approval.preparation_id,
            approval.approval_digest,
            RefreshAction::Execute
        )
        .await
        .is_err());
    assert!(crate::busy::try_acquire_pair("child", "parent").is_some());
    assert_eq!(journal_state(&f), saved);
    assert!(service
        .receipt(token, "child", "revoked-service")
        .unwrap()
        .is_none());
    assert!(f.db.pair_refresh_operation(approval.operation_id).is_err());
    assert!(
        crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&f.db.conn(), None)
            .is_ok()
    );
    assert_eq!(
        f.target
            .find_reference("refs/heads/feature")
            .unwrap()
            .target(),
        Some(f.baseline)
    );
    assert!(!std::fs::read_to_string(&f.log)
        .unwrap()
        .contains("receive-pack"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recovery_server_fence_lifetime_preserves_held_epoch_and_cleans_only_terminal_state() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = f.engine();
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let fence = crate::writer_fence::claim(engine.db(), f.temp.path())
        .unwrap()
        .into_recovery_lifetime();
    let (actor, _workspace, prepared) = prepared(&f, &engine).await;
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    let digest = actor.prepared_approval_digest(&engine, &prepared).unwrap();
    let op = actor
        .enroll_prepared(&engine, &prepared, "lifetime-request", &digest, &busy)
        .unwrap();
    let saved = journal_state(&f);
    let lease = f.db.get_state("writer_fence_v1:lease").unwrap();
    let native = f.db.get_state("writer_fence_v1:native").unwrap();
    assert!(fence.release_if_quiet(engine.db()).is_err());
    fence.require_current(engine.db()).unwrap();
    assert_eq!(journal_state(&f), saved);
    assert_eq!(f.db.get_state("writer_fence_v1:lease").unwrap(), lease);
    assert_eq!(f.db.get_state("writer_fence_v1:native").unwrap(), native);
    assert_eq!(f.db.pair_refresh_operation(op.id).unwrap(), op);
    actor
        .cancel_prepared_enrollment(&engine, &prepared, &busy)
        .unwrap();
    let terminal = journal_state(&f)
        .into_iter()
        .filter(|(key, _, _)| key != "writer_fence_v1:native")
        .collect::<Vec<_>>();
    fence.release_if_quiet(engine.db()).unwrap();
    assert!(f.db.get_state("writer_fence_v1:lease").unwrap().is_none());
    assert!(f.db.get_state("writer_fence_v1:native").unwrap().is_none());
    assert_eq!(journal_state(&f), terminal);
    assert!(fence.require_current(engine.db()).is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owned_service_shutdown_joins_an_authorized_publication_before_returning() {
    use crate::pair_refresh::service::{RefreshAction, RefreshResponse, RefreshService};
    use std::os::unix::fs::PermissionsExt;
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = std::sync::Arc::new(f.engine());
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let service = RefreshService::with_fixture_client(engine, f.client.clone());
    let token = "disposable-workspace-session";
    let approval = match service
        .prepare(token.into(), "child".into(), "shutdown-request".into())
        .await
        .unwrap()
    {
        RefreshResponse::Approval(value) => value,
        other => panic!("unexpected {other:?}"),
    };
    let hook = f.temp.path().join("svn-repository/hooks/pre-commit");
    std::fs::write(
        &hook,
        r#"#!/usr/bin/env python3
from pathlib import Path
import time
root=Path(__file__).resolve().parents[2]
(root/'shutdown-ready').write_text('ready')
for _ in range(600):
    if (root/'shutdown-release').exists():
        raise SystemExit(0)
    time.sleep(0.05)
raise SystemExit(1)
"#,
    )
    .unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    let _svn_diag = SvnDiag::arm(f.temp.path());
    let request_service = service.clone();
    let handle = approval.preparation_id;
    let digest = approval.approval_digest;
    let diagnostic_digest = digest.clone();
    let mut request = tokio::spawn(async move {
        request_service
            .act(
                token.into(),
                "child",
                "shutdown-request",
                handle,
                digest,
                RefreshAction::Execute,
            )
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(45), async {
        while !f.temp.path().join("shutdown-ready").is_file() {
            assert!(
                !request.is_finished(),
                "request ended before entering actual shutdown SVN publication: {}",
                {
                    let detail = match (&mut request).await {
                        Ok(Err(error)) => format!("{error:#}"),
                        Ok(Ok(_)) => "success-before-barrier".to_owned(),
                        Err(error) => format!("request join failed: {error}"),
                    };
                    let mut detail = crate::errors::redact_vcs_error_detail(&detail);
                    for secret in [
                        token,
                        "disposable-preparation-token",
                        diagnostic_digest.as_str(),
                    ] {
                        detail = detail.replace(secret, "[REDACTED_FIXTURE_SECRET]");
                    }
                    detail.push(' ');
                    detail.push_str(&case_diag(
                        f.temp.path(),
                        &hook,
                        &f.temp.path().join("shutdown-ready"),
                    ));
                    detail
                }
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        f.db.pair_refresh_operation(approval.operation_id)
            .unwrap()
            .phase,
        crate::db::pair_refresh_operations::PairRefreshPhase::SvnIntent
    );
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());
    let shutdown_service = service.clone();
    let (started, ready) = tokio::sync::oneshot::channel();
    let shutdown = tokio::spawn(async move {
        started.send(()).unwrap();
        shutdown_service.shutdown().await;
    });
    ready.await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    assert!(!shutdown.is_finished());
    assert!(service
        .prepare(token.into(), "child".into(), "during-shutdown".into())
        .await
        .is_err());
    std::fs::write(f.temp.path().join("shutdown-release"), b"release").unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(45), shutdown)
        .await
        .unwrap()
        .unwrap();
    let receipt = service
        .receipt(token, "child", "shutdown-request")
        .unwrap()
        .unwrap();
    assert_eq!(receipt.operation_id, approval.operation_id);
    assert_eq!(receipt.phase, "completed");
    assert_eq!(receipt.svn_revision, Some(5));
    assert_eq!(
        std::fs::read_to_string(&f.log)
            .unwrap()
            .lines()
            .filter(|line| line.contains("/git-receive-pack"))
            .count(),
        1
    );
    assert!(crate::busy::try_acquire_pair("child", "parent").is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ordinary_startup_and_unproved_claim_preserve_a_valid_authenticated_hold() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = f.engine();
    let lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let (actor, _workspace, prepared) = prepared(&f, &engine).await;
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    let digest = actor.prepared_approval_digest(&engine, &prepared).unwrap();
    let op = actor
        .enroll_prepared(&engine, &prepared, "held-startup", &digest, &busy)
        .unwrap();
    drop(fence);
    drop(lock);
    let before = f.state();
    let path = f.temp.path().join("reposync.db");
    let wal = path.with_extension("db-wal");
    let bytes = std::fs::read(&path).unwrap();
    let wal_bytes = std::fs::read(&wal).unwrap();
    assert!(Database::open_with_exclusive_owner(f.temp.path()).is_err());
    assert_eq!(f.state(), before);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(std::fs::read(&wal).unwrap(), wal_bytes);
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    assert!(crate::writer_fence::claim(engine.db(), f.temp.path()).is_err());
    assert_eq!(f.state(), before);
    assert_eq!(f.db.pair_refresh_operation(op.id).unwrap(), op);
    assert!(
        crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&f.db.conn(), None)
            .is_err()
    );
}

// Only successful native claims use this expected-state builder. Ordinary
// journal snapshots keep native metadata visible, including renewal invariance.
fn journal_after_native_lease_claim(
    f: &Fixture,
    before: &[(String, String, String)],
    previous_epoch: u64,
    expected_pid: u32,
) -> Vec<(String, String, String)> {
    let lease: serde_json::Value =
        serde_json::from_str(&f.db.get_state("writer_fence_v1:lease").unwrap().unwrap()).unwrap();
    let native_row = journal_state(f)
        .into_iter()
        .find(|(key, _, _)| key == "writer_fence_v1:native")
        .unwrap();
    let native: serde_json::Value = serde_json::from_str(&native_row.1).unwrap();
    for field in [
        "version",
        "epoch",
        "holder_pid",
        "holder_host",
        "acquired_at",
    ] {
        assert_eq!(native["lease"][field], lease[field], "{field}");
    }
    assert!(lease["epoch"].as_u64().unwrap() > previous_epoch);
    assert_eq!(lease["holder_pid"].as_u64(), Some(u64::from(expected_pid)));
    let owner: crate::pair_refresh::native_owner::NativeProcessOwner =
        serde_json::from_value(native["owner"].clone()).unwrap();
    owner.validate().unwrap();
    assert_eq!(owner.pid(), expected_pid);
    assert!(owner
        .same_domain(&crate::pair_refresh::native_owner::NativeProcessOwner::capture().unwrap()));
    let mut expected = before
        .iter()
        .filter(|(key, _, _)| key != "writer_fence_v1:native")
        .cloned()
        .collect::<Vec<_>>();
    expected.push(native_row);
    expected.sort();
    expected
}
