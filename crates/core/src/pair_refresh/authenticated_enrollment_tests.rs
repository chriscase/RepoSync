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

// M2a is storage-only after setup. Reuse the existing authenticated fixture and
// real production completion path; do not invent signed rows or new providers.
// Existing SCM/TLS fixture effects occur in setup, never in the capture API.
mod changed_child_provenance_review {
    use super::*;
    use crate::db::pair_refresh_operations::{PairRefreshOperation, PairRefreshPhase};
    use crate::pair_refresh::execution_configuration::LiveExecutionConfiguration;
    use std::collections::BTreeMap;

    type LogicalSnapshot = BTreeMap<String, Vec<Vec<rusqlite::types::Value>>>;

    fn storage(db: &Database) -> LogicalSnapshot {
        use rusqlite::types::Value;
        let conn = db.conn();
        let mut result = BTreeMap::new();
        let schema = conn
            .prepare("SELECT type,name,tbl_name,rootpage,sql FROM sqlite_master ORDER BY type,name")
            .unwrap()
            .query_map([], |row| {
                (0..5)
                    .map(|i| row.get::<_, Value>(i))
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        result.insert("schema".into(), schema);
        let tables = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        for table in tables {
            let sql = format!("SELECT * FROM \"{}\"", table.replace('"', "\"\""));
            let mut query = conn.prepare(&sql).unwrap();
            let columns = query.column_count();
            let mut rows = query
                .query_map([], |row| {
                    (0..columns)
                        .map(|i| row.get::<_, Value>(i))
                        .collect::<rusqlite::Result<Vec<_>>>()
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            rows.sort_by_cached_key(|row| format!("{row:?}"));
            result.insert(format!("table:{table}"), rows);
        }
        result
    }

    async fn completed(
        f: &Fixture,
        engine: &crate::sync_engine::SyncEngine,
    ) -> PairRefreshOperation {
        let (actor, _workspace, value) = prepared(f, engine).await;
        let approved = actor.prepared_approval_digest(engine, &value).unwrap();
        let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
        let op = super::super::super::execution::complete_owned_refresh_with_client(
            engine,
            &actor,
            &value,
            "m2a-authenticated-history",
            &approved,
            &busy,
            f.client.clone(),
        )
        .await
        .unwrap();
        assert_eq!(op.version, 3);
        assert_eq!(op.phase, PairRefreshPhase::Completed);
        assert_eq!(f.db.pair_refresh_operation(op.id).unwrap(), op);
        op
    }

    fn request_log(f: &Fixture) -> Option<Vec<u8>> {
        match std::fs::read(&f.log) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => panic!("request-log observation failed: {error}"),
        }
    }

    #[track_caller]
    fn refuse_without_repair(
        f: &Fixture,
        engine: &crate::sync_engine::SyncEngine,
        expected: &LiveExecutionConfiguration,
        id: Uuid,
        subcase: &str,
    ) {
        let before = storage(&f.db);
        assert_eq!(storage(engine.db()), before);
        let requests = request_log(f);
        assert!(
            f.db.capture_changed_child_provenance_review(expected, id)
                .is_err(),
            "review capture accepted negative subcase {subcase} at {}",
            std::panic::Location::caller()
        );
        assert_eq!(storage(&f.db), before);
        assert_eq!(storage(engine.db()), before);
        assert_eq!(request_log(f), requests);
    }

    #[track_caller]
    fn kv_fault(
        f: &Fixture,
        engine: &crate::sync_engine::SyncEngine,
        expected: &LiveExecutionConfiguration,
        id: Uuid,
        key: &str,
        value: Option<&str>,
    ) {
        use rusqlite::OptionalExtension;
        let prior: Option<(String, String)> =
            f.db.conn()
                .query_row(
                    "SELECT value,updated_at FROM kv_state WHERE key=?1",
                    [key],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .unwrap();
        let original = storage(&f.db);
        match value {
            Some(value) => f.db.set_state(key, value).unwrap(),
            None => {
                f.db.conn()
                    .execute("DELETE FROM kv_state WHERE key=?1", [key])
                    .unwrap();
            }
        }
        refuse_without_repair(f, engine, expected, id, key);
        match prior {
            Some((value, updated)) => {
                f.db.conn().execute("INSERT INTO kv_state(key,value,updated_at) VALUES(?1,?2,?3) ON CONFLICT(key) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at", rusqlite::params![key,value,updated]).unwrap();
            }
            None => {
                f.db.conn()
                    .execute("DELETE FROM kv_state WHERE key=?1", [key])
                    .unwrap();
            }
        }
        assert_eq!(storage(&f.db), original);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn signed_history_and_changed_current_checkpoints_remain_distinct_review_only() {
        let _serial = SERIAL.lock().await;
        let f = Fixture::new();
        let engine = f.engine();
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
        let op = completed(&f, &engine).await;
        drop(fence); // Stop independent lease heartbeat before full storage comparisons.
        let original_configuration =
            f.db.capture_pair_refresh_execution_configuration("child")
                .unwrap();
        let doc_key = format!("pair_refresh_execution_v3:document:{}", op.id);
        let original_document = f.db.get_state(&doc_key).unwrap().unwrap();
        let before = storage(&f.db);
        let original =
            f.db.capture_changed_child_provenance_review(&original_configuration, op.id)
                .unwrap();
        assert_eq!(original.historical_operation(), &op);
        assert_eq!(original.historical_document(), original_document);
        {
            let integrated = f
                .target
                .find_commit(Oid::from_str(original.historical_child_anchor().0).unwrap())
                .unwrap();
            assert_eq!(integrated.parent_count(), 2);
            assert_eq!(
                integrated.parent_id(1).unwrap().to_string(),
                original.historical_parent_anchor().0
            );
        }
        assert_eq!(storage(&f.db), before);
        assert_eq!(storage(engine.db()), before);

        // These are deliberately *unproven* current registration values. Real
        // Git commits model R->C and P1->P2; no claim of SVN/current mapping proof
        // is made, and no remote ref, SVN revision or completed receipt is edited.
        let child = commit(
            &f.target,
            b"child after integration\n",
            &[Oid::from_str(&op.pins.git.prepared_sha).unwrap()],
        );
        let parent = commit(&f.target, b"parent after integration\n", &[f.parent]);
        let child_revision = op.svn_receipt.as_ref().unwrap().revision + 2;
        let parent_revision = child_revision + 1;
        for (registration, sha, revision) in [
            (&original_configuration.pin().child, child, child_revision),
            (
                &original_configuration.pin().parent,
                parent,
                parent_revision,
            ),
        ] {
            f.db.conn()
                .execute(
                    "UPDATE repositories SET last_git_sha=?1,last_svn_rev=?2 WHERE id=?3",
                    rusqlite::params![sha.to_string(), revision, registration.repo_id],
                )
                .unwrap();
            f.db.set_state(
                &format!("last_git_sha_{}", registration.scope_uuid),
                &sha.to_string(),
            )
            .unwrap();
            f.db.set_state(
                &format!("last_svn_rev_{}", registration.repo_id),
                &revision.to_string(),
            )
            .unwrap();
        }
        refuse_without_repair(
            &f,
            &engine,
            &original_configuration,
            op.id,
            "stale-checkpoints",
        );
        let current =
            f.db.capture_pair_refresh_execution_configuration("child")
                .unwrap();
        let before = storage(&f.db);
        let requests = std::fs::read(&f.log).unwrap();
        let review =
            f.db.capture_changed_child_provenance_review(&current, op.id)
                .unwrap();
        assert_eq!(
            review.historical_parent_anchor(),
            (
                op.pins.parent.last_git_sha.as_str(),
                op.pins.parent.last_svn_rev
            )
        );
        assert_eq!(
            review.historical_child_anchor(),
            (
                op.pins.git.prepared_sha.as_str(),
                op.svn_receipt.as_ref().unwrap().revision
            )
        );
        assert_ne!(
            review.historical_parent_anchor().0,
            review.historical_child_anchor().0
        );
        assert_eq!(review.current_configuration(), current.pin());
        assert_eq!(
            review.current_configuration().child.last_git_sha,
            child.to_string()
        );
        assert_eq!(
            review.current_configuration().parent.last_git_sha,
            parent.to_string()
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
            f.target.find_reference("refs/heads/main").unwrap().target(),
            Some(f.parent)
        );
        assert_eq!(review.historical_operation(), &op);
        assert_eq!(review.historical_document(), original_document);
        assert_eq!(
            review.historical_authority_digest(),
            original.historical_authority_digest()
        );
        assert_ne!(review.snapshot_digest(), original.snapshot_digest());
        assert!(!review.execution_eligible());
        for gate in [
            "historical_baseline_applicability",
            "current_mapping_provenance",
            "live_git_graphs_and_remote_freshness",
        ] {
            assert!(review.unverified_gates().contains(&gate));
        }
        assert!(f
            .db
            .capture_completed_pair_refresh_baseline(&current)
            .is_err());
        let repeated =
            f.db.capture_changed_child_provenance_review(&current, op.id)
                .unwrap();
        assert_eq!(review.snapshot_digest(), repeated.snapshot_digest());
        assert_eq!(storage(&f.db), before);
        assert_eq!(storage(engine.db()), before);
        assert_eq!(std::fs::read(&f.log).unwrap(), requests);
        assert_eq!(
            f.db.get_state(&doc_key).unwrap().unwrap(),
            original_document
        );

        // Historical signing authority is independent of a valid current
        // credential rotation. No HTTP request is sent with either token here.
        f.db.set_state("secret_git_token_child", "disposable-m2a-rotated-token")
            .unwrap();
        refuse_without_repair(&f, &engine, &current, op.id, "stale-credentials");
        let rotated =
            f.db.capture_pair_refresh_execution_configuration("child")
                .unwrap();
        let before = storage(&f.db);
        let rotated_review =
            f.db.capture_changed_child_provenance_review(&rotated, op.id)
                .unwrap();
        assert_ne!(
            rotated_review
                .current_configuration()
                .credential_context_digest,
            op.pins.credential_context_digest
        );
        assert_eq!(rotated_review.historical_operation(), &op);
        assert_eq!(
            rotated_review.historical_authority_digest(),
            original.historical_authority_digest()
        );
        assert_eq!(rotated_review.historical_document(), original_document);
        assert_ne!(rotated_review.snapshot_digest(), review.snapshot_digest());
        assert!(!rotated_review.execution_eligible());
        assert_eq!(storage(&f.db), before);
        assert_eq!(storage(engine.db()), before);
        assert_eq!(std::fs::read(&f.log).unwrap(), requests);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn corrupt_stale_revoked_and_foreign_review_sources_refuse_without_repair() {
        let _serial = SERIAL.lock().await;
        let f = Fixture::new();
        let engine = f.engine();
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
        let op = completed(&f, &engine).await;
        drop(fence);
        let expected =
            f.db.capture_pair_refresh_execution_configuration("child")
                .unwrap();
        let doc_key = format!("pair_refresh_execution_v3:document:{}", op.id);
        let auth_key = format!("pair_refresh_execution_v3:authority:{}", op.id);
        let document = f.db.get_state(&doc_key).unwrap().unwrap();
        let authority = f.db.get_state(&auth_key).unwrap().unwrap();
        refuse_without_repair(&f, &engine, &expected, Uuid::nil(), "nil-selector");
        refuse_without_repair(&f, &engine, &expected, Uuid::new_v4(), "missing-selector");
        for (key, value) in [
            (&auth_key, None),
            (&auth_key, Some("{}")),
            (&doc_key, None),
            (&doc_key, Some("{}")),
        ] {
            kv_fault(&f, &engine, &expected, op.id, key, value);
        }
        let auth: serde_json::Value = serde_json::from_str(&authority).unwrap();
        let mac = auth["mac"].as_str().unwrap();
        let tampered = authority.replacen(mac, &"0".repeat(64), 1);
        kv_fault(&f, &engine, &expected, op.id, &auth_key, Some(&tampered));
        let mut missing_receipt: serde_json::Value = serde_json::from_str(&document).unwrap();
        missing_receipt["svn_receipt"] = serde_json::Value::Null;
        kv_fault(
            &f,
            &engine,
            &expected,
            op.id,
            &doc_key,
            Some(&missing_receipt.to_string()),
        );
        kv_fault(
            &f,
            &engine,
            &expected,
            op.id,
            &format!("last_git_sha_{}", expected.pin().child.scope_uuid),
            Some(&"f".repeat(40)),
        );
        kv_fault(
            &f,
            &engine,
            &expected,
            op.id,
            &format!("last_svn_rev_{}", expected.pin().parent.repo_id),
            None,
        );
        kv_fault(
            &f,
            &engine,
            &expected,
            op.id,
            "secret_git_token_child",
            Some(""),
        );
        kv_fault(
            &f,
            &engine,
            &expected,
            op.id,
            "import_operation_v1:active:child",
            Some("missing-operation"),
        );
        kv_fault(
            &f,
            &engine,
            &expected,
            op.id,
            &crate::history_inspect::history_block_key(Some("child")),
            Some("{}"),
        );
        let alias = format!("pair_refresh_execution_v3:document:{}", Uuid::new_v4());
        kv_fault(&f, &engine, &expected, op.id, &alias, Some(&document));

        let secret: Vec<u8> =
            f.db.conn()
                .query_row(
                    "SELECT secret FROM pair_refresh_journal_key_v3 WHERE id=1",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
        f.db.conn()
            .execute("DELETE FROM pair_refresh_journal_key_v3 WHERE id=1", [])
            .unwrap();
        refuse_without_repair(&f, &engine, &expected, op.id, "missing-key");
        let count: i64 =
            f.db.conn()
                .query_row(
                    "SELECT count(*) FROM pair_refresh_journal_key_v3",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
        assert_eq!(count, 0);
        f.db.conn()
            .execute(
                "INSERT INTO pair_refresh_journal_key_v3(id,secret) VALUES(1,?1)",
                [secret],
            )
            .unwrap();
        f.db.conn().execute_batch("CREATE TRIGGER m2a_unknown_key_trigger AFTER UPDATE ON pair_refresh_journal_key_v3 BEGIN SELECT 1; END;").unwrap();
        refuse_without_repair(&f, &engine, &expected, op.id, "key-schema-trigger");
        f.db.conn()
            .execute_batch("DROP TRIGGER m2a_unknown_key_trigger;")
            .unwrap();

        // Coherent but different current incarnation/configuration cannot reuse
        // the historical authenticated operation. No old document is re-signed.
        let old_scope = expected.pin().child.scope_uuid.clone();
        let new_scope = Uuid::new_v4().to_string();
        let new_key = format!("last_git_sha_{new_scope}");
        f.db.set_state(&new_key, &expected.pin().child.last_git_sha)
            .unwrap();
        f.db.conn()
            .execute(
                "UPDATE repositories SET scope_uuid=?1 WHERE id='child'",
                [&new_scope],
            )
            .unwrap();
        let recreated =
            f.db.capture_pair_refresh_execution_configuration("child")
                .unwrap();
        refuse_without_repair(&f, &engine, &recreated, op.id, "new-incarnation");
        f.db.conn()
            .execute(
                "UPDATE repositories SET scope_uuid=?1 WHERE id='child'",
                [&old_scope],
            )
            .unwrap();
        f.db.conn()
            .execute("DELETE FROM kv_state WHERE key=?1", [&new_key])
            .unwrap();
        f.db.conn()
            .execute(
                "UPDATE repositories SET git_branch='feature-other' WHERE id='child'",
                [],
            )
            .unwrap();
        let changed =
            f.db.capture_pair_refresh_execution_configuration("child")
                .unwrap();
        refuse_without_repair(&f, &engine, &changed, op.id, "changed-branch");
        f.db.conn()
            .execute(
                "UPDATE repositories SET git_branch='feature' WHERE id='child'",
                [],
            )
            .unwrap();
        let echo_key = format!("repo_echo_generation_{}", expected.pin().child.scope_uuid);
        let echo = f.db.get_state(&echo_key).unwrap();
        f.db.set_state(
            &echo_key,
            &(expected.pin().child.echo_generation + 1).to_string(),
        )
        .unwrap();
        let changed =
            f.db.capture_pair_refresh_execution_configuration("child")
                .unwrap();
        refuse_without_repair(&f, &engine, &changed, op.id, "changed-echo");
        match echo {
            Some(value) => f.db.set_state(&echo_key, &value).unwrap(),
            None => {
                f.db.conn()
                    .execute("DELETE FROM kv_state WHERE key=?1", [&echo_key])
                    .unwrap();
            }
        }

        let fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
        let window = f.db.begin_ordinary_writer_window(Some("child")).unwrap();
        window.require_engine_scope(&f.db, Some("child")).unwrap();
        refuse_without_repair(&f, &engine, &expected, op.id, "matching-writer-window");
        window.finish_preparation().unwrap();
        drop(fence);

        // Existing copied-storage regression mechanism, no new OS adapter.
        let copy_dir = tempfile::tempdir().unwrap();
        let copy_path = copy_dir.path().join("foreign.db");
        f.db.conn()
            .execute("VACUUM INTO ?1", [copy_path.to_str().unwrap()])
            .unwrap();
        let foreign = Database::new(copy_path).unwrap();
        let before = storage(&foreign);
        assert!(foreign
            .capture_changed_child_provenance_review(&expected, op.id)
            .is_err());
        assert_eq!(storage(&foreign), before);
        assert_eq!(f.db.pair_refresh_operation(op.id).unwrap(), op);
        assert_eq!(f.db.get_state(&doc_key).unwrap().unwrap(), document);
        assert_eq!(f.db.get_state(&auth_key).unwrap().unwrap(), authority);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn missing_or_held_history_never_establishes_review_authority() {
        let _serial = SERIAL.lock().await;
        let f = Fixture::new();
        let engine = f.engine();
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let expected =
            f.db.capture_pair_refresh_execution_configuration("child")
                .unwrap();
        assert_eq!(request_log(&f), None);
        refuse_without_repair(&f, &engine, &expected, Uuid::new_v4(), "no-history");
        assert!(!key_table(&f));
        let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
        let (actor, _workspace, prepared) = prepared(&f, &engine).await;
        let approved = actor.prepared_approval_digest(&engine, &prepared).unwrap();
        let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
        let held = actor
            .enroll_prepared(&engine, &prepared, "m2a-held", &approved, &busy)
            .unwrap();
        let before = journal_state(&f); // Existing independent writer heartbeat excluded.
        assert!(f
            .db
            .capture_changed_child_provenance_review(&expected, held.id)
            .is_err());
        assert_eq!(journal_state(&f), before);
        assert_eq!(f.db.pair_refresh_operation(held.id).unwrap(), held);
        assert!(key_table(&f));
    }
}

mod changed_child_preview_binding {
    use super::*;
    use crate::db::pair_refresh_operations::PairRefreshOperation;
    use std::collections::BTreeMap;

    type Storage = BTreeMap<String, Vec<Vec<rusqlite::types::Value>>>;

    fn storage(db: &Database) -> Storage {
        use rusqlite::types::Value;
        let conn = db.conn();
        let mut snapshot = BTreeMap::new();
        let schema = conn
            .prepare("SELECT type,name,tbl_name,rootpage,sql FROM sqlite_master ORDER BY type,name")
            .unwrap()
            .query_map([], |row| {
                (0..5)
                    .map(|i| row.get::<_, Value>(i))
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        snapshot.insert("schema".into(), schema);
        let tables = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        for table in tables {
            let sql = format!("SELECT * FROM \"{}\"", table.replace('"', "\"\""));
            let mut query = conn.prepare(&sql).unwrap();
            let columns = query.column_count();
            let mut values = query
                .query_map([], |row| {
                    (0..columns)
                        .map(|i| row.get::<_, Value>(i))
                        .collect::<rusqlite::Result<Vec<_>>>()
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            values.sort_by_cached_key(|row| format!("{row:?}"));
            snapshot.insert(table, values);
        }
        snapshot
    }

    fn files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        fn collect(root: &Path, path: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
            for entry in std::fs::read_dir(path).unwrap() {
                let entry = entry.unwrap();
                let kind = entry.file_type().unwrap();
                assert!(!kind.is_symlink());
                if kind.is_dir() {
                    collect(root, &entry.path(), out);
                } else {
                    assert!(kind.is_file());
                    out.insert(
                        entry.path().strip_prefix(root).unwrap().to_owned(),
                        std::fs::read(entry.path()).unwrap(),
                    );
                }
            }
        }
        let mut out = BTreeMap::new();
        collect(root, root, &mut out);
        out
    }

    async fn completed(
        f: &Fixture,
        engine: &crate::sync_engine::SyncEngine,
    ) -> PairRefreshOperation {
        let (actor, _workspace, value) = prepared(f, engine).await;
        let digest = actor.prepared_approval_digest(engine, &value).unwrap();
        let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
        super::super::super::execution::complete_owned_refresh_with_client(
            engine,
            &actor,
            &value,
            "m2b-signed-history",
            &digest,
            &busy,
            f.client.clone(),
        )
        .await
        .unwrap()
    }

    fn set_checkpoint(f: &Fixture, id: &str, sha: Oid, revision: i64) {
        let scope: String =
            f.db.conn()
                .query_row(
                    "SELECT scope_uuid FROM repositories WHERE id=?1",
                    [id],
                    |r| r.get(0),
                )
                .unwrap();
        f.db.conn()
            .execute(
                "UPDATE repositories SET last_git_sha=?1,last_svn_rev=?2 WHERE id=?3",
                rusqlite::params![sha.to_string(), revision, id],
            )
            .unwrap();
        f.db.set_state(&format!("last_git_sha_{scope}"), &sha.to_string())
            .unwrap();
        f.db.set_state(&format!("last_svn_rev_{id}"), &revision.to_string())
            .unwrap();
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

    // Real disposable source advances, not signed-row fabrication or a claim
    // that current mapping rows were authenticated by these fixture updates.
    fn advance(f: &Fixture, op: &PairRefreshOperation) {
        let child_wc = f.temp.path().join("m2b-child-wc");
        svn(
            &[
                "checkout",
                &format!("{}/branches/f", f.root),
                child_wc.to_str().unwrap(),
            ],
            None,
        );
        std::fs::write(child_wc.join("file.txt"), b"child work\n").unwrap();
        svn(&["commit", "-m", "m2b child work"], Some(&child_wc));
        let child_revision = svn_revision(&format!("{}/branches/f", f.root));
        std::fs::write(f._parent_wc.join("file.txt"), b"parent work\n").unwrap();
        svn(&["commit", "-m", "m2b parent work"], Some(&f._parent_wc));
        let parent_revision = svn_revision(&format!("{}/trunk", f.root));
        let r = Oid::from_str(&op.pins.git.prepared_sha).unwrap();
        let p1 = Oid::from_str(&op.pins.parent.last_git_sha).unwrap();
        let c = commit(&f.target, b"child work\n", &[r]);
        let p2 = commit(&f.target, b"parent work\n", &[p1]);
        f.target
            .reference("refs/heads/feature", c, true, "disposable m2b fixture")
            .unwrap();
        f.target
            .reference("refs/heads/main", p2, true, "disposable m2b fixture")
            .unwrap();
        set_checkpoint(f, "child", c, child_revision);
        set_checkpoint(f, "parent", p2, parent_revision);
    }

    async fn ready(f: &Fixture, engine: &crate::sync_engine::SyncEngine) -> PairRefreshOperation {
        let fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
        let op = completed(f, engine).await;
        drop(fence);
        advance(f, &op);
        op
    }

    async fn capture(
        f: &Fixture,
        id: Uuid,
    ) -> anyhow::Result<super::super::LiveChangedChildPreviewContext> {
        super::super::capture_changed_child_preview_with_sources(
            &f.db,
            "child",
            id,
            Some(f.client.clone()),
            |_| Ok(()),
        )
        .await
    }

    fn saved_kv(f: &Fixture, key: &str) -> Option<(String, String)> {
        use rusqlite::OptionalExtension;
        f.db.conn()
            .query_row(
                "SELECT value,updated_at FROM kv_state WHERE key=?1",
                [key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .unwrap()
    }

    fn restore_kv(f: &Fixture, key: &str, value: Option<(String, String)>) {
        match value {
            Some((value, updated)) => {
                f.db.conn().execute("INSERT INTO kv_state(key,value,updated_at) VALUES(?1,?2,?3) ON CONFLICT(key) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at", rusqlite::params![key, value, updated]).unwrap();
            }
            None => {
                f.db.conn()
                    .execute("DELETE FROM kv_state WHERE key=?1", [key])
                    .unwrap();
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn distinct_registered_roles_are_stable_read_only_and_never_execution() {
        let _serial = SERIAL.lock().await;
        let f = Fixture::new();
        let engine = f.engine();
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let op = ready(&f, &engine).await;
        let before = storage(&f.db);
        let shared = files(f.target.path());
        let worktree = files(&f._parent_wc);
        let requests = std::fs::read(&f.log).unwrap();
        let first = capture(&f, op.id).await.unwrap();
        let second = capture(&f, op.id).await.unwrap();
        let p1 = first.historical_review().historical_parent_anchor().0;
        let r = first.historical_review().historical_child_anchor().0;
        let c = &first.git_sources().child_tip;
        let p2 = &first.git_sources().parent_tip;
        assert_ne!(p1, r);
        assert_ne!(r, c);
        assert_ne!(p1, p2);
        assert_ne!(c, p2);
        assert!(!first.parent_contains_integrated_child());
        assert_eq!(first.context_digest(), second.context_digest());
        assert_eq!(first.historical_review().historical_operation(), &op);
        assert_eq!(
            first.svn_lineage().child_copy.created_revision,
            op.svn_receipt.as_ref().unwrap().child_copy.created_revision
        );
        assert!(first
            .verified_gates()
            .contains(&"role_specific_git_ancestry"));
        assert!(first
            .unverified_gates()
            .contains(&"current_mapping_provenance"));
        assert!(first
            .unverified_gates()
            .contains(&"unique_applicable_historical_baseline"));
        assert!(!first.execution_eligible());
        let current_mappings: i64 = f
            .db
            .conn()
            .query_row(
                "SELECT count(*) FROM sync_records WHERE repo_id='child' AND git_sha=?1 AND svn_rev=?2 AND status='applied'",
                rusqlite::params![c, first.current_configuration().child.last_svn_rev],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(current_mappings, 0);
        let current =
            f.db.capture_pair_refresh_execution_configuration("child")
                .unwrap();
        assert!(f
            .db
            .capture_completed_pair_refresh_baseline(&current)
            .is_err());
        // The unchanged clean-child preparer must not accept C merely because
        // its signed ancestry reaches P1 through the integration merge R.
        assert!(crate::pair_refresh::preparation::prepare_clean_child_git(
            first.git.repository(),
            &crate::pair_refresh::preparation::CleanChildGitPins {
                child_tip: c.clone(),
                parent_tip: p2.clone(),
                parent_baseline_tip: p1.to_owned(),
            },
            Uuid::new_v4(),
            0,
        )
        .is_err());
        assert_eq!(storage(&f.db), before);
        assert_eq!(storage(engine.db()), before);
        assert_eq!(files(f.target.path()), shared);
        assert_eq!(files(&f._parent_wc), worktree);
        let reads = std::fs::read(&f.log).unwrap();
        assert!(reads.starts_with(&requests));
        assert!(reads.len() > requests.len());
        let added = std::str::from_utf8(&reads[requests.len()..]).unwrap();
        assert!(added.contains("git-upload-pack"));
        assert!(!added.contains("git-receive-pack"));
        let scratch = first.git.repository().path().to_owned();
        assert!(!scratch.join("FETCH_HEAD").exists());
        assert_eq!(first.git.repository().references().unwrap().count(), 0);
        drop(first);
        assert!(!scratch.exists());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn selectors_authentication_and_incarnation_refuse_before_live_reads() {
        let _serial = SERIAL.lock().await;
        let f = Fixture::new();
        let engine = f.engine();
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let op = ready(&f, &engine).await;
        let before = storage(&f.db);
        let requests = std::fs::read(&f.log).unwrap();
        assert!(
            super::super::capture_registered_changed_child_preview_context(
                &f.db,
                "child",
                Uuid::nil(),
            )
            .await
            .is_err()
        );
        assert!(capture(&f, Uuid::nil()).await.is_err());
        assert!(capture(&f, Uuid::new_v4()).await.is_err());
        let configuration =
            f.db.capture_pair_refresh_execution_configuration("child")
                .unwrap();
        let scope = &configuration.pin().child.scope_uuid;
        let echo = format!("repo_echo_generation_{scope}");
        let next_echo = (configuration.pin().child.echo_generation + 1).to_string();
        let auth = format!("pair_refresh_execution_v3:authority:{}", op.id);
        for (key, value) in [
            (auth.as_str(), "{}"),
            ("secret_git_token_child", ""),
            ("import_operation_v1:active:child", "missing-m2b-import"),
            (echo.as_str(), next_echo.as_str()),
        ] {
            let prior = saved_kv(&f, key);
            f.db.set_state(key, value).unwrap();
            let fault = storage(&f.db);
            assert!(capture(&f, op.id).await.is_err(), "negative key {key}");
            assert_eq!(storage(&f.db), fault);
            assert_eq!(storage(engine.db()), fault);
            assert_eq!(std::fs::read(&f.log).unwrap(), requests);
            restore_kv(&f, key, prior);
            assert_eq!(storage(&f.db), before);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn repeated_remote_observation_refuses_ref_and_svn_head_drift() {
        let _serial = SERIAL.lock().await;
        let f = Fixture::new();
        let engine = f.engine();
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let op = ready(&f, &engine).await;
        let before = storage(&f.db);
        let original = f
            .target
            .find_reference("refs/heads/feature")
            .unwrap()
            .target()
            .unwrap();
        let other = commit(&f.target, b"later child\n", &[original]);
        let result = super::super::capture_changed_child_preview_with_sources(
            &f.db,
            "child",
            op.id,
            Some(f.client.clone()),
            |_| {
                f.target
                    .reference("refs/heads/feature", other, true, "disposable drift")?;
                Ok(())
            },
        )
        .await;
        assert!(result.is_err());
        assert_eq!(
            f.target
                .find_reference("refs/heads/feature")
                .unwrap()
                .target(),
            Some(other)
        );
        assert_eq!(storage(&f.db), before);
        f.target
            .reference("refs/heads/feature", original, true, "disposable restore")
            .unwrap();
        let result = super::super::capture_changed_child_preview_with_sources(
            &f.db,
            "child",
            op.id,
            Some(f.client.clone()),
            |_| {
                // A real new repository revision, deliberately outside either
                // role. It is not rolled back or credited as API publication.
                svn(
                    &[
                        "mkdir",
                        &format!("{}/m2b-unrelated", f.root),
                        "-m",
                        "observation drift",
                    ],
                    None,
                );
                Ok(())
            },
        )
        .await;
        assert!(result.is_err());
        assert_eq!(storage(&f.db), before);
        assert_eq!(storage(engine.db()), before);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn final_recapture_refuses_stored_drift_and_new_admitted_window() {
        let _serial = SERIAL.lock().await;
        let f = Fixture::new();
        let engine = f.engine();
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let op = ready(&f, &engine).await;
        let before = storage(&f.db);
        let auth = format!("pair_refresh_execution_v3:authority:{}", op.id);
        let configuration =
            f.db.capture_pair_refresh_execution_configuration("child")
                .unwrap();
        let echo = format!(
            "repo_echo_generation_{}",
            configuration.pin().child.scope_uuid
        );
        let next_echo = (configuration.pin().child.echo_generation + 1).to_string();
        for (key, value) in [
            (auth.as_str(), "{}"),
            ("secret_git_token_child", ""),
            (echo.as_str(), next_echo.as_str()),
        ] {
            let prior = saved_kv(&f, key);
            let injected = std::cell::RefCell::new(None);
            let result = super::super::capture_changed_child_preview_with_sources(
                &f.db,
                "child",
                op.id,
                Some(f.client.clone()),
                |db| {
                    db.set_state(key, value)?;
                    *injected.borrow_mut() = Some(storage(db));
                    Ok(())
                },
            )
            .await;
            assert!(result.is_err(), "late negative key {key}");
            assert_eq!(storage(&f.db), *injected.borrow().as_ref().unwrap());
            assert_eq!(storage(engine.db()), *injected.borrow().as_ref().unwrap());
            restore_kv(&f, key, prior);
            assert_eq!(storage(&f.db), before);
        }
        let fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
        let window = std::cell::RefCell::new(None);
        let held_state = std::cell::RefCell::new(None);
        let result = super::super::capture_changed_child_preview_with_sources(
            &f.db,
            "child",
            op.id,
            Some(f.client.clone()),
            |_| {
                *window.borrow_mut() = Some(f.db.begin_ordinary_writer_window(Some("child"))?);
                *held_state.borrow_mut() = Some(journal_state(&f));
                Ok(())
            },
        )
        .await;
        assert!(result.is_err());
        // Only the existing independent lease heartbeat is excluded here.
        assert_eq!(journal_state(&f), *held_state.borrow().as_ref().unwrap());
        window
            .borrow_mut()
            .take()
            .unwrap()
            .finish_preparation()
            .unwrap();
        drop(fence);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn equivalent_content_and_unknown_properties_cannot_mint_a_baseline() {
        let _serial = SERIAL.lock().await;
        let f = Fixture::new();
        let engine = f.engine();
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let op = ready(&f, &engine).await;
        let configuration =
            f.db.capture_pair_refresh_execution_configuration("child")
                .unwrap();
        let c = Oid::from_str(&configuration.pin().child.last_git_sha).unwrap();
        // Equal bytes with P1 ancestry cannot substitute for R ancestry.
        let foreign = commit(&f.target, b"child work\n", &[f.parent]);
        assert_eq!(
            f.target.find_commit(c).unwrap().tree_id(),
            f.target.find_commit(foreign).unwrap().tree_id()
        );
        f.target
            .reference("refs/heads/feature", foreign, true, "disposable wrong role")
            .unwrap();
        set_checkpoint(&f, "child", foreign, configuration.pin().child.last_svn_rev);
        let fault = storage(&f.db);
        assert!(capture(&f, op.id).await.is_err());
        assert_eq!(storage(&f.db), fault);
        f.target
            .reference("refs/heads/feature", c, true, "disposable restore")
            .unwrap();
        set_checkpoint(&f, "child", c, configuration.pin().child.last_svn_rev);
        let first = capture(&f, op.id).await.unwrap();
        let child_wc = f.temp.path().join("m2b-child-wc");
        svn(&["update"], Some(&child_wc));
        svn(
            &["propset", "m2b:unproved", "not-a-mapping-proof", "."],
            Some(&child_wc),
        );
        svn(
            &["commit", "-m", "unproved fixture property"],
            Some(&child_wc),
        );
        let revision = svn_revision(&format!("{}/branches/f", f.root));
        set_checkpoint(&f, "child", c, revision);
        let second = capture(&f, op.id).await.unwrap();
        assert_ne!(first.context_digest(), second.context_digest());
        assert!(second
            .unverified_gates()
            .contains(&"svn_visibility_content_properties_mergeinfo_correspondence"));
        assert!(second
            .unverified_gates()
            .contains(&"current_mapping_provenance"));
        assert!(!second.execution_eligible());
    }
}

// Candidate-closure tests use genuine completed production receipts and the
// existing private TLS/Git/SVN fixture; no positive journal MAC is fabricated.
mod changed_child_baseline_candidate_closure {
    use super::*;
    use crate::db::pair_refresh_operations::{
        ChangedChildBaselineCandidateReview, PairRefreshOperation, PairRefreshPhase,
    };
    use std::collections::BTreeMap;

    type Storage = BTreeMap<String, Vec<Vec<rusqlite::types::Value>>>;

    fn storage(db: &Database) -> Storage {
        use rusqlite::types::Value;
        let conn = db.conn();
        let mut snapshot = BTreeMap::new();
        let schema = conn
            .prepare("SELECT type,name,tbl_name,rootpage,sql FROM sqlite_master ORDER BY type,name")
            .unwrap()
            .query_map([], |row| {
                (0..5)
                    .map(|i| row.get::<_, Value>(i))
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        snapshot.insert("schema".into(), schema);
        let tables = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        for table in tables {
            let sql = format!("SELECT * FROM \"{}\"", table.replace('"', "\"\""));
            let mut query = conn.prepare(&sql).unwrap();
            let columns = query.column_count();
            let mut values = query
                .query_map([], |row| {
                    (0..columns)
                        .map(|i| row.get::<_, Value>(i))
                        .collect::<rusqlite::Result<Vec<_>>>()
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            values.sort_by_cached_key(|row| format!("{row:?}"));
            snapshot.insert(table, values);
        }
        snapshot
    }

    fn files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        fn collect(root: &Path, path: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
            for entry in std::fs::read_dir(path).unwrap() {
                let entry = entry.unwrap();
                let kind = entry.file_type().unwrap();
                assert!(!kind.is_symlink());
                if kind.is_dir() {
                    collect(root, &entry.path(), out);
                } else {
                    assert!(kind.is_file());
                    out.insert(
                        entry.path().strip_prefix(root).unwrap().to_owned(),
                        std::fs::read(entry.path()).unwrap(),
                    );
                }
            }
        }
        let mut out = BTreeMap::new();
        collect(root, root, &mut out);
        out
    }

    fn set_checkpoint(f: &Fixture, id: &str, sha: Oid, revision: i64) {
        let scope: String =
            f.db.conn()
                .query_row(
                    "SELECT scope_uuid FROM repositories WHERE id=?1",
                    [id],
                    |r| r.get(0),
                )
                .unwrap();
        f.db.conn()
            .execute(
                "UPDATE repositories SET last_git_sha=?1,last_svn_rev=?2 WHERE id=?3",
                rusqlite::params![sha.to_string(), revision, id],
            )
            .unwrap();
        f.db.set_state(&format!("last_git_sha_{scope}"), &sha.to_string())
            .unwrap();
        f.db.set_state(&format!("last_svn_rev_{id}"), &revision.to_string())
            .unwrap();
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

    fn advance(f: &Fixture, op: &PairRefreshOperation) {
        let child_wc = f.temp.path().join("m2b-child-wc");
        svn(
            &[
                "checkout",
                &format!("{}/branches/f", f.root),
                child_wc.to_str().unwrap(),
            ],
            None,
        );
        std::fs::write(child_wc.join("file.txt"), b"child work\n").unwrap();
        svn(&["commit", "-m", "m2b child work"], Some(&child_wc));
        let child_revision = svn_revision(&format!("{}/branches/f", f.root));
        std::fs::write(f._parent_wc.join("file.txt"), b"parent work\n").unwrap();
        svn(&["commit", "-m", "m2b parent work"], Some(&f._parent_wc));
        let parent_revision = svn_revision(&format!("{}/trunk", f.root));
        let r = Oid::from_str(&op.pins.git.prepared_sha).unwrap();
        let p1 = Oid::from_str(&op.pins.parent.last_git_sha).unwrap();
        let c = commit(&f.target, b"child work\n", &[r]);
        let p2 = commit(&f.target, b"parent work\n", &[p1]);
        f.target
            .reference("refs/heads/feature", c, true, "disposable m2b fixture")
            .unwrap();
        f.target
            .reference("refs/heads/main", p2, true, "disposable m2b fixture")
            .unwrap();
        set_checkpoint(f, "child", c, child_revision);
        set_checkpoint(f, "parent", p2, parent_revision);
    }

    fn saved_kv(f: &Fixture, key: &str) -> Option<(String, String)> {
        use rusqlite::OptionalExtension;
        f.db.conn()
            .query_row(
                "SELECT value,updated_at FROM kv_state WHERE key=?1",
                [key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .unwrap()
    }

    fn restore_kv(f: &Fixture, key: &str, value: Option<(String, String)>) {
        match value {
            Some((value, updated)) => {
                f.db.conn().execute("INSERT INTO kv_state(key,value,updated_at) VALUES(?1,?2,?3) ON CONFLICT(key) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at", rusqlite::params![key, value, updated]).unwrap();
            }
            None => {
                f.db.conn()
                    .execute("DELETE FROM kv_state WHERE key=?1", [key])
                    .unwrap();
            }
        }
    }

    async fn completed(
        f: &Fixture,
        engine: &crate::sync_engine::SyncEngine,
        request: &str,
    ) -> PairRefreshOperation {
        let (actor, _workspace, value) = prepared(f, engine).await;
        let approved = actor.prepared_approval_digest(engine, &value).unwrap();
        let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
        let op = super::super::super::execution::complete_owned_refresh_with_client(
            engine,
            &actor,
            &value,
            request,
            &approved,
            &busy,
            f.client.clone(),
        )
        .await
        .unwrap();
        assert_eq!(op.version, 3);
        assert_eq!(op.phase, PairRefreshPhase::Completed);
        assert_eq!(f.db.pair_refresh_operation(op.id).unwrap(), op);
        op
    }

    async fn ready(f: &Fixture, engine: &crate::sync_engine::SyncEngine) -> PairRefreshOperation {
        let fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
        let op = completed(f, engine, "m2c-first-genuine-receipt").await;
        drop(fence);
        advance(f, &op);
        op
    }

    fn candidate(
        f: &Fixture,
    ) -> Result<ChangedChildBaselineCandidateReview, crate::errors::DatabaseError> {
        let current = f.db.capture_pair_refresh_execution_configuration("child")?;
        f.db.capture_changed_child_baseline_candidate_review(&current)
    }

    async fn capture(
        f: &Fixture,
    ) -> anyhow::Result<super::super::LiveChangedChildBaselineCandidateContext> {
        super::super::capture_changed_child_baseline_candidate_with_sources(
            &f.db,
            "child",
            Some(f.client.clone()),
            |_| Ok(()),
        )
        .await
    }

    fn request_log(f: &Fixture) -> Option<Vec<u8>> {
        match std::fs::read(&f.log) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => panic!("request-log observation failed: {error}"),
        }
    }

    #[track_caller]
    fn refuse_stored_without_repair(
        f: &Fixture,
        engine: &crate::sync_engine::SyncEngine,
        expected_detail: &str,
    ) {
        let before = storage(&f.db);
        let refs_and_objects = files(f.target.path());
        let parent_wc = files(&f._parent_wc);
        let requests = request_log(f);
        let error = match candidate(f) {
            Ok(_) => panic!("accepted negative candidate subcase {expected_detail}"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains(expected_detail),
            "unexpected refusal: {error}"
        );
        assert_eq!(storage(&f.db), before);
        assert_eq!(storage(engine.db()), before);
        assert_eq!(files(f.target.path()), refs_and_objects);
        assert_eq!(files(&f._parent_wc), parent_wc);
        assert_eq!(request_log(f), requests);
    }

    // Two actual production completions, including distinct parent commits
    // that return to the same content. No receipt/document/authority is signed
    // by a test helper. Real SVN revisions and Git commits retain their history.
    async fn two_genuine_receipts(
        f: &Fixture,
        engine: &crate::sync_engine::SyncEngine,
    ) -> (PairRefreshOperation, PairRefreshOperation) {
        let fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
        let first = completed(f, engine, "m2c-chain-first").await;
        std::fs::write(f._parent_wc.join("file.txt"), b"temporary parent work\n").unwrap();
        svn(
            &["commit", "-m", "m2c temporary parent work"],
            Some(&f._parent_wc),
        );
        let temporary = commit(&f.target, b"temporary parent work\n", &[f.parent]);
        std::fs::write(f._parent_wc.join("file.txt"), b"advanced\n").unwrap();
        svn(
            &["commit", "-m", "m2c parent content return"],
            Some(&f._parent_wc),
        );
        let parent_revision = svn_revision(&format!("{}/trunk", f.root));
        let parent = commit(&f.target, b"advanced\n", &[temporary]);
        f.target
            .reference(
                "refs/heads/main",
                parent,
                true,
                "disposable m2c parent advance",
            )
            .unwrap();
        f.db.insert_commit_map_with_repo(
            parent_revision,
            &parent.to_string(),
            "svn_to_git",
            "fixture",
            "fixture",
            Some("parent"),
        )
        .unwrap();
        f.db.update_repo_watermark("parent", parent_revision, &parent.to_string())
            .unwrap();
        crate::echo_receipt_scope::write_scoped_last_git_sha_kv(
            &f.db.conn(),
            "parent",
            &parent.to_string(),
            &chrono::Utc::now().to_rfc3339(),
        )
        .unwrap();
        let second = completed(f, engine, "m2c-chain-second").await;
        drop(fence);
        assert_ne!(first.id, second.id);
        assert_ne!(first.pins.git.prepared_sha, second.pins.git.prepared_sha);
        assert_eq!(first.pins.git.tree_sha, second.pins.git.tree_sha);
        assert_ne!(
            first.svn_receipt.as_ref().unwrap().revision,
            second.svn_receipt.as_ref().unwrap().revision
        );
        assert_eq!(f.db.pair_refresh_operation(first.id).unwrap(), first);
        assert_eq!(f.db.pair_refresh_operation(second.id).unwrap(), second);
        (first, second)
    }

    // Withhold and restore exact genuine rows only to control the read boundary.
    // The shared signing key and both genuine completion documents are retained.
    fn operation_rows(f: &Fixture, id: Uuid) -> Vec<(String, String, String)> {
        let document = format!("pair_refresh_execution_v3:document:{id}");
        let authority = format!("pair_refresh_execution_v3:authority:{id}");
        let rows = f.db.conn()
            .prepare("SELECT key,value,updated_at FROM kv_state WHERE key=?1 OR key=?2 OR (key LIKE 'pair_refresh_execution_v3:request:%' AND value=?3) ORDER BY key")
            .unwrap()
            .query_map(rusqlite::params![document, authority, id.to_string()], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(rows.len(), 3);
        rows
    }

    fn remove_rows(f: &Fixture, rows: &[(String, String, String)]) {
        for (key, _, _) in rows {
            assert_eq!(
                f.db.conn()
                    .execute("DELETE FROM kv_state WHERE key=?1", [key])
                    .unwrap(),
                1
            );
        }
    }

    fn restore_rows(f: &Fixture, rows: &[(String, String, String)]) {
        for (key, value, updated) in rows {
            assert_eq!(
                f.db.conn()
                    .execute(
                        "INSERT INTO kv_state(key,value,updated_at) VALUES(?1,?2,?3)",
                        rusqlite::params![key, value, updated],
                    )
                    .unwrap(),
                1
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn single_authenticated_candidate_is_stable_read_only_and_never_execution() {
        let _serial = SERIAL.lock().await;
        let f = Fixture::new();
        let engine = f.engine();
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let op = ready(&f, &engine).await;
        let before = storage(&f.db);
        let target = files(f.target.path());
        let parent_wc = files(&f._parent_wc);
        let requests = request_log(&f).unwrap();
        let first = capture(&f).await.unwrap();
        let second = capture(&f).await.unwrap();
        assert_eq!(
            first
                .candidate_review()
                .selected_review()
                .historical_operation(),
            &op
        );
        assert_eq!(
            first
                .preview_context()
                .historical_review()
                .historical_operation(),
            &op
        );
        assert_eq!(first.context_digest(), second.context_digest());
        assert_eq!(
            first.candidate_review().candidate_set_digest(),
            second.candidate_review().candidate_set_digest()
        );
        assert_ne!(
            first.context_digest(),
            first.preview_context().context_digest()
        );
        assert_ne!(op.pins.parent.last_git_sha, op.pins.git.prepared_sha);
        assert_ne!(
            first.preview_context().git_sources().child_tip,
            op.pins.git.prepared_sha
        );
        assert_ne!(
            first.preview_context().git_sources().parent_tip,
            op.pins.parent.last_git_sha
        );
        assert!(!first.preview_context().parent_contains_integrated_child());
        assert!(first
            .verified_gates()
            .contains(&"single_authenticated_receipt_candidate"));
        for gate in [
            "unique_applicable_historical_baseline",
            "current_mapping_provenance",
            "svn_visibility_content_properties_mergeinfo_correspondence",
            "current_actor_session_authorization",
            "exclusive_changed_child_execution_admission",
        ] {
            assert!(first.unverified_gates().contains(&gate));
        }
        assert!(!first.execution_eligible());
        assert!(!first.candidate_review().execution_eligible());
        assert!(!first.preview_context().execution_eligible());
        assert_eq!(storage(&f.db), before);
        assert_eq!(storage(engine.db()), before);
        assert_eq!(files(f.target.path()), target);
        assert_eq!(files(&f._parent_wc), parent_wc);
        let reads = request_log(&f).unwrap();
        assert!(reads.starts_with(&requests));
        let added = std::str::from_utf8(&reads[requests.len()..]).unwrap();
        assert!(added.contains("git-upload-pack"));
        assert!(!added.contains("git-receive-pack"));
        let scratch = first.preview.git.repository().path().to_owned();
        assert_eq!(
            first.preview.git.repository().references().unwrap().count(),
            0
        );
        assert!(!scratch.join("FETCH_HEAD").exists());
        drop(first);
        assert!(!scratch.exists());

        // Independent current credential resolution permits a valid rotation
        // for stored closure capture without reviving or altering signed history.
        let old = candidate(&f).unwrap();
        f.db.set_state("secret_git_token_child", "disposable-m2c-rotated-token")
            .unwrap();
        let rotated_state = storage(&f.db);
        let rotated = candidate(&f).unwrap();
        assert_eq!(
            old.selected_review().historical_document(),
            rotated.selected_review().historical_document()
        );
        assert_eq!(
            old.selected_review().historical_authority_digest(),
            rotated.selected_review().historical_authority_digest()
        );
        assert_ne!(old.candidate_set_digest(), rotated.candidate_set_digest());
        assert_eq!(storage(&f.db), rotated_state);
        assert_eq!(storage(engine.db()), rotated_state);
        // No live request is made with the rotated token in this storage check.
        assert_eq!(request_log(&f).unwrap(), reads);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn missing_or_ineligible_history_cannot_supply_a_candidate() {
        let _serial = SERIAL.lock().await;
        let f = Fixture::new();
        let engine = f.engine();
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        refuse_stored_without_repair(
            &f,
            &engine,
            "no authenticated same-incarnation receipt candidate",
        );
        assert!(!key_table(&f));
        assert_eq!(request_log(&f), None);
        {
            let fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
            let (actor, _workspace, value) = prepared(&f, &engine).await;
            let approved = actor.prepared_approval_digest(&engine, &value).unwrap();
            let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
            let held = actor
                .enroll_prepared(&engine, &value, "m2c-not-completed", &approved, &busy)
                .unwrap();
            assert_eq!(held.phase, PairRefreshPhase::Prepared);
            let before = journal_state(&f);
            assert!(candidate(&f).is_err());
            assert_eq!(journal_state(&f), before);
            let cancelled = actor
                .cancel_prepared_enrollment(&engine, &value, &busy)
                .unwrap();
            assert_eq!(
                cancelled.phase,
                PairRefreshPhase::CancelledBeforePublication
            );
            drop(fence);
        }
        // A real cancelled authority remains valid stored history but cannot
        // become a completed-receipt candidate. No signing key is synthesized.
        refuse_stored_without_repair(
            &f,
            &engine,
            "no authenticated same-incarnation receipt candidate",
        );
        let op = ready(&f, &engine).await;
        let current =
            f.db.capture_pair_refresh_execution_configuration("child")
                .unwrap();
        let echo = format!("repo_echo_generation_{}", current.pin().child.scope_uuid);
        let saved = saved_kv(&f, &echo);
        f.db.set_state(
            &echo,
            &(current.pin().child.echo_generation + 1).to_string(),
        )
        .unwrap();
        refuse_stored_without_repair(
            &f,
            &engine,
            "no authenticated same-incarnation receipt candidate",
        );
        restore_kv(&f, &echo, saved);
        let old_scope = current.pin().child.scope_uuid.clone();
        let new_scope = Uuid::new_v4().to_string();
        let key = format!("last_git_sha_{new_scope}");
        f.db.set_state(&key, &current.pin().child.last_git_sha)
            .unwrap();
        f.db.conn()
            .execute(
                "UPDATE repositories SET scope_uuid=?1 WHERE id='child'",
                [&new_scope],
            )
            .unwrap();
        refuse_stored_without_repair(
            &f,
            &engine,
            "no authenticated same-incarnation receipt candidate",
        );
        f.db.conn()
            .execute(
                "UPDATE repositories SET scope_uuid=?1 WHERE id='child'",
                [&old_scope],
            )
            .unwrap();
        f.db.conn()
            .execute("DELETE FROM kv_state WHERE key=?1", [&key])
            .unwrap();
        f.db.conn()
            .execute(
                "UPDATE repositories SET git_branch='other-feature' WHERE id='child'",
                [],
            )
            .unwrap();
        refuse_stored_without_repair(
            &f,
            &engine,
            "no authenticated same-incarnation receipt candidate",
        );
        f.db.conn()
            .execute(
                "UPDATE repositories SET git_branch='feature' WHERE id='child'",
                [],
            )
            .unwrap();
        assert_eq!(
            candidate(&f)
                .unwrap()
                .selected_review()
                .historical_operation(),
            &op
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn multiple_genuine_receipts_refuse_without_recency_or_equal_tree_selection() {
        let _serial = SERIAL.lock().await;
        let f = Fixture::new();
        let engine = f.engine();
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let (first, second) = two_genuine_receipts(&f, &engine).await;
        let first_rows = operation_rows(&f, first.id);
        let second_rows = operation_rows(&f, second.id);
        refuse_stored_without_repair(&f, &engine, "ambiguous authenticated receipt candidates");
        let before = storage(&f.db);
        let requests = request_log(&f);
        let error = match capture(&f).await {
            Ok(_) => panic!("selector-free live factory accepted two genuine receipts"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("ambiguous authenticated receipt candidates"));
        assert_eq!(storage(&f.db), before);
        assert_eq!(storage(engine.db()), before);
        assert_eq!(request_log(&f), requests);
        assert_eq!(operation_rows(&f, first.id), first_rows);
        assert_eq!(operation_rows(&f, second.id), second_rows);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn candidate_or_registration_drift_invalidates_final_recapture() {
        let _serial = SERIAL.lock().await;
        let f = Fixture::new();
        let engine = f.engine();
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let (first, second) = two_genuine_receipts(&f, &engine).await;
        let second_rows = operation_rows(&f, second.id);
        remove_rows(&f, &second_rows);
        assert_eq!(
            candidate(&f)
                .unwrap()
                .selected_review()
                .historical_operation(),
            &first
        );
        let target = files(f.target.path());
        let parent_wc = files(&f._parent_wc);
        let injected = std::cell::RefCell::new(None);
        let result = super::super::capture_changed_child_baseline_candidate_with_sources(
            &f.db,
            "child",
            Some(f.client.clone()),
            |_| {
                restore_rows(&f, &second_rows);
                *injected.borrow_mut() = Some(storage(&f.db));
                Ok(())
            },
        )
        .await;
        let error = match result {
            Ok(_) => panic!("late second genuine receipt did not invalidate candidate closure"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("ambiguous authenticated receipt candidates"),
            "{error}"
        );
        assert_eq!(storage(&f.db), *injected.borrow().as_ref().unwrap());
        assert_eq!(storage(engine.db()), *injected.borrow().as_ref().unwrap());
        assert_eq!(operation_rows(&f, second.id), second_rows);
        assert_eq!(files(f.target.path()), target);
        assert_eq!(files(&f._parent_wc), parent_wc);

        // Remove the exact fixture's second rows again, then change current
        // registration only after the initial authenticated/live observations.
        remove_rows(&f, &second_rows);
        let current =
            f.db.capture_pair_refresh_execution_configuration("child")
                .unwrap();
        let echo = format!("repo_echo_generation_{}", current.pin().child.scope_uuid);
        let prior = saved_kv(&f, &echo);
        let injected = std::cell::RefCell::new(None);
        let result = super::super::capture_changed_child_baseline_candidate_with_sources(
            &f.db,
            "child",
            Some(f.client.clone()),
            |db| {
                db.set_state(
                    &echo,
                    &(current.pin().child.echo_generation + 1).to_string(),
                )?;
                *injected.borrow_mut() = Some(storage(db));
                Ok(())
            },
        )
        .await;
        assert!(result.is_err());
        assert!(
            injected.borrow().is_some(),
            "fault boundary was not reached"
        );
        assert_eq!(storage(&f.db), *injected.borrow().as_ref().unwrap());
        assert_eq!(storage(engine.db()), *injected.borrow().as_ref().unwrap());
        assert_eq!(files(f.target.path()), target);
        assert_eq!(files(&f._parent_wc), parent_wc);
        restore_kv(&f, &echo, prior);
        restore_rows(&f, &second_rows);
        assert_eq!(f.db.pair_refresh_operation(second.id).unwrap(), second);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn corrupt_or_held_authority_refuses_without_repair() {
        let _serial = SERIAL.lock().await;
        let f = Fixture::new();
        let engine = f.engine();
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let op = ready(&f, &engine).await;
        let auth = format!("pair_refresh_execution_v3:authority:{}", op.id);
        let doc = format!("pair_refresh_execution_v3:document:{}", op.id);
        for (key, value) in [
            (auth.clone(), Some("{}")),
            (auth.clone(), None),
            (doc.clone(), Some("{}")),
            (
                "pair_refresh_execution_v99:unknown:retained".into(),
                Some("{}"),
            ),
            (
                "import_operation_v1:active:child".into(),
                Some("retained-m2c-import"),
            ),
        ] {
            let prior = saved_kv(&f, &key);
            match value {
                Some(value) => f.db.set_state(&key, value).unwrap(),
                None => {
                    f.db.conn()
                        .execute("DELETE FROM kv_state WHERE key=?1", [&key])
                        .unwrap();
                }
            }
            // Different malformed/unknown/held gates have distinct original
            // diagnostics; preserve all evidence instead of repairing on read.
            refuse_stored_without_repair(&f, &engine, "pair refresh journal:");
            restore_kv(&f, &key, prior);
        }
        let original = storage(&f.db);
        let secret: Vec<u8> =
            f.db.conn()
                .query_row(
                    "SELECT secret FROM pair_refresh_journal_key_v3 WHERE id=1",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
        f.db.conn()
            .execute("DELETE FROM pair_refresh_journal_key_v3 WHERE id=1", [])
            .unwrap();
        refuse_stored_without_repair(&f, &engine, "pair refresh journal:");
        assert!(!f
            .db
            .conn()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM pair_refresh_journal_key_v3 WHERE id=1)",
                [],
                |row| row.get::<_, bool>(0),
            )
            .unwrap());
        f.db.conn()
            .execute(
                "INSERT INTO pair_refresh_journal_key_v3(id,secret) VALUES(1,?1)",
                [secret],
            )
            .unwrap();
        assert_eq!(storage(&f.db), original);

        // Exercise a real current writer admission; the independent existing
        // lease heartbeat is excluded only in this bounded held-window check.
        let fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
        let window = f.db.begin_ordinary_writer_window(Some("child")).unwrap();
        window.require_engine_scope(&f.db, Some("child")).unwrap();
        let before = journal_state(&f);
        assert!(candidate(&f).is_err());
        assert_eq!(journal_state(&f), before);
        window.finish_preparation().unwrap();
        drop(fence);
        assert_eq!(
            candidate(&f)
                .unwrap()
                .selected_review()
                .historical_operation(),
            &op
        );
    }
}

// Genuine current mappings come from ordinary production SyncEngine cycles.
// Existing fixture bootstrap checkpoints are not full initialization evidence.
mod changed_child_source_correspondence {
    use super::super::LiveChangedChildSourceCorrespondence;
    use super::*;
    use crate::db::pair_refresh_operations::{
        ChangedChildMappingReview, PairRefreshOperation, PairRefreshPhase,
    };
    use std::collections::BTreeMap;

    type Storage = BTreeMap<String, Vec<Vec<rusqlite::types::Value>>>;

    fn storage(db: &Database) -> Storage {
        use rusqlite::types::Value;
        let conn = db.conn();
        let mut snapshot = BTreeMap::new();
        let schema = conn
            .prepare("SELECT type,name,tbl_name,rootpage,sql FROM sqlite_master ORDER BY type,name")
            .unwrap()
            .query_map([], |row| {
                (0..5)
                    .map(|i| row.get::<_, Value>(i))
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        snapshot.insert("schema".into(), schema);
        let tables = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        for table in tables {
            let sql = format!("SELECT * FROM \"{}\"", table.replace('"', "\"\""));
            let mut query = conn.prepare(&sql).unwrap();
            let columns = query.column_count();
            let mut values = query
                .query_map([], |row| {
                    (0..columns)
                        .map(|i| row.get::<_, Value>(i))
                        .collect::<rusqlite::Result<Vec<_>>>()
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            values.sort_by_cached_key(|row| format!("{row:?}"));
            snapshot.insert(table, values);
        }
        snapshot
    }

    fn files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        fn collect(root: &Path, path: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
            for entry in std::fs::read_dir(path).unwrap() {
                let entry = entry.unwrap();
                let kind = entry.file_type().unwrap();
                assert!(!kind.is_symlink());
                if kind.is_dir() {
                    collect(root, &entry.path(), out);
                } else {
                    assert!(kind.is_file());
                    out.insert(
                        entry.path().strip_prefix(root).unwrap().to_owned(),
                        std::fs::read(entry.path()).unwrap(),
                    );
                }
            }
        }
        let mut out = BTreeMap::new();
        collect(root, root, &mut out);
        out
    }

    fn set_checkpoint(f: &Fixture, id: &str, sha: Oid, revision: i64) {
        let scope: String =
            f.db.conn()
                .query_row(
                    "SELECT scope_uuid FROM repositories WHERE id=?1",
                    [id],
                    |r| r.get(0),
                )
                .unwrap();
        f.db.conn()
            .execute(
                "UPDATE repositories SET last_git_sha=?1,last_svn_rev=?2 WHERE id=?3",
                rusqlite::params![sha.to_string(), revision, id],
            )
            .unwrap();
        f.db.set_state(&format!("last_git_sha_{scope}"), &sha.to_string())
            .unwrap();
        f.db.set_state(&format!("last_svn_rev_{id}"), &revision.to_string())
            .unwrap();
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

    fn saved_kv(f: &Fixture, key: &str) -> Option<(String, String)> {
        use rusqlite::OptionalExtension;
        f.db.conn()
            .query_row(
                "SELECT value,updated_at FROM kv_state WHERE key=?1",
                [key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .unwrap()
    }

    fn restore_kv(f: &Fixture, key: &str, value: Option<(String, String)>) {
        match value {
            Some((value, updated)) => {
                f.db.conn().execute("INSERT INTO kv_state(key,value,updated_at) VALUES(?1,?2,?3) ON CONFLICT(key) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at", rusqlite::params![key, value, updated]).unwrap();
            }
            None => {
                f.db.conn()
                    .execute("DELETE FROM kv_state WHERE key=?1", [key])
                    .unwrap();
            }
        }
    }

    async fn completed(
        f: &Fixture,
        engine: &crate::sync_engine::SyncEngine,
    ) -> PairRefreshOperation {
        let (actor, _workspace, value) = prepared(f, engine).await;
        let approved = actor.prepared_approval_digest(engine, &value).unwrap();
        let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
        let op = super::super::super::execution::complete_owned_refresh_with_client(
            engine,
            &actor,
            &value,
            "m2d-genuine-history",
            &approved,
            &busy,
            f.client.clone(),
        )
        .await
        .unwrap();
        assert_eq!(op.phase, PairRefreshPhase::Completed);
        assert_eq!(f.db.pair_refresh_operation(op.id).unwrap(), op);
        op
    }

    // Use the same native DB and disposable bare repository, with a distinct
    // ordinary bridge per role. Local Git transport here avoids a new CA policy;
    // the correspondence factory still uses registered HTTPS observers/client.
    fn producer_engine(f: &Fixture, id: &str) -> (crate::sync_engine::SyncEngine, PathBuf) {
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
        config.daemon.data_dir = f.temp.path().to_owned();
        let svn_branch = if id == "child" { "branches/f" } else { "trunk" };
        let svn_url = format!("{}/{svn_branch}", f.root);
        config.svn.url = svn_url.clone();
        config.svn.layout = crate::config::SvnLayout::Custom;
        config.svn.password = Some(String::new());
        config.github.default_branch = branch.to_owned();
        config.github.token = Some(String::new());
        config.sync.auto_merge = false;
        let mut engine = crate::sync_engine::SyncEngine::new(
            config,
            Database::new(f.temp.path().join("reposync.db")).unwrap(),
            crate::svn::SvnClient::new(svn_url, "", ""),
            crate::git::GitClient::new(&path).unwrap(),
            std::sync::Arc::new(
                crate::identity::mapper::IdentityMapper::new(&crate::config::IdentityConfig {
                    email_domain: Some("example.invalid".into()),
                    ..Default::default()
                })
                .unwrap(),
            ),
        );
        engine.set_repo_id(id.to_owned());
        engine.set_git_credential_apply_clean(true);
        (engine, path)
    }

    // Fixture::new seeds an inbound KV without any ordinary handled-work proof.
    // The completed refresh proves the child only. For the untouched parent,
    // use the existing column-only, no-prior-work install bootstrap instead.
    // Remove only that exact fixture seed: never erase real mappings, receipts,
    // baselines, holds or an unknown cursor to make an ordinary cycle admissible.
    fn parent_install_bootstrap(f: &Fixture, history: &PairRefreshOperation) {
        use crate::db::repo_scope_identity as identity;

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
            crate::echo_receipt_scope::read_team_inbound_git_checkpoint_kv(&f.db, "parent")
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
        let local = crate::svn::SvnClient::new(&local_url, "", "");
        let entries = local.strict_log_range(revision, revision).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].revision, revision);
        assert_eq!(entries[0].author, "m2d-fixture-author");
        let registered = crate::svn::SvnClient::new(format!("{}/{branch}", f.root), "", "");
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

    async fn ready(f: &Fixture, engine: &crate::sync_engine::SyncEngine) -> Ready {
        let fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
        let history = completed(f, engine).await;
        parent_install_bootstrap(f, &history);
        let (parent_engine, parent_bridge) = producer_engine(f, "parent");
        let (child_engine, child_bridge) = producer_engine(f, "child");
        std::fs::write(f._parent_wc.join("file.txt"), b"m2d parent work\n").unwrap();
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

    fn mapping(f: &Fixture) -> Result<ChangedChildMappingReview, crate::errors::DatabaseError> {
        let current = f.db.capture_pair_refresh_execution_configuration("child")?;
        let candidate =
            f.db.capture_changed_child_baseline_candidate_review(&current)?;
        f.db.capture_changed_child_mapping_review(&current, &candidate)
    }

    async fn capture(f: &Fixture) -> anyhow::Result<LiveChangedChildSourceCorrespondence> {
        super::super::capture_changed_child_correspondence_with_sources(
            &f.db,
            "child",
            Some(f.client.clone()),
            |_| Ok(()),
        )
        .await
    }

    fn request_log(f: &Fixture) -> Option<Vec<u8>> {
        match std::fs::read(&f.log) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => panic!("request-log read failed: {error}"),
        }
    }

    // Successful ordinary-window observation retains the existing lease
    // heartbeat. Accept only its two clock fields, never a different holder,
    // acquisition, native lease, checkpoint, proof or any other stored state.
    #[track_caller]
    fn expected_after_only_ordinary_lease_renewal(db: &Database, before: &Storage) -> Storage {
        use rusqlite::types::Value;
        let actual = storage(db);
        let lease_row = |snapshot: &Storage| {
            let rows = snapshot["kv_state"]
                .iter()
                .filter(|row| row[0] == Value::Text("writer_fence_v1:lease".into()))
                .cloned()
                .collect::<Vec<_>>();
            assert_eq!(rows.len(), 1, "one existing lease must remain");
            assert_eq!(rows[0].len(), 3);
            rows[0].clone()
        };
        let text = |value: &Value| match value {
            Value::Text(value) => value.clone(),
            _ => panic!("lease fields must remain text"),
        };
        let old_row = lease_row(before);
        let new_row = lease_row(&actual);
        let old: serde_json::Value = serde_json::from_str(&text(&old_row[1])).unwrap();
        let new: serde_json::Value = serde_json::from_str(&text(&new_row[1])).unwrap();
        let fields = old
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        assert_eq!(
            fields,
            [
                "acquired_at",
                "epoch",
                "holder_host",
                "holder_pid",
                "renewed_at",
                "version"
            ]
        );
        let acquired =
            chrono::DateTime::parse_from_rfc3339(old["acquired_at"].as_str().unwrap()).unwrap();
        let old_renewed =
            chrono::DateTime::parse_from_rfc3339(old["renewed_at"].as_str().unwrap()).unwrap();
        let new_renewed =
            chrono::DateTime::parse_from_rfc3339(new["renewed_at"].as_str().unwrap()).unwrap();
        assert!(acquired <= old_renewed && old_renewed <= new_renewed);
        assert!(
            chrono::DateTime::parse_from_rfc3339(&text(&old_row[2])).unwrap()
                <= chrono::DateTime::parse_from_rfc3339(&text(&new_row[2])).unwrap()
        );
        let mut expected_lease = old;
        expected_lease["renewed_at"] = new["renewed_at"].clone();
        assert_eq!(
            new, expected_lease,
            "all acquisition fields must remain exact"
        );
        let mut expected = before.clone();
        let row = expected
            .get_mut("kv_state")
            .unwrap()
            .iter_mut()
            .find(|row| row[0] == Value::Text("writer_fence_v1:lease".into()))
            .unwrap();
        row[1] = new_row[1].clone();
        row[2] = new_row[2].clone();
        assert_eq!(actual, expected, "only checked lease clocks may change");
        expected
    }

    fn child_row(f: &Fixture) -> (String, String, i64) {
        let current =
            f.db.capture_pair_refresh_execution_configuration("child")
                .unwrap();
        f.db.conn().query_row(
            "SELECT id,git_sha,svn_rev FROM sync_records WHERE repo_id='child' AND git_sha=?1 AND svn_rev=?2 AND status='applied' ORDER BY id",
            rusqlite::params![current.pin().child.last_git_sha, current.pin().child.last_svn_rev],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).unwrap()
    }

    #[track_caller]
    fn refuse_mapping_without_repair(
        f: &Fixture,
        engine: &crate::sync_engine::SyncEngine,
        detail: &str,
    ) {
        let before = storage(&f.db);
        let target = files(f.target.path());
        let parent_wc = files(&f._parent_wc);
        let requests = request_log(f);
        let error = match mapping(f) {
            Ok(_) => panic!("accepted negative mapping subcase {detail}"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains(detail),
            "unexpected mapping refusal: {error}"
        );
        assert_eq!(storage(&f.db), before);
        assert_eq!(storage(engine.db()), before);
        assert_eq!(files(f.target.path()), target);
        assert_eq!(files(&f._parent_wc), parent_wc);
        assert_eq!(request_log(f), requests);
    }

    async fn refuse_live_without_repair(
        f: &Fixture,
        engine: &crate::sync_engine::SyncEngine,
    ) -> String {
        let before = storage(&f.db);
        let target = files(f.target.path());
        let parent_wc = files(&f._parent_wc);
        let error = match capture(f).await {
            Ok(_) => panic!("source correspondence accepted a negative fixture"),
            Err(error) => error.to_string(),
        };
        assert_eq!(storage(&f.db), before);
        assert_eq!(storage(engine.db()), before);
        assert_eq!(files(f.target.path()), target);
        assert_eq!(files(&f._parent_wc), parent_wc);
        error
    }
    fn sync_row(f: &Fixture, id: &str) -> Vec<rusqlite::types::Value> {
        let conn = f.db.conn();
        let mut query = conn
            .prepare("SELECT * FROM sync_records WHERE id=?1")
            .unwrap();
        let columns = query.column_count();
        query
            .query_row([id], |row| {
                (0..columns)
                    .map(|i| row.get(i))
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap()
    }

    fn restore_sync_row(f: &Fixture, row: &[rusqlite::types::Value]) {
        let placeholders = (1..=row.len())
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(",");
        f.db.conn()
            .execute(
                &format!("INSERT INTO sync_records VALUES({placeholders})"),
                rusqlite::params_from_iter(row),
            )
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn genuine_current_mappings_and_four_visible_roles_bind_one_review() {
        let _serial = SERIAL.lock().await;
        let f = Fixture::new();
        let engine = f.engine();
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let ready = ready(&f, &engine).await;
        let before = storage(&f.db);
        let first = capture(&f).await.unwrap();
        let second = capture(&f).await.unwrap();
        assert_eq!(first.context_digest(), second.context_digest());
        assert_eq!(
            first.mapping_review().snapshot_digest(),
            second.mapping_review().snapshot_digest()
        );
        assert_eq!(first.mapping_review().mapping_row_counts(), [1, 1]);
        assert_eq!(first.retained_snapshot_count(), 4);
        assert_eq!(
            first
                .roles
                .iter()
                .map(|p| p.role.as_str())
                .collect::<Vec<_>>(),
            ["P1", "R", "C", "P2"]
        );
        assert_eq!(
            first.roles[0].git_commit,
            ready.history.pins.parent.last_git_sha
        );
        assert_eq!(
            first.roles[1].git_commit,
            ready.history.pins.git.prepared_sha
        );
        assert_ne!(first.roles[0].git_commit, first.roles[1].git_commit);
        assert_ne!(first.roles[1].git_commit, first.roles[2].git_commit);
        assert_ne!(first.roles[0].git_commit, first.roles[3].git_commit);
        assert_eq!(first.roles[0].git_tree, first.roles[1].git_tree);
        assert_ne!(first.roles[2].git_tree, first.roles[3].git_tree);
        for proof in &first.roles {
            assert!(proof.presence.node_count > 0);
            assert!(!proof.presence.ancestors.is_empty());
            assert!(proof.presence.max_changed_revision <= proof.handled_revision);
        }
        let expected_mergeinfo = ready
            .history
            .svn_receipt
            .as_ref()
            .unwrap()
            .mergeinfo
            .as_str();
        assert_eq!(
            first.roles[1].expected_mergeinfo.as_deref(),
            Some(expected_mergeinfo)
        );
        assert_eq!(
            first.roles[2].expected_mergeinfo.as_deref(),
            Some(expected_mergeinfo)
        );
        assert_eq!(first.roles[0].expected_mergeinfo, None);
        assert_eq!(first.roles[3].expected_mergeinfo, None);
        assert!(first
            .verified_gates()
            .contains(&"current_scoped_applied_mapping_capture"));
        assert!(first
            .verified_gates()
            .contains(&"four_role_complete_visible_svn_correspondence"));
        assert!(!first.execution_eligible());
        assert!(!first.mapping_review().execution_eligible());
        assert!(!first.candidate_context().execution_eligible());
        assert!(first
            .unverified_gates()
            .contains(&"authenticated_changed_child_resolution"));
        assert!(first
            .unverified_gates()
            .contains(&"exclusive_changed_child_execution_admission"));
        assert_eq!(storage(&f.db), before);
        assert_eq!(storage(engine.db()), before);
        assert_eq!(
            f.db.pair_refresh_operation(ready.history.id).unwrap(),
            ready.history
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn missing_foreign_split_and_unscoped_correspondence_refuse() {
        let _serial = SERIAL.lock().await;
        let f = Fixture::new();
        let engine = f.engine();
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let _ready = ready(&f, &engine).await;
        let (id, _, _) = child_row(&f);
        let original = sync_row(&f, &id);
        let before = storage(&f.db);
        f.db.conn()
            .execute("DELETE FROM sync_records WHERE id=?1", [&id])
            .unwrap();
        refuse_mapping_without_repair(
            &f,
            &engine,
            "current scoped applied correspondence is missing",
        );
        restore_sync_row(&f, &original);
        assert_eq!(storage(&f.db), before);
        for update in [
            "UPDATE sync_records SET repo_id='foreign-repo' WHERE id=?1",
            "UPDATE sync_records SET svn_rev=svn_rev+1 WHERE id=?1",
            "UPDATE sync_records SET status='pending' WHERE id=?1",
            "UPDATE sync_records SET direction='unknown-direction' WHERE id=?1",
        ] {
            f.db.conn().execute(update, [&id]).unwrap();
            refuse_mapping_without_repair(&f, &engine, "current");
            f.db.conn()
                .execute("DELETE FROM sync_records WHERE id=?1", [&id])
                .unwrap();
            restore_sync_row(&f, &original);
            assert_eq!(storage(&f.db), before);
        }
        let collision = Uuid::new_v4().to_string();
        f.db.conn().execute(
            "INSERT INTO sync_records(id,repo_id,svn_rev,git_sha,direction,author,message,timestamp,synced_at,status) SELECT ?1,NULL,svn_rev,git_sha,direction,author,message,timestamp,synced_at,status FROM sync_records WHERE id=?2",
            rusqlite::params![collision, id],
        ).unwrap();
        refuse_mapping_without_repair(&f, &engine, "foreign or contradictory");
        f.db.conn()
            .execute("DELETE FROM sync_records WHERE id=?1", [&collision])
            .unwrap();
        assert_eq!(storage(&f.db), before);
        let current =
            f.db.capture_pair_refresh_execution_configuration("child")
                .unwrap();
        let echo = format!("repo_echo_generation_{}", current.pin().child.scope_uuid);
        let prior = saved_kv(&f, &echo);
        f.db.set_state(
            &echo,
            &(current.pin().child.echo_generation + 1).to_string(),
        )
        .unwrap();
        refuse_mapping_without_repair(
            &f,
            &engine,
            "no authenticated same-incarnation receipt candidate",
        );
        restore_kv(&f, &echo, prior);
        assert_eq!(storage(&f.db), before);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rewritten_equal_content_or_mismatched_roles_refuse() {
        let _serial = SERIAL.lock().await;
        let f = Fixture::new();
        let engine = f.engine();
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let ready = ready(&f, &engine).await;
        let (id, original, revision) = child_row(&f);
        let c = Oid::from_str(&original).unwrap();
        let foreign = commit(&f.target, b"m2d child work\n", &[f.parent]);
        assert_eq!(
            f.target.find_commit(c).unwrap().tree_id(),
            f.target.find_commit(foreign).unwrap().tree_id()
        );
        f.target
            .reference(
                "refs/heads/feature",
                foreign,
                true,
                "disposable rewritten fixture",
            )
            .unwrap();
        set_checkpoint(&f, "child", foreign, revision);
        // Tampering the original applied row is a negative fault, not a new
        // genuine mapping. Full graph proof must refuse despite equal bytes.
        f.db.conn()
            .execute(
                "UPDATE sync_records SET git_sha=?1 WHERE id=?2",
                rusqlite::params![foreign.to_string(), id],
            )
            .unwrap();
        // Keep the signed R reachable through the other registered branch.
        // This is a disposable negative graph, not a new positive SVN mapping.
        let (parent_revision, parent_tip) = f.db.get_repo_watermark("parent").unwrap();
        let parent_oid = Oid::from_str(&parent_tip).unwrap();
        let visible_parent = commit(&f.target, b"m2d parent work\n", &[parent_oid, c]);
        assert_eq!(
            f.target.find_commit(parent_oid).unwrap().tree_id(),
            f.target.find_commit(visible_parent).unwrap().tree_id()
        );
        f.target
            .reference(
                "refs/heads/main",
                visible_parent,
                true,
                "negative graph retains signed receipt object",
            )
            .unwrap();
        set_checkpoint(&f, "parent", visible_parent, parent_revision);
        let current =
            f.db.capture_pair_refresh_execution_configuration("child")
                .unwrap();
        let (_, observed) =
            super::super::changed_child_git_observation(current, Some(f.client.clone()))
                .await
                .unwrap();
        let receipt = Oid::from_str(&ready.history.pins.git.prepared_sha).unwrap();
        assert!(
            observed.repository().find_commit(receipt).is_ok(),
            "registered observer must really receive signed R"
        );
        assert!(!observed
            .repository()
            .graph_descendant_of(foreign, receipt)
            .unwrap());
        drop(observed);
        let error = refuse_live_without_repair(&f, &engine).await;
        assert!(error.contains("distinct receipt roles"), "{error}");
        assert_eq!(
            f.target
                .find_reference("refs/heads/feature")
                .unwrap()
                .target(),
            Some(foreign)
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unhandled_git_or_svn_work_remains_explicit_and_preserved() {
        let _serial = SERIAL.lock().await;
        let f = Fixture::new();
        let engine = f.engine();
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let ready = ready(&f, &engine).await;
        let (_, sha, _) = child_row(&f);
        let original = Oid::from_str(&sha).unwrap();
        let pending = commit(&f.target, b"unhandled git work\n", &[original]);
        f.target
            .reference(
                "refs/heads/feature",
                pending,
                true,
                "disposable pending Git",
            )
            .unwrap();
        refuse_live_without_repair(&f, &engine).await;
        assert_eq!(
            f.target
                .find_reference("refs/heads/feature")
                .unwrap()
                .target(),
            Some(pending)
        );
        f.target
            .reference("refs/heads/feature", original, true, "disposable restore")
            .unwrap();
        std::fs::write(ready.child_wc.join("file.txt"), b"unhandled SVN work\n").unwrap();
        svn(
            &["commit", "-m", "m2d unhandled SVN-only work"],
            Some(&ready.child_wc),
        );
        let revision = svn_revision(&format!("{}/branches/f", f.root));
        let error = refuse_live_without_repair(&f, &engine).await;
        assert!(error.contains("pending changes"), "{error}");
        assert_eq!(svn_revision(&format!("{}/branches/f", f.root)), revision);
        assert_eq!(
            std::fs::read(ready.child_wc.join("file.txt")).unwrap(),
            b"unhandled SVN work\n"
        );
    }

    fn changed_child_tree(f: &Fixture, parent: Oid, bytes: &[u8], mode: i32) -> Oid {
        let old = f.target.find_commit(parent).unwrap();
        let old_tree = old.tree().unwrap();
        let mut tree = f.target.treebuilder(Some(&old_tree)).unwrap();
        tree.insert("file.txt", f.target.blob(bytes).unwrap(), mode)
            .unwrap();
        let tree = f.target.find_tree(tree.write().unwrap()).unwrap();
        let signature = Signature::new(
            "Fixture",
            "fixture@example.invalid",
            &Time::new(1_790_000_000, 0),
        )
        .unwrap();
        f.target
            .commit(
                None,
                &signature,
                &signature,
                "negative source tree",
                &tree,
                &[&old],
            )
            .unwrap()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bytes_modes_empty_directories_and_property_drift_refuse() {
        let _serial = SERIAL.lock().await;
        {
            let f = Fixture::new();
            let engine = f.engine();
            let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
            let _ready = ready(&f, &engine).await;
            let (id, original, revision) = child_row(&f);
            let original = Oid::from_str(&original).unwrap();
            let config =
                f.db.capture_pair_refresh_execution_configuration("child")
                    .unwrap();
            let git_key = format!("last_git_sha_{}", config.pin().child.scope_uuid);
            let svn_key = "last_svn_rev_child";
            let git_kv = saved_kv(&f, &git_key);
            let svn_kv = saved_kv(&f, svn_key);
            let original_state = storage(&f.db);
            for (bytes, mode, detail) in [
                (
                    &b"\0unmapped binary\xff"[..],
                    0o100644,
                    "SVN content differs",
                ),
                (
                    &b"m2d child work\n"[..],
                    0o100755,
                    "SVN executable property differs",
                ),
            ] {
                let wrong = changed_child_tree(&f, original, bytes, mode);
                f.target
                    .reference("refs/heads/feature", wrong, true, "disposable mismatch")
                    .unwrap();
                set_checkpoint(&f, "child", wrong, revision);
                f.db.conn()
                    .execute(
                        "UPDATE sync_records SET git_sha=?1 WHERE id=?2",
                        rusqlite::params![wrong.to_string(), id],
                    )
                    .unwrap();
                let error = refuse_live_without_repair(&f, &engine).await;
                assert!(error.contains(detail), "{error}");
                f.target
                    .reference("refs/heads/feature", original, true, "disposable restore")
                    .unwrap();
                f.db.conn()
                    .execute(
                        "UPDATE repositories SET last_git_sha=?1 WHERE id='child'",
                        [original.to_string()],
                    )
                    .unwrap();
                f.db.conn()
                    .execute(
                        "UPDATE sync_records SET git_sha=?1 WHERE id=?2",
                        rusqlite::params![original.to_string(), id],
                    )
                    .unwrap();
                restore_kv(&f, &git_key, git_kv.clone());
                restore_kv(&f, svn_key, svn_kv.clone());
                assert_eq!(storage(&f.db), original_state);
            }
        }
        for property_fault in ["empty-directory", "unknown-property", "mergeinfo"] {
            let f = Fixture::new();
            let engine = f.engine();
            let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
            let ready = ready(&f, &engine).await;
            let (id, sha, _) = child_row(&f);
            let mut status = Command::new("svn");
            status
                .args(["status", "--ignore-externals"])
                .current_dir(&ready.child_wc);
            assert!(
                run(&mut status).trim().is_empty(),
                "do not update over pending fixture work"
            );
            let root_revision = svn_revision(&local_source_url(&f, "branches/f"));
            svn(
                &[
                    "update",
                    "--revision",
                    &root_revision.to_string(),
                    "--ignore-externals",
                ],
                Some(&ready.child_wc),
            );
            assert_eq!(
                svn_revision(ready.child_wc.to_str().unwrap()),
                root_revision
            );
            eprintln!("M2d property fault={property_fault} root_revision={root_revision}");
            match property_fault {
                "empty-directory" => svn(&["mkdir", "empty"], Some(&ready.child_wc)),
                "unknown-property" => svn(
                    &["propset", "m2d:unknown", "retained", "."],
                    Some(&ready.child_wc),
                ),
                "mergeinfo" => svn(
                    &["propset", "svn:mergeinfo", "/trunk:1", "."],
                    Some(&ready.child_wc),
                ),
                _ => unreachable!(),
            }
            svn(
                &["commit", "-m", "negative correspondence metadata"],
                Some(&ready.child_wc),
            );
            let revision = svn_revision(&format!("{}/branches/f", f.root));
            set_checkpoint(&f, "child", Oid::from_str(&sha).unwrap(), revision);
            // Retain but deliberately mislabel a current row for this negative
            // case. Neither a row nor equal Git bytes can prove SVN properties.
            f.db.conn()
                .execute(
                    "UPDATE sync_records SET svn_rev=?1 WHERE id=?2",
                    rusqlite::params![revision, id],
                )
                .unwrap();
            let error = refuse_live_without_repair(&f, &engine).await;
            let detail = match property_fault {
                "empty-directory" => "SVN and Git tree paths differ",
                "unknown-property" => "unsupported SVN property semantics",
                "mergeinfo" => "SVN mergeinfo differs",
                _ => unreachable!(),
            };
            assert!(error.contains(detail), "{property_fault}: {error}");
            assert_eq!(svn_revision(&format!("{}/branches/f", f.root)), revision);
        }
    }
    async fn serve_with_authz(f: &mut Fixture) -> fixture_process::OwnedProcessGroup {
        let conf = f.temp.path().join("svn-repository/conf");
        std::fs::write(
            conf.join("svnserve.conf"),
            "[general]\nanon-access = write\nauth-access = write\nauthz-db = authz\n",
        )
        .unwrap();
        std::fs::write(conf.join("authz"), "[/]\n* = rw\n").unwrap();
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let mut command = Command::new("svnserve");
        command
            .args([
                "--daemon",
                "--foreground",
                "--listen-host",
                "127.0.0.1",
                "--listen-port",
            ])
            .arg(port.to_string())
            .arg("--root")
            .arg(f.temp.path())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        let server = fixture_process::OwnedProcessGroup::spawn(&mut command).unwrap();
        f.root = format!("svn://127.0.0.1:{port}/svn-repository");
        f.db.conn()
            .execute("UPDATE repositories SET svn_url=?1", [&f.root])
            .unwrap();
        let client = crate::svn::SvnClient::new(format!("{}/trunk", f.root), "", "");
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if client.info().await.is_ok() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("disposable svnserve did not become ready");
        server
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn excluded_sparse_unreadable_ancestor_or_history_refuses() {
        let _serial = SERIAL.lock().await;
        let mut f = Fixture::new();
        let _svnserve = serve_with_authz(&mut f).await;
        let engine = f.engine();
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let _ready = ready(&f, &engine).await;
        let first = capture(&f).await.unwrap();
        let client = first.candidate.preview.svn.child_client();
        let head = first.candidate.preview.svn.pins().inspected_head_revision;
        // Controlled corruption of task-owned fixture metadata exercises the
        // same real WC31 validator; no OS function/production parser replacement.
        for sql in [
            "UPDATE NODES SET presence='server-excluded' WHERE local_relpath='file.txt'",
            "UPDATE NODES SET depth='empty' WHERE local_relpath=''",
        ] {
            let snapshot = super::super::super::svn_visibility::observe_subtree(client, head)
                .await
                .unwrap();
            let metadata = rusqlite::Connection::open(snapshot.path().join(".svn/wc.db")).unwrap();
            assert!(metadata.execute(sql, []).unwrap() > 0);
            drop(metadata);
            assert!(super::super::super::svn_visibility::presence_pin(
                "negative",
                snapshot.path(),
                snapshot.info(),
            )
            .is_err());
        }
        drop(first);
        let authz = f.temp.path().join("svn-repository/conf/authz");
        std::fs::write(&authz, "[/]\n* = rw\n[/branches/f/file.txt]\n* =\n").unwrap();
        // Genuine read authorization changes, with all recorded coordinates
        // unchanged. A history/visibility prerequisite may refuse first; never
        // invent a more specific runtime cause or call an omitted path empty.
        refuse_live_without_repair(&f, &engine).await;
        let child = crate::svn::SvnClient::new_isolated_refresh(
            &format!("{}/branches/f", f.root),
            "",
            None,
        )
        .unwrap();
        assert!(
            super::super::super::svn_visibility::observe_subtree(&child, head)
                .await
                .is_err()
        );
        std::fs::write(&authz, "[/]\n* = rw\n[/branches]\n* =\n").unwrap();
        refuse_live_without_repair(&f, &engine).await;
        let retained = std::fs::read(&authz).unwrap();
        assert_eq!(retained, b"[/]\n* = rw\n[/branches]\n* =\n");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn mapping_candidate_sources_and_credentials_drift_refuse() {
        let _serial = SERIAL.lock().await;
        let f = Fixture::new();
        let engine = f.engine();
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let _ready = ready(&f, &engine).await;
        let (id, sha, _) = child_row(&f);
        let original_row = sync_row(&f, &id);
        let credential = saved_kv(&f, "secret_git_token_child");
        let current_configuration =
            f.db.capture_pair_refresh_execution_configuration("child")
                .unwrap();
        let current_candidate =
            f.db.capture_changed_child_baseline_candidate_review(&current_configuration)
                .unwrap();
        let auth_key = format!(
            "pair_refresh_execution_v3:authority:{}",
            current_candidate
                .selected_review()
                .historical_operation()
                .id
        );
        let authority = saved_kv(&f, &auth_key);
        for fault in [
            "mapping-metadata",
            "missing-mapping",
            "authority",
            "credentials",
        ] {
            let injected = std::cell::RefCell::new(None);
            let result = super::super::capture_changed_child_correspondence_with_sources(
                &f.db,
                "child",
                Some(f.client.clone()),
                |db| {
                    match fault {
                        "mapping-metadata" => {
                            db.conn().execute(
                                "UPDATE sync_records SET message=message||' late' WHERE id=?1",
                                [&id],
                            )?;
                        }
                        "missing-mapping" => {
                            db.conn()
                                .execute("DELETE FROM sync_records WHERE id=?1", [&id])?;
                        }
                        "authority" => db.set_state(&auth_key, "{}")?,
                        "credentials" => db.set_state("secret_git_token_child", "")?,
                        _ => unreachable!(),
                    }
                    *injected.borrow_mut() = Some(storage(db));
                    Ok(())
                },
            )
            .await;
            let error = match result {
                Ok(_) => panic!("late {fault} produced a correspondence proof"),
                Err(error) => error.to_string(),
            };
            assert!(
                injected.borrow().is_some(),
                "late {fault} boundary was not reached: {error}"
            );
            if fault == "mapping-metadata" {
                assert!(error.contains("stored evidence changed"), "{error}");
            }
            assert_eq!(storage(&f.db), *injected.borrow().as_ref().unwrap());
            assert_eq!(storage(engine.db()), *injected.borrow().as_ref().unwrap());
            f.db.conn()
                .execute("DELETE FROM sync_records WHERE id=?1", [&id])
                .unwrap();
            restore_sync_row(&f, &original_row);
            restore_kv(&f, &auth_key, authority.clone());
            restore_kv(&f, "secret_git_token_child", credential.clone());
        }
        let current = Oid::from_str(&sha).unwrap();
        let pending = commit(&f.target, b"late unhandled source\n", &[current]);
        let reached = std::cell::Cell::new(false);
        let result = super::super::capture_changed_child_correspondence_with_sources(
            &f.db,
            "child",
            Some(f.client.clone()),
            |_| {
                f.target
                    .reference("refs/heads/feature", pending, true, "late source fault")?;
                reached.set(true);
                Ok(())
            },
        )
        .await;
        assert!(result.is_err());
        assert!(reached.get());
        assert_eq!(
            f.target
                .find_reference("refs/heads/feature")
                .unwrap()
                .target(),
            Some(pending)
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn review_capture_preserves_shared_storage_refs_and_worktrees() {
        let _serial = SERIAL.lock().await;
        let f = Fixture::new();
        let engine = f.engine();
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let ready = ready(&f, &engine).await;
        let before = storage(&f.db);
        let target = files(f.target.path());
        let parent_wc = files(&f._parent_wc);
        let child_wc = files(&ready.child_wc);
        let parent_bridge = files(&ready.parent_bridge);
        let child_bridge = files(&ready.child_bridge);
        let requests = request_log(&f).unwrap();
        let proof = capture(&f).await.unwrap();
        let snapshots = proof
            .snapshots
            .iter()
            .map(|s| s.path().parent().unwrap().to_owned())
            .collect::<Vec<_>>();
        let git_scratch = proof.candidate.preview.git.repository().path().to_owned();
        assert_eq!(snapshots.len(), 4);
        assert!(snapshots.iter().all(|p| p.is_dir()));
        assert!(!proof.execution_eligible());
        let reads = request_log(&f).unwrap();
        assert!(reads.starts_with(&requests));
        let added = std::str::from_utf8(&reads[requests.len()..]).unwrap();
        assert!(added.contains("git-upload-pack"));
        assert!(!added.contains("git-receive-pack"));
        assert_eq!(storage(&f.db), before);
        assert_eq!(storage(engine.db()), before);
        assert_eq!(files(f.target.path()), target);
        assert_eq!(files(&f._parent_wc), parent_wc);
        assert_eq!(files(&ready.child_wc), child_wc);
        assert_eq!(files(&ready.parent_bridge), parent_bridge);
        assert_eq!(files(&ready.child_bridge), child_bridge);
        assert_eq!(
            f.db.pair_refresh_operation(ready.history.id).unwrap(),
            ready.history
        );
        drop(proof);
        assert!(snapshots.iter().all(|p| !p.exists()));
        assert!(!git_scratch.exists());
        assert_eq!(storage(&f.db), before);
    }

    // The negative capability tests below reuse genuinely produced completed
    // journals and mappings from ready(). Only the inbound pointer is rolled
    // back to the genuine signed R to model an unsettled range; no positive
    // mapping/receipt is synthesized. Each injected fault must remain refused.
    fn unsettled_child_input(
        f: &Fixture,
        ready: &Ready,
    ) -> (String, String, String, Vec<(String, String)>) {
        let old = ready.history.pins.git.prepared_sha.clone();
        let (_, current) = f.db.get_repo_watermark("child").unwrap();
        let commit = f
            .target
            .find_commit(Oid::from_str(&current).unwrap())
            .unwrap();
        assert_eq!(commit.parent_count(), 1);
        assert_eq!(commit.parent_id(0).unwrap().to_string(), old);
        let origin = Repository::open(&ready.child_bridge)
            .unwrap()
            .find_remote("origin")
            .unwrap()
            .url()
            .unwrap()
            .to_owned();
        crate::echo_receipt_scope::write_scoped_last_git_sha_kv(
            &f.db.conn(),
            "child",
            &old,
            &chrono::Utc::now().to_rfc3339(),
        )
        .unwrap();
        let chain = vec![(current.clone(), commit.tree_id().to_string())];
        (old, current, origin, chain)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn producer_convergence_real_roles_and_quiet_cycles_do_not_republish() {
        let _serial = SERIAL.lock().await;
        let f = Fixture::new();
        let engine = f.engine();
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let _ready = ready(&f, &engine).await;
        let pin =
            f.db.capture_pair_refresh_execution_configuration("child")
                .unwrap();
        for role in [&pin.pin().child, &pin.pin().parent] {
            assert_eq!(
                role.scoped_git_checkpoint.as_deref(),
                Some(role.last_git_sha.as_str())
            );
        }
        let source = files(f.target.path());
        let revision = svn_revision(&f.root);
        let mappings: i64 =
            f.db.conn()
                .query_row("SELECT count(*) FROM sync_records", [], |row| row.get(0))
                .unwrap();
        let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
        for role in ["parent", "child"] {
            let (ordinary, _) = producer_engine(&f, role);
            let stats = ordinary.run_sync_cycle().await.unwrap();
            assert_eq!(stats.svn_to_git_count, 0);
            assert_eq!(stats.git_to_svn_count, 0);
        }
        assert_eq!(files(f.target.path()), source);
        assert_eq!(svn_revision(&f.root), revision);
        assert_eq!(
            f.db.conn()
                .query_row("SELECT count(*) FROM sync_records", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            mappings
        );
        assert_eq!(
            f.db.capture_pair_refresh_execution_configuration("child")
                .unwrap()
                .pin(),
            pin.pin()
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn producer_convergence_captured_identity_and_owner_drift_refuse_without_repair() {
        let _serial = SERIAL.lock().await;
        for fault in [
            "configuration",
            "generation",
            "credential",
            "scope",
            "cursor",
            "window",
            "origin",
        ] {
            let f = Fixture::new();
            let engine = f.engine();
            let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
            let ready = ready(&f, &engine).await;
            let (old, current, origin, chain) = unsettled_child_input(&f, &ready);
            let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
            let window = f.db.begin_ordinary_writer_window(Some("child")).unwrap();
            let converge = window
                .prepare_emitted_git_convergence(
                    "child",
                    "feature",
                    &origin,
                    &old,
                    &current,
                    "{\"allowed_paths\":[],\"blocked_patterns\":[]}",
                )
                .unwrap()
                .unwrap();
            match fault {
                "configuration" => {
                    f.db.conn().execute("UPDATE repositories SET poll_interval_secs=poll_interval_secs+1 WHERE id='child'",[]).unwrap();
                }
                "generation" => {
                    let scope = crate::db::repo_scope_identity::repository_scope_uuid(
                        &f.db.conn(),
                        "child",
                    )
                    .unwrap();
                    let generation =
                        crate::echo_receipt_scope::repo_echo_generation(&f.db, "child").unwrap();
                    f.db.set_state(
                        &crate::db::repo_scope_identity::repo_echo_generation_kv_key(&scope),
                        &(generation + 1).to_string(),
                    )
                    .unwrap();
                }
                "credential" => {
                    f.db.set_state("secret_git_token_child", "changed-disposable-token")
                        .unwrap();
                }
                "scope" => {
                    f.db.conn()
                        .execute(
                            "UPDATE repositories SET scope_uuid=?1 WHERE id='child'",
                            [Uuid::new_v4().to_string()],
                        )
                        .unwrap();
                }
                "cursor" => {
                    crate::echo_receipt_scope::write_scoped_last_git_sha_kv(
                        &f.db.conn(),
                        "child",
                        &current,
                        &chrono::Utc::now().to_rfc3339(),
                    )
                    .unwrap();
                }
                "window" => {
                    f.db.conn().execute("UPDATE kv_state SET value=json_set(value,'$.writer_epoch',json_extract(value,'$.writer_epoch')+1) WHERE key LIKE 'pair_refresh_writer_window_v1:document:%'",[]).unwrap();
                }
                "origin" => (),
                _ => unreachable!(),
            }
            let before = storage(&f.db);
            let source = files(f.target.path());
            let observed_origin = if fault == "origin" {
                "different-origin"
            } else {
                &origin
            };
            assert!(
                converge(&current, observed_origin, &chain).is_err(),
                "{fault}"
            );
            assert_eq!(storage(&f.db), before, "{fault}");
            assert_eq!(storage(engine.db()), before, "{fault}");
            assert_eq!(files(f.target.path()), source, "{fault}");
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn producer_convergence_mapping_and_completed_receipt_faults_refuse() {
        let _serial = SERIAL.lock().await;
        for fault in [
            "missing",
            "foreign",
            "duplicate",
            "unscoped",
            "unfinished",
            "remote",
            "incomplete",
        ] {
            let f = Fixture::new();
            let engine = f.engine();
            let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
            let ready = ready(&f, &engine).await;
            let id = child_row(&f).0;
            let (old, current, origin, chain) = unsettled_child_input(&f, &ready);
            let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
            let window = f.db.begin_ordinary_writer_window(Some("child")).unwrap();
            let converge = window
                .prepare_emitted_git_convergence(
                    "child",
                    "feature",
                    &origin,
                    &old,
                    &current,
                    "{\"allowed_paths\":[],\"blocked_patterns\":[]}",
                )
                .unwrap()
                .unwrap();
            match fault {
                "missing" => {
                    f.db.conn()
                        .execute("DELETE FROM sync_records WHERE id=?1", [&id])
                        .unwrap();
                }
                "foreign" => {
                    f.db.conn()
                        .execute(
                            "UPDATE sync_records SET repo_id='parent' WHERE id=?1",
                            [&id],
                        )
                        .unwrap();
                }
                "duplicate" => {
                    let inserted = f.db.conn().execute("INSERT INTO sync_records (id,repo_id,svn_rev,git_sha,direction,author,message,timestamp,synced_at,status) SELECT id||'-duplicate',repo_id,svn_rev,git_sha,direction,author,message,timestamp,synced_at,status FROM sync_records WHERE id=?1",[&id]).unwrap();
                    assert_eq!(inserted, 1, "the duplicate fault must really insert a row");
                    let duplicate: (String, String, i64, String, String) = f.db.conn().query_row(
                        "SELECT repo_id,git_sha,svn_rev,direction,status FROM sync_records WHERE id=?1",
                        [format!("{id}-duplicate")],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
                    ).unwrap();
                    assert_eq!(
                        duplicate,
                        (
                            "child".into(),
                            current.clone(),
                            f.db.conn()
                                .query_row(
                                    "SELECT svn_rev FROM sync_records WHERE id=?1",
                                    [&id],
                                    |row| row.get::<_, i64>(0)
                                )
                                .unwrap(),
                            "svn_to_git".into(),
                            "applied".into()
                        )
                    );
                    assert_eq!(f.db.conn().query_row(
                        "SELECT count(*) FROM sync_records WHERE repo_id='child' AND git_sha=?1 AND svn_rev=?2 AND direction='svn_to_git' AND status='applied'",
                        rusqlite::params![current, duplicate.2], |row| row.get::<_, i64>(0),
                    ).unwrap(), 2);
                }
                "unscoped" => {
                    f.db.conn()
                        .execute("UPDATE sync_records SET repo_id=NULL WHERE id=?1", [&id])
                        .unwrap();
                }
                "unfinished" => {
                    f.db.conn().execute("UPDATE kv_state SET value=json_set(value,'$.state','running') WHERE key LIKE 'svn_to_git_push_v1:document:%' AND json_extract(value,'$.repo_id')='child' AND json_extract(value,'$.intended_local_git_sha')=?1",[&current]).unwrap();
                }
                "remote" | "incomplete" => (),
                _ => unreachable!(),
            }
            let before = storage(&f.db);
            let observed = if fault == "remote" { &old } else { &current };
            let selected = if fault == "incomplete" {
                &[][..]
            } else {
                &chain[..]
            };
            assert!(converge(observed, &origin, selected).is_err(), "{fault}");
            assert_eq!(storage(&f.db), before, "{fault}");
            assert_eq!(storage(engine.db()), before, "{fault}");
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn producer_convergence_transaction_failure_rolls_back_cursor_and_mirrors() {
        let _serial = SERIAL.lock().await;
        let f = Fixture::new();
        let engine = f.engine();
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let ready = ready(&f, &engine).await;
        let (old, current, origin, chain) = unsettled_child_input(&f, &ready);
        let proof_key = emitted_frontier_key(&f, "child", &current);
        // Negative fault fixture for an uncommitted transition, not a new
        // positive mapping. The real writer must insert the proof atomically.
        f.db.delete_kv_state(&proof_key).unwrap();
        let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
        let window = f.db.begin_ordinary_writer_window(Some("child")).unwrap();
        let converge = window
            .prepare_emitted_git_convergence(
                "child",
                "feature",
                &origin,
                &old,
                &current,
                "{\"allowed_paths\":[],\"blocked_patterns\":[]}",
            )
            .unwrap()
            .unwrap();
        let scope =
            crate::db::repo_scope_identity::repository_scope_uuid(&f.db.conn(), "child").unwrap();
        let key = crate::db::repo_scope_identity::last_git_sha_kv_key(&scope);
        // The real canonical UPDATE reaches this trigger; SQLite must undo
        // that write and all transaction work when the injected error occurs.
        f.db.conn().execute_batch(&format!(
            "CREATE TEMP TRIGGER convergence_atomic_failure AFTER UPDATE ON kv_state WHEN NEW.key='{key}' AND NEW.value='{current}' BEGIN SELECT RAISE(ABORT,'convergence atomic fault'); END;"
        )).unwrap();
        let before = storage(&f.db);
        let error = converge(&current, &origin, &chain).unwrap_err().to_string();
        assert!(error.contains("convergence atomic fault"), "{error}");
        assert_eq!(storage(&f.db), before);
        assert_eq!(storage(engine.db()), before);
        assert_eq!(f.db.get_state(&proof_key).unwrap(), None);
        assert!(!retained_frontier(&f, &current).unwrap());
        assert_eq!(
            crate::echo_receipt_scope::read_team_inbound_git_checkpoint_kv(&f.db, "child")
                .unwrap()
                .as_deref(),
            Some(old.as_str())
        );
        assert_eq!(f.db.conn().query_row("SELECT count(*) FROM sqlite_temp_master WHERE type='trigger' AND name='convergence_atomic_failure'",[],|row|row.get::<_,i64>(0)).unwrap(),1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn producer_convergence_dropped_observer_recovers_known_range_without_republication() {
        let _serial = SERIAL.lock().await;
        let f = Fixture::new();
        let engine = f.engine();
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let ready = ready(&f, &engine).await;
        let (old, current, origin, _) = unsettled_child_input(&f, &ready);
        let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
        {
            let window = f.db.begin_ordinary_writer_window(Some("child")).unwrap();
            let before = storage(&f.db);
            let captured = window
                .prepare_emitted_git_convergence(
                    "child",
                    "feature",
                    &origin,
                    &old,
                    &current,
                    "{\"allowed_paths\":[],\"blocked_patterns\":[]}",
                )
                .unwrap()
                .unwrap();
            let before = expected_after_only_ordinary_lease_renewal(&f.db, &before);
            drop(captured);
            assert_eq!(storage(&f.db), before);
        }
        assert_eq!(
            crate::echo_receipt_scope::read_team_inbound_git_checkpoint_kv(&f.db, "child")
                .unwrap()
                .as_deref(),
            Some(old.as_str())
        );
        let source = files(f.target.path());
        let revision = svn_revision(&f.root);
        let (ordinary, _) = producer_engine(&f, "child");
        let stats = ordinary.run_sync_cycle().await.unwrap();
        assert_eq!(stats.svn_to_git_count, 0);
        assert_eq!(stats.git_to_svn_count, 0);
        assert_eq!(files(f.target.path()), source);
        assert_eq!(svn_revision(&f.root), revision);
        assert_eq!(
            crate::echo_receipt_scope::read_team_inbound_git_checkpoint_kv(&f.db, "child")
                .unwrap()
                .as_deref(),
            Some(current.as_str())
        );
        f.db.capture_pair_refresh_execution_configuration("child")
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn producer_convergence_missing_old_authority_and_unrelated_root_are_not_adopted() {
        let _serial = SERIAL.lock().await;
        // Existing ordinary in-memory support is not new convergence authority.
        // Disregard only the established writer lease's permitted renewal when
        // checking no additional storage/proof/cursor mutation on enrolled calls.
        let retained_without_lease = |db: &Database| {
            let mut snapshot = storage(db);
            snapshot.get_mut("kv_state").unwrap().retain(|row| {
                !matches!(&row[0], rusqlite::types::Value::Text(key) if key.starts_with("writer_fence_v1:"))
            });
            snapshot
        };
        {
            let memory = Database::in_memory().unwrap();
            memory.initialize().unwrap();
            memory.conn().execute(
                "INSERT INTO repositories (id,name,svn_url,svn_branch,svn_username,git_provider,git_api_url,git_repo,git_branch,sync_mode,poll_interval_secs,lfs_threshold_mb,auto_merge,enabled,created_at,updated_at,last_svn_rev,last_git_sha,sync_status,total_syncs,total_errors) VALUES ('compat-root','compat','file:///unused','','','local','','unused','main','team',5,0,0,1,'t','t',2,?1,'idle',0,0)",
                ["b".repeat(40)],
            ).unwrap();
            crate::db::repo_scope_identity::migrate_v13_scope_uuid(&memory.conn()).unwrap();
            assert!(memory.opened_file().is_err());
            assert_eq!(
                crate::writer_fence::ordinary_current_epoch(&memory.conn()).unwrap(),
                None
            );
            {
                let window = memory
                    .begin_ordinary_writer_window(Some("compat-root"))
                    .unwrap();
                let before = storage(&memory);
                assert!(window
                    .prepare_emitted_git_convergence(
                        "compat-root",
                        "main",
                        "file:///unused",
                        &"b".repeat(40),
                        &"b".repeat(40),
                        "{\"allowed_paths\":[],\"blocked_patterns\":[]}",
                    )
                    .unwrap()
                    .is_none());
                assert_eq!(storage(&memory), before);
            }
            let scratch = tempfile::tempdir().unwrap();
            let _fence = crate::writer_fence::claim(&memory, scratch.path()).unwrap();
            assert!(crate::writer_fence::ordinary_current_epoch(&memory.conn())
                .unwrap()
                .is_some());
            let window = memory
                .begin_ordinary_writer_window(Some("compat-root"))
                .unwrap();
            let before = retained_without_lease(&memory);
            assert!(window
                .prepare_emitted_git_convergence(
                    "compat-root",
                    "main",
                    "file:///unused",
                    &"b".repeat(40),
                    &"b".repeat(40),
                    "{\"allowed_paths\":[],\"blocked_patterns\":[]}",
                )
                .unwrap()
                .is_none());
            assert_eq!(retained_without_lease(&memory), before);
            assert!(memory.opened_file().is_err());
        }
        let f = Fixture::new();
        let engine = f.engine();
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let ready = ready(&f, &engine).await;
        let (old, current, origin, _) = unsettled_child_input(&f, &ready);
        {
            let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
            let window = f.db.begin_ordinary_writer_window(Some("child")).unwrap();
            let before = retained_without_lease(&f.db);
            let capability = window
                .prepare_emitted_git_convergence(
                    "child",
                    "feature",
                    &origin,
                    &old,
                    &current,
                    "{\"allowed_paths\":[],\"blocked_patterns\":[]}",
                )
                .unwrap();
            assert!(
                capability.is_some(),
                "eligible valid native pair must remain applicable"
            );
            drop(capability);
            assert_eq!(retained_without_lease(&f.db), before);
            // A real second task-owned name breaks the unchanged single-link
            // native guard. No fabricated attestation or OS-function adapter.
            let alias = f.temp.path().join("pc1-native-alias.db");
            std::fs::hard_link(f.temp.path().join("reposync.db"), &alias).unwrap();
            let before = retained_without_lease(&f.db);
            let result = window.prepare_emitted_git_convergence(
                "child",
                "feature",
                &origin,
                &old,
                &current,
                "{\"allowed_paths\":[],\"blocked_patterns\":[]}",
            );
            std::fs::remove_file(&alias).unwrap();
            let error = match result {
                Err(error) => error.to_string(),
                Ok(_) => panic!(
                    "applicable native identity failure must not become a decline or capability"
                ),
            };
            assert!(error.contains("owned singly linked native file"), "{error}");
            assert_eq!(retained_without_lease(&f.db), before);
            assert_eq!(retained_without_lease(engine.db()), before);
        }
        let scope =
            crate::db::repo_scope_identity::repository_scope_uuid(&f.db.conn(), "child").unwrap();
        f.db.delete_kv_state(&crate::db::repo_scope_identity::last_git_sha_kv_key(&scope))
            .unwrap();
        f.db.delete_kv_state("last_git_sha_child").unwrap();
        let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
        let window = f.db.begin_ordinary_writer_window(Some("child")).unwrap();
        let before = storage(&f.db);
        assert!(window
            .prepare_emitted_git_convergence(
                "child",
                "feature",
                &origin,
                &current,
                &current,
                "{\"allowed_paths\":[],\"blocked_patterns\":[]}",
            )
            .is_err());
        assert_eq!(storage(&f.db), before);
        // Controlled topology fault makes this an unrelated ordinary root.
        // The optional convergence route must grant no capability or writes.
        f.db.conn()
            .execute(
                "UPDATE repositories SET parent_id=NULL WHERE id='child'",
                [],
            )
            .unwrap();
        let before = storage(&f.db);
        assert!(window
            .prepare_emitted_git_convergence(
                "child",
                "feature",
                &origin,
                &current,
                &current,
                "{\"allowed_paths\":[],\"blocked_patterns\":[]}",
            )
            .unwrap()
            .is_none());
        let before = expected_after_only_ordinary_lease_renewal(&f.db, &before);
        assert_eq!(storage(&f.db), before);
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct ConvergenceCheckpointState {
        columns: (String, i64),
        copies: BTreeMap<String, Option<(String, String)>>,
    }

    // Read current coordinates directly: a retained history block must not
    // prevent observing that refusal left checkpoint columns/copies unchanged.
    fn convergence_checkpoint_state(f: &Fixture) -> ConvergenceCheckpointState {
        let columns =
            f.db.conn()
                .query_row(
                    "SELECT last_git_sha,last_svn_rev FROM repositories WHERE id='child'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
        let scope =
            crate::db::repo_scope_identity::repository_scope_uuid(&f.db.conn(), "child").unwrap();
        let keys = [
            crate::db::repo_scope_identity::last_git_sha_kv_key(&scope),
            crate::db::repo_scope_identity::legacy_last_git_sha_kv_key("child"),
            "last_svn_rev_child".to_owned(),
        ];
        ConvergenceCheckpointState {
            columns,
            copies: keys
                .into_iter()
                .map(|key| {
                    let value = saved_kv(f, &key);
                    (key, value)
                })
                .collect(),
        }
    }

    fn emitted_frontier_key(f: &Fixture, id: &str, sha: &str) -> String {
        let scope =
            crate::db::repo_scope_identity::repository_scope_uuid(&f.db.conn(), id).unwrap();
        let generation = crate::echo_receipt_scope::repo_echo_generation(&f.db, id).unwrap();
        format!("handled_git_emitted_frontier_v1:{scope}:g{generation}:{sha}")
    }

    fn retained_frontier(f: &Fixture, sha: &str) -> Result<bool, crate::errors::DatabaseError> {
        let conn = f.db.conn();
        crate::db::ordinary_writer_windows::OrdinaryWriterWindow::verified_emitted_git_frontier(
            &conn,
            &f.db,
            "child",
            sha,
            "{\"allowed_paths\":[],\"blocked_patterns\":[]}",
        )
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn producer_convergence_proof_survives_reopen_and_another_real_transition() {
        let _serial = SERIAL.lock().await;
        let f = Fixture::new();
        let engine = f.engine();
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let ready = ready(&f, &engine).await;
        let (_, first) = f.db.get_repo_watermark("child").unwrap();
        assert!(retained_frontier(&f, &first).unwrap());
        let key = emitted_frontier_key(&f, "child", &first);
        let original_proof = f.db.get_state(&key).unwrap().unwrap();
        let reopened = Database::new(f.temp.path().join("reposync.db")).unwrap();
        {
            let conn = reopened.conn();
            assert!(crate::db::ordinary_writer_windows::OrdinaryWriterWindow::verified_emitted_git_frontier(
                &conn,&reopened,"child",&first,"{\"allowed_paths\":[],\"blocked_patterns\":[]}",
            ).unwrap());
        }
        let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
        let (ordinary, _) = producer_engine(&f, "child");
        let quiet = ordinary.run_sync_cycle().await.unwrap();
        assert_eq!(quiet.svn_to_git_count, 0);
        assert_eq!(quiet.git_to_svn_count, 0);
        std::fs::write(
            ready.child_wc.join("file.txt"),
            b"second genuine child work\n",
        )
        .unwrap();
        svn(
            &[
                "commit",
                "--username",
                "m2d-fixture-author",
                "--no-auth-cache",
                "-m",
                "second genuine child source",
            ],
            Some(&ready.child_wc),
        );
        let revision = require_named_source_revision(&f, "branches/f").await;
        let advanced = ordinary.run_sync_cycle().await.unwrap();
        assert_eq!(advanced.svn_to_git_count, 1);
        assert_eq!(advanced.git_to_svn_count, 0);
        let (handled, second) = f.db.get_repo_watermark("child").unwrap();
        assert_eq!(handled, revision);
        assert_ne!(first, second);
        assert!(retained_frontier(&f, &second).unwrap());
        assert_eq!(
            f.db.get_state(&key).unwrap().as_deref(),
            Some(original_proof.as_str())
        );
        let second_proof: serde_json::Value = serde_json::from_str(
            &f.db
                .get_state(&emitted_frontier_key(&f, "child", &second))
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            second_proof["payload"]["old_authority"]["kind"],
            "convergence"
        );
        assert_eq!(second_proof["payload"]["old_frontier"], first);
        let source = files(f.target.path());
        let svn_before = svn_revision(&f.root);
        let quiet = ordinary.run_sync_cycle().await.unwrap();
        assert_eq!(quiet.svn_to_git_count, 0);
        assert_eq!(quiet.git_to_svn_count, 0);
        assert_eq!(files(f.target.path()), source);
        assert_eq!(svn_revision(&f.root), svn_before);
        f.db.capture_pair_refresh_execution_configuration("child")
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn producer_convergence_retained_payload_and_required_proof_refuse_tampering() {
        let _serial = SERIAL.lock().await;
        let f = Fixture::new();
        let engine = f.engine();
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let _ready = ready(&f, &engine).await;
        let (_, current) = f.db.get_repo_watermark("child").unwrap();
        let key = emitted_frontier_key(&f, "child", &current);
        let original = f.db.get_state(&key).unwrap().unwrap();
        assert!(retained_frontier(&f, &current).unwrap());
        // Invalid field changes are MAC-negative tests, not claims that every
        // downstream semantic branch was independently reached.
        for field in [
            "version",
            "outcome",
            "repo_id",
            "scope_uuid",
            "generation",
            "configuration_digest",
            "projection",
            "origin_digest",
            "native_database",
            "writer_epoch",
            "new_git_sha",
            "new_git_tree",
            "new_svn_revision",
            "old_authority",
        ] {
            let mut record: serde_json::Value = serde_json::from_str(&original).unwrap();
            record["payload"][field] = serde_json::json!("wrong");
            f.db.set_state(&key, &record.to_string()).unwrap();
            let before = storage(&f.db);
            assert!(retained_frontier(&f, &current).is_err(), "{field}");
            assert_eq!(storage(&f.db), before, "{field}");
            assert_eq!(storage(engine.db()), before, "{field}");
            f.db.set_state(&key, &original).unwrap();
            assert!(retained_frontier(&f, &current).unwrap());
        }
        // Valid MAC, altered current configuration reaches invalidation.
        f.db.conn()
            .execute(
                "UPDATE repositories SET poll_interval_secs=poll_interval_secs+1 WHERE id='child'",
                [],
            )
            .unwrap();
        assert!(retained_frontier(&f, &current)
            .unwrap_err()
            .to_string()
            .contains("stale or foreign"));
        f.db.conn()
            .execute(
                "UPDATE repositories SET poll_interval_secs=poll_interval_secs-1 WHERE id='child'",
                [],
            )
            .unwrap();
        assert!(retained_frontier(&f, &current).unwrap());
        // A different actual database handle cannot supply the native context
        // for this connection, even when the proof and key bytes are untouched.
        let other = Database::new(f.temp.path().join("other-native.db")).unwrap();
        other.initialize().unwrap();
        {
            let conn = f.db.conn();
            assert!(crate::db::ordinary_writer_windows::OrdinaryWriterWindow::verified_emitted_git_frontier(
                &conn,&other,"child",&current,"{\"allowed_paths\":[],\"blocked_patterns\":[]}",
            ).is_err());
        }
        let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
        f.db.delete_kv_state(&key).unwrap();
        let before = storage(&f.db);
        assert!(!retained_frontier(&f, &current).unwrap());
        assert_eq!(storage(&f.db), before);
        let source = files(f.target.path());
        let row = child_row(&f);
        let checkpoint_before = convergence_checkpoint_state(&f);
        let (ordinary, _) = producer_engine(&f, "child");
        assert!(
            matches!(ordinary.run_sync_cycle().await,Err(crate::errors::SyncError::HistoryBlocked {reason,..}) if reason=="ambiguous_checkpoint")
        );
        // The ordinary refusal deliberately retained a history block. Keep
        // strict refresh capture refused; inspect the already pinned row itself.
        let observed: (String, String, i64) = f.db.conn().query_row(
            "SELECT id,git_sha,svn_rev FROM sync_records WHERE id=?1 AND repo_id='child' AND direction='svn_to_git' AND status='applied'",
            [&row.0], |record| Ok((record.get(0)?, record.get(1)?, record.get(2)?)),
        ).unwrap();
        assert_eq!(observed, row);
        assert_eq!(convergence_checkpoint_state(&f), checkpoint_before);
        let refusal = match f.db.capture_pair_refresh_execution_configuration("child") {
            Ok(_) => panic!("retained history block must still refuse refresh capture"),
            Err(error) => error.to_string(),
        };
        assert!(
            refusal.contains("history inspection must be quiet"),
            "{refusal}"
        );
        assert_eq!(files(f.target.path()), source);
        assert_eq!(f.db.get_state(&key).unwrap(), None);
        // Controlled counterexamples: keeping the old row, absent proof and
        // retained history error must not hide a changed current coordinate.
        let refused_state = storage(&f.db);
        let pinned_row = sync_row(&f, &row.0);
        let history_key =
            crate::history_inspect::history_block_key_for_repo(&f.db, "child").unwrap();
        let history = saved_kv(&f, &history_key);
        assert!(history.is_some());
        let scope =
            crate::db::repo_scope_identity::repository_scope_uuid(&f.db.conn(), "child").unwrap();
        let canonical = crate::db::repo_scope_identity::last_git_sha_kv_key(&scope);
        let legacy = crate::db::repo_scope_identity::legacy_last_git_sha_kv_key("child");
        let other = f.baseline.to_string();
        assert_ne!(other, checkpoint_before.columns.0);
        for fault in [
            "repository-git",
            "repository-svn",
            "canonical-git",
            "legacy-git",
            "svn-copy",
            "coherent-git",
        ] {
            match fault {
                "repository-git" => {
                    f.db.conn()
                        .execute(
                            "UPDATE repositories SET last_git_sha=?1 WHERE id='child'",
                            [&other],
                        )
                        .unwrap();
                }
                "repository-svn" => {
                    f.db.conn()
                        .execute(
                            "UPDATE repositories SET last_svn_rev=last_svn_rev+1 WHERE id='child'",
                            [],
                        )
                        .unwrap();
                }
                "coherent-git" => {
                    f.db.conn()
                        .execute(
                            "UPDATE repositories SET last_git_sha=?1 WHERE id='child'",
                            [&other],
                        )
                        .unwrap();
                    f.db.set_state(&canonical, &other).unwrap();
                    if checkpoint_before.copies[&legacy].is_some() {
                        f.db.set_state(&legacy, &other).unwrap();
                    }
                }
                "canonical-git" => f.db.set_state(&canonical, &other).unwrap(),
                "legacy-git" => f.db.set_state(&legacy, &other).unwrap(),
                "svn-copy" => {
                    f.db.set_state(
                        "last_svn_rev_child",
                        &(checkpoint_before.columns.1 + 1).to_string(),
                    )
                    .unwrap()
                }
                _ => unreachable!(),
            }
            assert_ne!(
                convergence_checkpoint_state(&f),
                checkpoint_before,
                "{fault} must be detected"
            );
            assert_eq!(sync_row(&f, &row.0), pinned_row, "{fault}");
            assert_eq!(saved_kv(&f, &history_key), history, "{fault}");
            assert_eq!(f.db.get_state(&key).unwrap(), None, "{fault}");
            if fault == "coherent-git" {
                let still_refused = match f.db.capture_pair_refresh_execution_configuration("child")
                {
                    Ok(_) => {
                        panic!("coherent changed coordinates cannot erase the history refusal")
                    }
                    Err(error) => error.to_string(),
                };
                assert!(
                    still_refused.contains("history inspection must be quiet"),
                    "{still_refused}"
                );
            }
            f.db.conn()
                .execute(
                    "UPDATE repositories SET last_git_sha=?1,last_svn_rev=?2 WHERE id='child'",
                    rusqlite::params![checkpoint_before.columns.0, checkpoint_before.columns.1],
                )
                .unwrap();
            for (name, value) in &checkpoint_before.copies {
                restore_kv(&f, name, value.clone());
            }
            assert_eq!(
                convergence_checkpoint_state(&f),
                checkpoint_before,
                "{fault}"
            );
            assert_eq!(storage(&f.db), refused_state, "{fault}");
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn producer_convergence_retained_dependencies_and_generation_reset_fail_closed() {
        let _serial = SERIAL.lock().await;
        let f = Fixture::new();
        let engine = f.engine();
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let ready = ready(&f, &engine).await;
        let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
        let (_, current) = f.db.get_repo_watermark("child").unwrap();
        let key = emitted_frontier_key(&f, "child", &current);
        let original = f.db.get_state(&key).unwrap().unwrap();
        let record: serde_json::Value = serde_json::from_str(&original).unwrap();
        let producer_key = record["payload"]["chain"][0]["producer_key"]
            .as_str()
            .unwrap();
        let producer = f.db.get_state(producer_key).unwrap().unwrap();
        f.db.delete_kv_state(producer_key).unwrap();
        let before = storage(&f.db);
        assert!(retained_frontier(&f, &current)
            .unwrap_err()
            .to_string()
            .contains("producer dependency changed or missing"));
        assert_eq!(storage(&f.db), before);
        f.db.set_state(producer_key, &producer).unwrap();
        assert!(retained_frontier(&f, &current).unwrap());
        let row_id = child_row(&f).0;
        let row = sync_row(&f, &row_id);
        f.db.conn()
            .execute("DELETE FROM sync_records WHERE id=?1", [&row_id])
            .unwrap();
        let before = storage(&f.db);
        assert!(retained_frontier(&f, &current)
            .unwrap_err()
            .to_string()
            .contains("correspondence changed or missing"));
        assert_eq!(storage(&f.db), before);
        restore_sync_row(&f, &row);
        let old_id = ready.history.id.to_string();
        let old_row = sync_row(&f, &old_id);
        f.db.conn()
            .execute("DELETE FROM sync_records WHERE id=?1", [&old_id])
            .unwrap();
        let before = storage(&f.db);
        assert!(retained_frontier(&f, &current)
            .unwrap_err()
            .to_string()
            .contains("old applied authority dependency missing or changed"));
        assert_eq!(storage(&f.db), before);
        let source_before = files(f.target.path());
        let checkpoint_before = f.db.get_repo_watermark("child").unwrap();
        let (ordinary, _) = producer_engine(&f, "child");
        let rejected = ordinary.run_sync_cycle().await.unwrap_err().to_string();
        assert!(
            rejected.contains("old applied authority dependency missing or changed"),
            "{rejected}"
        );
        assert_eq!(files(f.target.path()), source_before);
        assert_eq!(f.db.get_repo_watermark("child").unwrap(), checkpoint_before);
        // Losing both this proof and old disposable outbound diagnostics
        // must not reclassify C as a legacy import: signed refresh history R
        // is still present for the same incarnation/generation.
        f.db.delete_kv_state(&key).unwrap();
        assert!(!retained_frontier(&f, &current).unwrap());
        let (ordinary, _) = producer_engine(&f, "child");
        assert!(
            matches!(ordinary.run_sync_cycle().await,Err(crate::errors::SyncError::HistoryBlocked {reason,..}) if reason=="ambiguous_checkpoint")
        );
        assert_eq!(f.db.get_repo_watermark("child").unwrap(), checkpoint_before);
        assert_eq!(files(f.target.path()), source_before);
        assert_eq!(f.db.get_state(&key).unwrap(), None);
        f.db.set_state(&key, &original).unwrap();
        restore_sync_row(&f, &old_row);
        assert!(retained_frontier(&f, &current).unwrap());
        let prior_generation =
            crate::echo_receipt_scope::repo_echo_generation(&f.db, "child").unwrap();
        assert_eq!(
            crate::echo_receipt_scope::bump_repo_echo_generation(&f.db, "child").unwrap(),
            prior_generation + 1
        );
        assert!(!retained_frontier(&f, &current).unwrap());
        assert_eq!(
            crate::echo_receipt_scope::read_team_inbound_git_checkpoint_kv(&f.db, "child")
                .unwrap()
                .as_deref(),
            Some(ready.history.pins.git.prepared_sha.as_str())
        );
        let source = files(f.target.path());
        let revision = svn_revision(&f.root);
        let (ordinary, _) = producer_engine(&f, "child");
        let error = ordinary.run_sync_cycle().await.unwrap_err().to_string();
        assert!(error.contains("stale emitted proof"), "{error}");
        assert_eq!(files(f.target.path()), source);
        assert_eq!(svn_revision(&f.root), revision);
        assert_eq!(
            f.db.get_state(&key).unwrap().as_deref(),
            Some(original.as_str())
        );
        assert_eq!(
            f.db.get_state(&emitted_frontier_key(&f, "child", &current))
                .unwrap(),
            None
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn producer_convergence_proof_insertion_failure_rolls_back_entire_transition() {
        let _serial = SERIAL.lock().await;
        let f = Fixture::new();
        let engine = f.engine();
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let ready = ready(&f, &engine).await;
        let (old, current, origin, chain) = unsettled_child_input(&f, &ready);
        let key = emitted_frontier_key(&f, "child", &current);
        // Controlled uncommitted-transition model only: retain the genuine
        // producer/mapping dependencies, remove this already committed proof
        // so the real writer's INSERT (not a fake positive record) is exercised.
        f.db.delete_kv_state(&key).unwrap();
        let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
        let window = f.db.begin_ordinary_writer_window(Some("child")).unwrap();
        let converge = window
            .prepare_emitted_git_convergence(
                "child",
                "feature",
                &origin,
                &old,
                &current,
                "{\"allowed_paths\":[],\"blocked_patterns\":[]}",
            )
            .unwrap()
            .unwrap();
        f.db.conn().execute_batch(&format!("CREATE TEMP TRIGGER emitted_proof_insert_failure AFTER INSERT ON kv_state WHEN NEW.key='{key}' BEGIN SELECT RAISE(ABORT,'proof insertion fault'); END;")).unwrap();
        let before = storage(&f.db);
        let error = converge(&current, &origin, &chain).unwrap_err().to_string();
        assert!(error.contains("proof insertion fault"), "{error}");
        assert_eq!(storage(&f.db), before);
        assert_eq!(storage(engine.db()), before);
        assert_eq!(f.db.get_state(&key).unwrap(), None);
        assert!(!retained_frontier(&f, &current).unwrap());
        assert_eq!(
            crate::echo_receipt_scope::read_team_inbound_git_checkpoint_kv(&f.db, "child")
                .unwrap()
                .as_deref(),
            Some(old.as_str())
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn producer_convergence_pending_opposite_work_never_advances_as_an_echo() {
        let _serial = SERIAL.lock().await;
        let f = Fixture::new();
        let engine = f.engine();
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let ready = ready(&f, &engine).await;
        let (revision, handled) = f.db.get_repo_watermark("child").unwrap();
        let proof_key = emitted_frontier_key(&f, "child", &handled);
        let proof = f.db.get_state(&proof_key).unwrap().unwrap();
        let pending = commit(
            &f.target,
            b"independent pending Git work\n",
            &[Oid::from_str(&handled).unwrap()],
        );
        f.target
            .reference(
                "refs/heads/feature",
                pending,
                true,
                "disposable pending opposite work",
            )
            .unwrap();
        std::fs::write(
            ready.child_wc.join("file.txt"),
            b"independent pending SVN work\n",
        )
        .unwrap();
        svn(
            &[
                "commit",
                "--username",
                "m2d-fixture-author",
                "--no-auth-cache",
                "-m",
                "pending opposite SVN work",
            ],
            Some(&ready.child_wc),
        );
        let pending_revision = require_named_source_revision(&f, "branches/f").await;
        assert!(pending_revision > revision);
        let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
        let (ordinary, _) = producer_engine(&f, "child");
        assert!(matches!(
            ordinary.run_sync_cycle().await,
            Err(crate::errors::SyncError::UnresolvableConflict { .. })
        ));
        assert_eq!(
            f.db.get_repo_watermark("child").unwrap(),
            (revision, handled.clone())
        );
        assert_eq!(
            crate::echo_receipt_scope::read_team_inbound_git_checkpoint_kv(&f.db, "child")
                .unwrap()
                .as_deref(),
            Some(handled.as_str())
        );
        assert_eq!(
            f.target
                .find_reference("refs/heads/feature")
                .unwrap()
                .target(),
            Some(pending)
        );
        assert_eq!(svn_revision(&f.root), pending_revision);
        assert_eq!(
            f.db.get_state(&proof_key).unwrap().as_deref(),
            Some(proof.as_str())
        );
        assert_eq!(
            f.db.get_state(&emitted_frontier_key(&f, "child", &pending.to_string()))
                .unwrap(),
            None
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn producer_convergence_past_proof_does_not_repair_missing_cursor_copies() {
        let _serial = SERIAL.lock().await;
        for shape in ["kv-only", "column-only"] {
            let f = Fixture::new();
            let engine = f.engine();
            let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
            let _ready = ready(&f, &engine).await;
            let (_, current) = f.db.get_repo_watermark("child").unwrap();
            let proof_key = emitted_frontier_key(&f, "child", &current);
            let proof = f.db.get_state(&proof_key).unwrap().unwrap();
            if shape == "kv-only" {
                f.db.conn()
                    .execute(
                        "UPDATE repositories SET last_git_sha='' WHERE id='child'",
                        [],
                    )
                    .unwrap();
            } else {
                let scope =
                    crate::db::repo_scope_identity::repository_scope_uuid(&f.db.conn(), "child")
                        .unwrap();
                f.db.delete_kv_state(&crate::db::repo_scope_identity::last_git_sha_kv_key(&scope))
                    .unwrap();
                f.db.delete_kv_state("last_git_sha_child").unwrap();
            }
            // This local proof is still a fact about past handled work. Its
            // validation cannot manufacture a current cursor or a ready pair.
            let before = storage(&f.db);
            assert!(retained_frontier(&f, &current).unwrap(), "{shape}");
            assert_eq!(storage(&f.db), before, "{shape}");
            assert!(f
                .db
                .capture_pair_refresh_execution_configuration("child")
                .is_err());
            let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
            let source = files(f.target.path());
            let revision = svn_revision(&f.root);
            let watermark = f.db.get_repo_watermark("child").unwrap();
            let inbound =
                crate::echo_receipt_scope::read_team_inbound_git_checkpoint_kv(&f.db, "child")
                    .unwrap();
            let (ordinary, _) = producer_engine(&f, "child");
            assert!(ordinary.run_sync_cycle().await.is_err(), "{shape}");
            assert_eq!(
                f.db.get_repo_watermark("child").unwrap(),
                watermark,
                "{shape}"
            );
            assert_eq!(
                crate::echo_receipt_scope::read_team_inbound_git_checkpoint_kv(&f.db, "child")
                    .unwrap(),
                inbound,
                "{shape}"
            );
            assert_eq!(
                f.db.get_state(&proof_key).unwrap().as_deref(),
                Some(proof.as_str())
            );
            assert_eq!(files(f.target.path()), source);
            assert_eq!(svn_revision(&f.root), revision);
        }
    }
}

/// New source-only applicability regressions. Positive setup reuses the exact
/// qualified M2d ordinary producers and M2c genuine-receipt-chain recipe. No
/// analyzer input in these tests is an execution/approval capability.
#[cfg(test)]
mod changed_child_applicability_review {
    use super::*;
    use crate::db::pair_refresh_operations::{PairRefreshOperation, PairRefreshPhase};
    use crate::pair_refresh::preparation::{
        inspect_changed_child_applicable_graph, inspect_changed_child_review,
        ChangedChildApplicabilityRoles, ChangedChildDisposition, ChangedChildEntry,
        ChangedChildGitInput, ChangedChildReviewContext, ChangedChildReviewInputs,
        CHANGED_CHILD_APPLICABILITY_POLICY, CHANGED_CHILD_REVIEW_POLICY,
    };
    use std::collections::BTreeMap;

    type Storage = BTreeMap<String, Vec<Vec<rusqlite::types::Value>>>;

    fn storage(db: &Database) -> Storage {
        use rusqlite::types::Value;
        let conn = db.conn();
        let mut snapshot = BTreeMap::new();
        let schema = conn
            .prepare("SELECT type,name,tbl_name,rootpage,sql FROM sqlite_master ORDER BY type,name")
            .unwrap()
            .query_map([], |row| {
                (0..5)
                    .map(|i| row.get::<_, Value>(i))
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        snapshot.insert("schema".into(), schema);
        let tables = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        for table in tables {
            let sql = format!("SELECT * FROM \"{}\"", table.replace('"', "\"\""));
            let mut query = conn.prepare(&sql).unwrap();
            let columns = query.column_count();
            let mut values = query
                .query_map([], |row| {
                    (0..columns)
                        .map(|i| row.get::<_, Value>(i))
                        .collect::<rusqlite::Result<Vec<_>>>()
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            values.sort_by_cached_key(|row| format!("{row:?}"));
            snapshot.insert(table, values);
        }
        snapshot
    }

    fn files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        fn collect(root: &Path, path: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
            for entry in std::fs::read_dir(path).unwrap() {
                let entry = entry.unwrap();
                let kind = entry.file_type().unwrap();
                assert!(!kind.is_symlink());
                if kind.is_dir() {
                    collect(root, &entry.path(), out);
                } else {
                    assert!(kind.is_file());
                    out.insert(
                        entry.path().strip_prefix(root).unwrap().to_owned(),
                        std::fs::read(entry.path()).unwrap(),
                    );
                }
            }
        }
        let mut out = BTreeMap::new();
        collect(root, root, &mut out);
        out
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

    async fn completed(
        f: &Fixture,
        engine: &crate::sync_engine::SyncEngine,
        request: &str,
    ) -> PairRefreshOperation {
        let (actor, _workspace, value) = prepared(f, engine).await;
        let approved = actor.prepared_approval_digest(engine, &value).unwrap();
        let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
        let op = super::super::super::execution::complete_owned_refresh_with_client(
            engine,
            &actor,
            &value,
            request,
            &approved,
            &busy,
            f.client.clone(),
        )
        .await
        .unwrap();
        assert_eq!(op.phase, PairRefreshPhase::Completed);
        assert_eq!(f.db.pair_refresh_operation(op.id).unwrap(), op);
        op
    }

    // Use the same native DB and disposable bare repository, with a distinct
    // ordinary bridge per role. Local Git transport here avoids a new CA policy;
    // the correspondence factory still uses registered HTTPS observers/client.
    fn producer_engine(f: &Fixture, id: &str) -> (crate::sync_engine::SyncEngine, PathBuf) {
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
        config.daemon.data_dir = f.temp.path().to_owned();
        let svn_branch = if id == "child" { "branches/f" } else { "trunk" };
        let svn_url = format!("{}/{svn_branch}", f.root);
        config.svn.url = svn_url.clone();
        config.svn.layout = crate::config::SvnLayout::Custom;
        config.svn.password = Some(String::new());
        config.github.default_branch = branch.to_owned();
        config.github.token = Some(String::new());
        config.sync.auto_merge = false;
        let mut engine = crate::sync_engine::SyncEngine::new(
            config,
            Database::new(f.temp.path().join("reposync.db")).unwrap(),
            crate::svn::SvnClient::new(svn_url, "", ""),
            crate::git::GitClient::new(&path).unwrap(),
            std::sync::Arc::new(
                crate::identity::mapper::IdentityMapper::new(&crate::config::IdentityConfig {
                    email_domain: Some("example.invalid".into()),
                    ..Default::default()
                })
                .unwrap(),
            ),
        );
        engine.set_repo_id(id.to_owned());
        engine.set_git_credential_apply_clean(true);
        (engine, path)
    }

    // Fixture::new seeds an inbound KV without any ordinary handled-work proof.
    // The completed refresh proves the child only. For the untouched parent,
    // use the existing column-only, no-prior-work install bootstrap instead.
    // Remove only that exact fixture seed: never erase real mappings, receipts,
    // baselines, holds or an unknown cursor to make an ordinary cycle admissible.
    fn parent_install_bootstrap(f: &Fixture, history: &PairRefreshOperation) {
        use crate::db::repo_scope_identity as identity;

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
            crate::echo_receipt_scope::read_team_inbound_git_checkpoint_kv(&f.db, "parent")
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
        let local = crate::svn::SvnClient::new(&local_url, "", "");
        let entries = local.strict_log_range(revision, revision).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].revision, revision);
        assert_eq!(entries[0].author, "m2d-fixture-author");
        let registered = crate::svn::SvnClient::new(format!("{}/{branch}", f.root), "", "");
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

    async fn ready(f: &Fixture, engine: &crate::sync_engine::SyncEngine) -> Ready {
        let fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
        let history = completed(f, engine, "applicability-genuine-history").await;
        parent_install_bootstrap(f, &history);
        let (parent_engine, parent_bridge) = producer_engine(f, "parent");
        let (child_engine, child_bridge) = producer_engine(f, "child");
        std::fs::write(f._parent_wc.join("file.txt"), b"m2d parent work\n").unwrap();
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

    // Two actual production completions, including distinct parent commits
    // that return to the same content. No receipt/document/authority is signed
    // by a test helper. Real SVN revisions and Git commits retain their history.
    async fn two_genuine_receipts(
        f: &Fixture,
        engine: &crate::sync_engine::SyncEngine,
    ) -> (PairRefreshOperation, PairRefreshOperation) {
        let fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
        let first = completed(f, engine, "m2c-chain-first").await;
        std::fs::write(f._parent_wc.join("file.txt"), b"temporary parent work\n").unwrap();
        svn(
            &["commit", "-m", "m2c temporary parent work"],
            Some(&f._parent_wc),
        );
        let temporary = commit(&f.target, b"temporary parent work\n", &[f.parent]);
        std::fs::write(f._parent_wc.join("file.txt"), b"advanced\n").unwrap();
        svn(
            &["commit", "-m", "m2c parent content return"],
            Some(&f._parent_wc),
        );
        let parent_revision = svn_revision(&format!("{}/trunk", f.root));
        let parent = commit(&f.target, b"advanced\n", &[temporary]);
        f.target
            .reference(
                "refs/heads/main",
                parent,
                true,
                "disposable m2c parent advance",
            )
            .unwrap();
        f.db.insert_commit_map_with_repo(
            parent_revision,
            &parent.to_string(),
            "svn_to_git",
            "fixture",
            "fixture",
            Some("parent"),
        )
        .unwrap();
        f.db.update_repo_watermark("parent", parent_revision, &parent.to_string())
            .unwrap();
        crate::echo_receipt_scope::write_scoped_last_git_sha_kv(
            &f.db.conn(),
            "parent",
            &parent.to_string(),
            &chrono::Utc::now().to_rfc3339(),
        )
        .unwrap();
        let second = completed(f, engine, "m2c-chain-second").await;
        drop(fence);
        assert_ne!(first.id, second.id);
        assert_ne!(first.pins.git.prepared_sha, second.pins.git.prepared_sha);
        assert_eq!(first.pins.git.tree_sha, second.pins.git.tree_sha);
        assert_ne!(
            first.svn_receipt.as_ref().unwrap().revision,
            second.svn_receipt.as_ref().unwrap().revision
        );
        assert_eq!(f.db.pair_refresh_operation(first.id).unwrap(), first);
        assert_eq!(f.db.pair_refresh_operation(second.id).unwrap(), second);
        (first, second)
    }

    async fn capture(f: &Fixture) -> anyhow::Result<LiveChangedChildApplicabilityReview> {
        super::super::capture_changed_child_applicability_with_sources(
            &f.db,
            "child",
            Some(f.client.clone()),
            |_| Ok(()),
        )
        .await
    }

    fn object_with_entries(
        repo: &Repository,
        parents: &[Oid],
        entries: &[(&str, &[u8], i32)],
    ) -> Oid {
        let mut builder = repo.treebuilder(None).unwrap();
        for (name, bytes, mode) in entries {
            builder
                .insert(*name, repo.blob(bytes).unwrap(), *mode)
                .unwrap();
        }
        let tree = repo.find_tree(builder.write().unwrap()).unwrap();
        let signature = Signature::new(
            "Disposable applicability object control",
            "fixture@example.invalid",
            &Time::new(1_790_000_001, 0),
        )
        .unwrap();
        let parents = parents
            .iter()
            .map(|id| repo.find_commit(*id).unwrap())
            .collect::<Vec<_>>();
        let parents = parents.iter().collect::<Vec<_>>();
        repo.commit(
            None,
            &signature,
            &signature,
            "object-only applicability control",
            &tree,
            &parents,
        )
        .unwrap()
    }

    // Object-only negative/selection controls deliberately bind no live source
    // authority. The trusted positive test below uses the real factory instead.
    fn object_roles(
        repo: &Repository,
        original: &ChangedChildApplicabilityRoles,
        context: &ChangedChildReviewContext,
        child: Oid,
        parent: Oid,
    ) -> (ChangedChildApplicabilityRoles, ChangedChildReviewContext) {
        let mut roles = original.clone();
        let mut context = context.clone();
        roles.current_child = ChangedChildGitInput {
            commit: child.to_string(),
            tree: repo.find_commit(child).unwrap().tree_id().to_string(),
        };
        roles.current_parent = ChangedChildGitInput {
            commit: parent.to_string(),
            tree: repo.find_commit(parent).unwrap().tree_id().to_string(),
        };
        context.child.last_git_sha = child.to_string();
        context.child.scoped_git_checkpoint = Some(child.to_string());
        context.parent.last_git_sha = parent.to_string();
        context.parent.scoped_git_checkpoint = Some(parent.to_string());
        (roles, context)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn genuine_retained_merge_two_advances_bind_conflict_and_pin() {
        let _serial = SERIAL.lock().await;
        let f = Fixture::new();
        let engine = f.engine();
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let ready = ready(&f, &engine).await;
        let before = storage(&f.db);
        let review = capture(&f).await.unwrap();
        let manifest = review.manifest();
        let graph = &manifest.graph;
        assert_eq!(manifest.policy, CHANGED_CHILD_APPLICABILITY_POLICY);
        assert_eq!(graph.historical_merge, ready.history.pins.git);
        assert_eq!(
            graph.roles.parent_anchor.commit,
            ready.history.pins.parent.last_git_sha
        );
        assert_eq!(
            graph.roles.integrated_child.commit,
            ready.history.pins.git.prepared_sha
        );
        assert_ne!(
            graph.roles.parent_anchor.commit,
            graph.roles.integrated_child.commit
        );
        assert_eq!(
            graph.roles.parent_anchor.tree,
            graph.roles.integrated_child.tree
        );
        assert_ne!(
            graph.roles.current_child.commit,
            graph.roles.integrated_child.commit
        );
        assert_ne!(
            graph.roles.current_parent.commit,
            graph.roles.parent_anchor.commit
        );
        assert_eq!(
            graph.baseline_rule,
            "exact_authenticated_R_tree_equals_signed_P1_tree"
        );
        assert_eq!(graph.child_changes, ["file.txt"]);
        assert_eq!(graph.parent_changes, ["file.txt"]);
        assert_eq!(graph.paths.len(), 1);
        assert!(matches!(
            graph.paths[0].disposition,
            ChangedChildDisposition::Conflict
        ));
        assert_ne!(graph.paths[0].baseline, graph.paths[0].child);
        assert_ne!(graph.paths[0].baseline, graph.paths[0].parent);
        assert_ne!(graph.paths[0].child, graph.paths[0].parent);
        assert!(graph.candidate_tree.is_none());
        assert!(!graph.execution_eligible);
        assert!(!manifest.execution_eligible);
        assert!(!review.execution_eligible());
        assert_eq!(review.source_correspondence().retained_snapshot_count(), 4);
        assert_eq!(
            manifest.source_correspondence_digest,
            review.source_correspondence().context_digest()
        );
        assert_eq!(
            manifest.mapping_snapshot_digest,
            review
                .source_correspondence()
                .mapping_review()
                .snapshot_digest()
        );
        assert_eq!(manifest.manifest_digest.len(), 64);
        assert_eq!(graph.inspection_digest.len(), 64);
        assert!(manifest
            .unverified
            .iter()
            .any(|g| g == "authenticated_changed_child_resolution"));
        // The unchanged M1 path still refuses the retained merge in C's ancestry.
        let old_inputs = ChangedChildReviewInputs {
            schema: 1,
            policy: CHANGED_CHILD_REVIEW_POLICY.into(),
            baseline: graph.roles.parent_anchor.clone(),
            child: graph.roles.current_child.clone(),
            parent: graph.roles.current_parent.clone(),
            context: graph.context.clone(),
        };
        let old = inspect_changed_child_review(
            review.source.candidate.preview.git.repository(),
            &old_inputs,
        );
        assert!(
            old.is_err(),
            "new policy must not silently admit R through M1"
        );
        assert_eq!(storage(&f.db), before);
        assert_eq!(storage(engine.db()), before);
        assert_eq!(
            f.db.pair_refresh_operation(ready.history.id).unwrap(),
            ready.history
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn opaque_selection_keeps_both_binary_mode_absence_deltas() {
        let _serial = SERIAL.lock().await;
        let f = Fixture::new();
        let engine = f.engine();
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let _ready = ready(&f, &engine).await;
        let actual = capture(&f).await.unwrap();
        let graph = &actual.manifest().graph;
        let r = Oid::from_str(&graph.roles.integrated_child.commit).unwrap();
        let p1 = Oid::from_str(&graph.roles.parent_anchor.commit).unwrap();
        let tree = f.target.find_commit(r).unwrap().tree().unwrap();
        assert_eq!(tree.len(), 1);
        let original = f
            .target
            .find_blob(tree.get_name("file.txt").unwrap().id())
            .unwrap();
        let child =
            object_with_entries(&f.target, &[r], &[("child.bin", b"\0child\xff", 0o100755)]);
        let parent = object_with_entries(
            &f.target,
            &[p1],
            &[
                ("file.txt", original.content(), 0o100644),
                ("parent.bin", b"\0parent\xfe", 0o100644),
            ],
        );
        let (roles, context) = object_roles(&f.target, &graph.roles, &graph.context, child, parent);
        let before = files(f.target.path());
        let inspected = inspect_changed_child_applicable_graph(
            &f.target,
            &graph.historical_merge,
            &roles,
            &context,
        )
        .unwrap();
        assert!(inspected.child_changes.contains(&"file.txt".into()));
        assert!(inspected.child_changes.contains(&"child.bin".into()));
        assert_eq!(inspected.parent_changes, ["parent.bin"]);
        let entry = |path: &str| {
            &inspected
                .paths
                .iter()
                .find(|p| p.path == path)
                .unwrap()
                .disposition
        };
        assert!(matches!(
            entry("file.txt"),
            ChangedChildDisposition::Selected {
                entry: ChangedChildEntry::Absent {}
            }
        ));
        assert!(matches!(
            entry("child.bin"),
            ChangedChildDisposition::Selected {
                entry: ChangedChildEntry::File { mode: 0o100755, .. }
            }
        ));
        assert!(matches!(
            entry("parent.bin"),
            ChangedChildDisposition::Selected {
                entry: ChangedChildEntry::File { mode: 0o100644, .. }
            }
        ));
        assert_eq!(
            files(f.target.path()),
            before,
            "inspector must not write Git objects or refs"
        );
        let expected = object_with_entries(
            &f.target,
            &[child],
            &[
                ("child.bin", b"\0child\xff", 0o100755),
                ("parent.bin", b"\0parent\xfe", 0o100644),
            ],
        );
        assert_eq!(
            inspected.candidate_tree.as_deref(),
            Some(
                f.target
                    .find_commit(expected)
                    .unwrap()
                    .tree_id()
                    .to_string()
                    .as_str()
            )
        );
        // Compare only the analyzer interval: expected-object construction above
        // is explicit fixture setup, not an allowed effect of the inspector.
        let after_expected = files(f.target.path());
        let repeated = inspect_changed_child_applicable_graph(
            &f.target,
            &graph.historical_merge,
            &roles,
            &context,
        )
        .unwrap();
        assert_eq!(inspected, repeated);
        assert_eq!(files(f.target.path()), after_expected);
        assert!(before
            .iter()
            .all(|(p, bytes)| after_expected.get(p) == Some(bytes)));
        assert!(!inspected.execution_eligible);
        // Unsupported path topology and LFS stay refusals in the new policy.
        let mut nested = f.target.treebuilder(None).unwrap();
        nested
            .insert("leaf", f.target.blob(b"nested").unwrap(), 0o100644)
            .unwrap();
        let mut root = f.target.treebuilder(None).unwrap();
        root.insert("file.txt", nested.write().unwrap(), 0o040000)
            .unwrap();
        let tree = f.target.find_tree(root.write().unwrap()).unwrap();
        let child_commit = f.target.find_commit(child).unwrap();
        let directory_child = f
            .target
            .commit(
                None,
                &child_commit.author(),
                &child_commit.committer(),
                "object-only directory control",
                &tree,
                &[&child_commit],
            )
            .unwrap();
        let (directory_roles, directory_context) = object_roles(
            &f.target,
            &graph.roles,
            &graph.context,
            directory_child,
            parent,
        );
        let before_directory = files(f.target.path());
        let refused = inspect_changed_child_applicable_graph(
            &f.target,
            &graph.historical_merge,
            &directory_roles,
            &directory_context,
        )
        .unwrap();
        assert!(refused.candidate_tree.is_none());
        assert!(refused.paths.iter().any(|path| path.path == "file.txt"
            && matches!(&path.disposition, ChangedChildDisposition::Refused { reason }
                if reason == "unsupported_file_directory_topology")));
        assert_eq!(files(f.target.path()), before_directory);
        let lfs = object_with_entries(
            &f.target,
            &[r],
            &[(
                "file.txt",
                b"version https://git-lfs.github.com/spec/v1\n",
                0o100644,
            )],
        );
        let (lfs_roles, lfs_context) =
            object_roles(&f.target, &graph.roles, &graph.context, lfs, parent);
        let before_lfs = files(f.target.path());
        assert!(inspect_changed_child_applicable_graph(
            &f.target,
            &graph.historical_merge,
            &lfs_roles,
            &lfs_context,
        )
        .is_err());
        assert_eq!(files(f.target.path()), before_lfs);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rewritten_roles_foreign_sides_and_extra_merges_refuse() {
        let _serial = SERIAL.lock().await;
        let f = Fixture::new();
        let engine = f.engine();
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let _ready = ready(&f, &engine).await;
        let actual = capture(&f).await.unwrap();
        let graph = &actual.manifest().graph;
        let r = Oid::from_str(&graph.roles.integrated_child.commit).unwrap();
        let p1 = Oid::from_str(&graph.roles.parent_anchor.commit).unwrap();
        let c = Oid::from_str(&graph.roles.current_child.commit).unwrap();
        let p2 = Oid::from_str(&graph.roles.current_parent.commit).unwrap();
        let original = f
            .target
            .find_blob(
                f.target
                    .find_commit(c)
                    .unwrap()
                    .tree()
                    .unwrap()
                    .get_name("file.txt")
                    .unwrap()
                    .id(),
            )
            .unwrap();
        let foreign = object_with_entries(
            &f.target,
            &[p1],
            &[("file.txt", original.content(), 0o100644)],
        );
        let extra_merge = object_with_entries(
            &f.target,
            &[c, r],
            &[("file.txt", original.content(), 0o100644)],
        );
        let mut wrong_p1 = graph.roles.clone();
        wrong_p1.parent_anchor = graph.roles.integrated_child.clone();
        let mut wrong_r = graph.roles.clone();
        wrong_r.integrated_child = graph.roles.parent_anchor.clone();
        let (foreign_roles, foreign_context) =
            object_roles(&f.target, &graph.roles, &graph.context, foreign, p2);
        let (merge_roles, merge_context) =
            object_roles(&f.target, &graph.roles, &graph.context, extra_merge, p2);
        let before = files(f.target.path());
        for (label, roles, context) in [
            ("equal-content P1 rewrite", wrong_p1, graph.context.clone()),
            ("equal-content R rewrite", wrong_r, graph.context.clone()),
            ("foreign same-content C", foreign_roles, foreign_context),
            ("additional merge", merge_roles, merge_context),
        ] {
            assert!(
                inspect_changed_child_applicable_graph(
                    &f.target,
                    &graph.historical_merge,
                    &roles,
                    &context,
                )
                .is_err(),
                "{label}"
            );
        }
        assert_eq!(files(f.target.path()), before);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn ambiguous_genuine_receipts_are_not_selected() {
        let _serial = SERIAL.lock().await;
        let f = Fixture::new();
        let engine = f.engine();
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let (first, second) = two_genuine_receipts(&f, &engine).await;
        assert_ne!(first.id, second.id);
        let before = storage(&f.db);
        let target = files(f.target.path());
        let error = match capture(&f).await {
            Ok(_) => panic!("ambiguous genuine history must not select a baseline"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("ambiguous authenticated receipt candidates"),
            "{error}"
        );
        assert_eq!(storage(&f.db), before);
        assert_eq!(storage(engine.db()), before);
        assert_eq!(files(f.target.path()), target);
        assert_eq!(f.db.pair_refresh_operation(first.id).unwrap(), first);
        assert_eq!(f.db.pair_refresh_operation(second.id).unwrap(), second);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn late_mapping_policy_generation_and_history_drift_refuse() {
        let _serial = SERIAL.lock().await;
        for fault in ["mapping", "policy", "generation", "history"] {
            let f = Fixture::new();
            let engine = f.engine();
            let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
            let ready = ready(&f, &engine).await;
            let configuration =
                f.db.capture_pair_refresh_execution_configuration("child")
                    .unwrap();
            let pin = configuration.pin();
            let reached = std::cell::Cell::new(false);
            let injected = std::cell::RefCell::new(None);
            let result = super::super::capture_changed_child_applicability_with_sources(
                &f.db, "child", Some(f.client.clone()), |db| {
                    match fault {
                        "mapping" => {
                            let count = db.conn().execute(
                                "UPDATE sync_records SET message=coalesce(message,'')||' late' WHERE repo_id='child' AND git_sha=?1 AND svn_rev=?2",
                                rusqlite::params![pin.child.last_git_sha, pin.child.last_svn_rev],
                            )?;
                            assert_eq!(count, 1);
                        }
                        "policy" => {
                            assert_eq!(
                                db.conn().execute(
                                    "UPDATE repositories SET allowed_paths='[\"file.txt\"]' WHERE id='child'",
                                    [],
                                )?,
                                1
                            );
                        }
                        "generation" => {
                            db.set_state(
                                &format!("repo_echo_generation_{}", pin.child.scope_uuid),
                                &(pin.child.echo_generation + 1).to_string(),
                            )?;
                        }
                        "history" => {
                            db.set_state(
                                &format!("pair_refresh_execution_v3:authority:{}", ready.history.id),
                                "{}",
                            )?;
                        }
                        _ => unreachable!(),
                    }
                    reached.set(true);
                    *injected.borrow_mut() = Some(storage(db));
                    Ok(())
                },
            ).await;
            assert!(
                reached.get(),
                "post-inspection {fault} boundary not reached"
            );
            assert!(result.is_err(), "late {fault} must refuse");
            assert_eq!(storage(&f.db), *injected.borrow().as_ref().unwrap());
            assert_eq!(storage(engine.db()), *injected.borrow().as_ref().unwrap());
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pending_git_and_svn_work_is_refused_and_preserved() {
        let _serial = SERIAL.lock().await;
        let f = Fixture::new();
        let engine = f.engine();
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let ready = ready(&f, &engine).await;
        let first = capture(&f).await.unwrap();
        let current = Oid::from_str(&first.manifest().graph.roles.current_child.commit).unwrap();
        let pending = commit(&f.target, b"unhandled applicability Git work\n", &[current]);
        f.target
            .reference(
                "refs/heads/feature",
                pending,
                true,
                "disposable pending-work fault",
            )
            .unwrap();
        let before = storage(&f.db);
        let target = files(f.target.path());
        assert!(capture(&f).await.is_err());
        assert_eq!(storage(&f.db), before);
        assert_eq!(files(f.target.path()), target);
        assert_eq!(
            f.target
                .find_reference("refs/heads/feature")
                .unwrap()
                .target(),
            Some(pending)
        );
        // Restore only the deliberately injected ref before the separate SVN fault.
        f.target
            .reference(
                "refs/heads/feature",
                current,
                true,
                "restore disposable Git fault",
            )
            .unwrap();
        svn(&["update"], Some(&ready.child_wc));
        std::fs::write(
            ready.child_wc.join("file.txt"),
            b"unhandled applicability SVN work\n",
        )
        .unwrap();
        svn(
            &[
                "commit",
                "--username",
                "m2d-fixture-author",
                "-m",
                "pending applicability source",
            ],
            Some(&ready.child_wc),
        );
        let pending_revision = svn_revision(&local_source_url(&f, "branches/f"));
        let child = files(&ready.child_wc);
        let before = storage(&f.db);
        assert!(capture(&f).await.is_err());
        assert_eq!(storage(&f.db), before);
        assert_eq!(storage(engine.db()), before);
        assert_eq!(files(&ready.child_wc), child);
        assert_eq!(
            svn_revision(&local_source_url(&f, "branches/f")),
            pending_revision
        );
        assert_eq!(
            std::fs::read(ready.child_wc.join("file.txt")).unwrap(),
            b"unhandled applicability SVN work\n"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn post_inspection_property_visibility_drift_refuses() {
        let _serial = SERIAL.lock().await;
        let f = Fixture::new();
        let engine = f.engine();
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let ready = ready(&f, &engine).await;
        let first = capture(&f).await.unwrap();
        let old_head = first.manifest().observed_svn.inspected_head_revision;
        let reached = std::cell::Cell::new(false);
        let revision = std::cell::Cell::new(0);
        let before = storage(&f.db);
        let result = super::super::capture_changed_child_applicability_with_sources(
            &f.db,
            "child",
            Some(f.client.clone()),
            |_| {
                svn(&["update"], Some(&ready.child_wc));
                svn(
                    &["propset", "unreviewed:property", "retained", "."],
                    Some(&ready.child_wc),
                );
                svn(
                    &[
                        "commit",
                        "--username",
                        "m2d-fixture-author",
                        "-m",
                        "late applicability property",
                    ],
                    Some(&ready.child_wc),
                );
                revision.set(svn_revision(&local_source_url(&f, "branches/f")));
                reached.set(true);
                Ok(())
            },
        )
        .await;
        assert!(
            reached.get(),
            "real post-inspection boundary must be reached"
        );
        assert!(revision.get() > old_head);
        assert!(result.is_err());
        assert_eq!(storage(&f.db), before);
        assert_eq!(storage(engine.db()), before);
        assert_eq!(
            svn_revision(&local_source_url(&f, "branches/f")),
            revision.get()
        );
        let value = run(Command::new("svn").args([
            "propget",
            "unreviewed:property",
            ready.child_wc.to_str().unwrap(),
            "--non-interactive",
            "--no-auth-cache",
        ]));
        assert_eq!(value.trim(), "retained");
        // This proves late endpoint/property drift refusal, not which per-layer
        // authz or property validator would be reached on a subsequent capture.
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pin_comparison_serialized_metadata_and_resources_stay_read_only() {
        let _serial = SERIAL.lock().await;
        let f = Fixture::new();
        let engine = f.engine();
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let ready = ready(&f, &engine).await;
        let before = storage(&f.db);
        let target = files(f.target.path());
        let parent_wc = files(&f._parent_wc);
        let child_wc = files(&ready.child_wc);
        let parent_bridge = files(&ready.parent_bridge);
        let child_bridge = files(&ready.child_bridge);
        let requests = std::fs::read(&f.log).unwrap();
        let first = capture(&f).await.unwrap();
        let second = capture(&f).await.unwrap();
        let pin = first.manifest().manifest_digest.clone();
        assert_eq!(second.manifest().manifest_digest, pin);
        second.require_manifest_digest(&pin).unwrap();
        assert!(second.require_manifest_digest(&"0".repeat(64)).is_err());
        let mut serialized = serde_json::to_value(first.manifest()).unwrap();
        serialized["execution_eligible"] = serde_json::json!(true);
        serialized["manifest_digest"] = serde_json::json!("caller-selected");
        assert!(second
            .require_manifest_digest(serialized["manifest_digest"].as_str().unwrap())
            .is_err());
        assert!(!second.manifest().execution_eligible);
        assert!(!second.execution_eligible());
        assert!(!second.source_correspondence().execution_eligible());
        let snapshots = first
            .source
            .snapshots
            .iter()
            .map(|s| s.path().parent().unwrap().to_owned())
            .collect::<Vec<_>>();
        let git = first
            .source
            .candidate
            .preview
            .git
            .repository()
            .path()
            .to_owned();
        assert_eq!(snapshots.len(), 4);
        assert!(snapshots.iter().all(|p| p.is_dir()));
        assert!(git.is_dir());
        let reads = std::fs::read(&f.log).unwrap();
        assert!(reads.starts_with(&requests));
        let added = std::str::from_utf8(&reads[requests.len()..]).unwrap();
        assert!(added.contains("git-upload-pack"));
        assert!(!added.contains("git-receive-pack"));
        assert_eq!(storage(&f.db), before);
        assert_eq!(storage(engine.db()), before);
        assert_eq!(files(f.target.path()), target);
        assert_eq!(files(&f._parent_wc), parent_wc);
        assert_eq!(files(&ready.child_wc), child_wc);
        assert_eq!(files(&ready.parent_bridge), parent_bridge);
        assert_eq!(files(&ready.child_bridge), child_bridge);
        drop(first);
        assert!(snapshots.iter().all(|p| !p.exists()));
        assert!(!git.exists());
        assert_eq!(second.manifest().manifest_digest, pin);
    }
}

/// Source-only candidate-review regressions. Positive preparation uses the exact
/// qualified applicability fixture recipe; object controls confer no authority.
#[cfg(test)]
mod changed_child_resolution_candidate_review {
    use super::*;
    use crate::db::pair_refresh_operations::{PairRefreshOperation, PairRefreshPhase};
    use crate::pair_refresh::preparation::{
        inspect_changed_child_applicable_graph, require_changed_child_resolution_request_header,
        resolve_changed_child_applicable_graph, resolve_changed_child_review,
        ChangedChildApplicabilityRoles, ChangedChildChoice, ChangedChildConflictChoice,
        ChangedChildDisposition, ChangedChildEntry, ChangedChildGitInput, ChangedChildResolution,
        ChangedChildResolutionCandidateRequest, ChangedChildReviewContext,
        ChangedChildReviewInputs, CHANGED_CHILD_RESOLUTION_CANDIDATE_POLICY,
        CHANGED_CHILD_REVIEW_POLICY,
    };
    use std::collections::BTreeMap;

    type Storage = BTreeMap<String, Vec<Vec<rusqlite::types::Value>>>;

    fn storage(db: &Database) -> Storage {
        use rusqlite::types::Value;
        let conn = db.conn();
        let mut snapshot = BTreeMap::new();
        let schema = conn
            .prepare("SELECT type,name,tbl_name,rootpage,sql FROM sqlite_master ORDER BY type,name")
            .unwrap()
            .query_map([], |row| {
                (0..5)
                    .map(|i| row.get::<_, Value>(i))
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        snapshot.insert("schema".into(), schema);
        let tables = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        for table in tables {
            let sql = format!("SELECT * FROM \"{}\"", table.replace('"', "\"\""));
            let mut query = conn.prepare(&sql).unwrap();
            let columns = query.column_count();
            let mut values = query
                .query_map([], |row| {
                    (0..columns)
                        .map(|i| row.get::<_, Value>(i))
                        .collect::<rusqlite::Result<Vec<_>>>()
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            values.sort_by_cached_key(|row| format!("{row:?}"));
            snapshot.insert(table, values);
        }
        snapshot
    }

    fn files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        fn collect(root: &Path, path: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
            for entry in std::fs::read_dir(path).unwrap() {
                let entry = entry.unwrap();
                let kind = entry.file_type().unwrap();
                assert!(!kind.is_symlink());
                if kind.is_dir() {
                    collect(root, &entry.path(), out);
                } else {
                    assert!(kind.is_file());
                    out.insert(
                        entry.path().strip_prefix(root).unwrap().to_owned(),
                        std::fs::read(entry.path()).unwrap(),
                    );
                }
            }
        }
        let mut out = BTreeMap::new();
        collect(root, root, &mut out);
        out
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

    async fn completed(
        f: &Fixture,
        engine: &crate::sync_engine::SyncEngine,
        request: &str,
    ) -> PairRefreshOperation {
        let (actor, _workspace, value) = prepared(f, engine).await;
        let approved = actor.prepared_approval_digest(engine, &value).unwrap();
        let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
        let op = super::super::super::execution::complete_owned_refresh_with_client(
            engine,
            &actor,
            &value,
            request,
            &approved,
            &busy,
            f.client.clone(),
        )
        .await
        .unwrap();
        assert_eq!(op.phase, PairRefreshPhase::Completed);
        assert_eq!(f.db.pair_refresh_operation(op.id).unwrap(), op);
        op
    }

    // Use the same native DB and disposable bare repository, with a distinct
    // ordinary bridge per role. Local Git transport here avoids a new CA policy;
    // the correspondence factory still uses registered HTTPS observers/client.
    fn producer_engine(f: &Fixture, id: &str) -> (crate::sync_engine::SyncEngine, PathBuf) {
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
        config.daemon.data_dir = f.temp.path().to_owned();
        let svn_branch = if id == "child" { "branches/f" } else { "trunk" };
        let svn_url = format!("{}/{svn_branch}", f.root);
        config.svn.url = svn_url.clone();
        config.svn.layout = crate::config::SvnLayout::Custom;
        config.svn.password = Some(String::new());
        config.github.default_branch = branch.to_owned();
        config.github.token = Some(String::new());
        config.sync.auto_merge = false;
        let mut engine = crate::sync_engine::SyncEngine::new(
            config,
            Database::new(f.temp.path().join("reposync.db")).unwrap(),
            crate::svn::SvnClient::new(svn_url, "", ""),
            crate::git::GitClient::new(&path).unwrap(),
            std::sync::Arc::new(
                crate::identity::mapper::IdentityMapper::new(&crate::config::IdentityConfig {
                    email_domain: Some("example.invalid".into()),
                    ..Default::default()
                })
                .unwrap(),
            ),
        );
        engine.set_repo_id(id.to_owned());
        engine.set_git_credential_apply_clean(true);
        (engine, path)
    }

    // Fixture::new seeds an inbound KV without any ordinary handled-work proof.
    // The completed refresh proves the child only. For the untouched parent,
    // use the existing column-only, no-prior-work install bootstrap instead.
    // Remove only that exact fixture seed: never erase real mappings, receipts,
    // baselines, holds or an unknown cursor to make an ordinary cycle admissible.
    fn parent_install_bootstrap(f: &Fixture, history: &PairRefreshOperation) {
        use crate::db::repo_scope_identity as identity;

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
            crate::echo_receipt_scope::read_team_inbound_git_checkpoint_kv(&f.db, "parent")
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
        let local = crate::svn::SvnClient::new(&local_url, "", "");
        let entries = local.strict_log_range(revision, revision).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].revision, revision);
        assert_eq!(entries[0].author, "m2d-fixture-author");
        let registered = crate::svn::SvnClient::new(format!("{}/{branch}", f.root), "", "");
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

    async fn ready(f: &Fixture, engine: &crate::sync_engine::SyncEngine) -> Ready {
        let fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
        let history = completed(f, engine, "applicability-genuine-history").await;
        parent_install_bootstrap(f, &history);
        let (parent_engine, parent_bridge) = producer_engine(f, "parent");
        let (child_engine, child_bridge) = producer_engine(f, "child");
        std::fs::write(f._parent_wc.join("file.txt"), b"m2d parent work\n").unwrap();
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

    fn object_with_entries(
        repo: &Repository,
        parents: &[Oid],
        entries: &[(&str, &[u8], i32)],
    ) -> Oid {
        let mut builder = repo.treebuilder(None).unwrap();
        for (name, bytes, mode) in entries {
            builder
                .insert(*name, repo.blob(bytes).unwrap(), *mode)
                .unwrap();
        }
        let tree = repo.find_tree(builder.write().unwrap()).unwrap();
        let signature = Signature::new(
            "Disposable applicability object control",
            "fixture@example.invalid",
            &Time::new(1_790_000_001, 0),
        )
        .unwrap();
        let parents = parents
            .iter()
            .map(|id| repo.find_commit(*id).unwrap())
            .collect::<Vec<_>>();
        let parents = parents.iter().collect::<Vec<_>>();
        repo.commit(
            None,
            &signature,
            &signature,
            "object-only applicability control",
            &tree,
            &parents,
        )
        .unwrap()
    }

    // Object-only negative/selection controls deliberately bind no live source
    // authority. The trusted positive test below uses the real factory instead.
    fn object_roles(
        repo: &Repository,
        original: &ChangedChildApplicabilityRoles,
        context: &ChangedChildReviewContext,
        child: Oid,
        parent: Oid,
    ) -> (ChangedChildApplicabilityRoles, ChangedChildReviewContext) {
        let mut roles = original.clone();
        let mut context = context.clone();
        roles.current_child = ChangedChildGitInput {
            commit: child.to_string(),
            tree: repo.find_commit(child).unwrap().tree_id().to_string(),
        };
        roles.current_parent = ChangedChildGitInput {
            commit: parent.to_string(),
            tree: repo.find_commit(parent).unwrap().tree_id().to_string(),
        };
        context.child.last_git_sha = child.to_string();
        context.child.scoped_git_checkpoint = Some(child.to_string());
        context.parent.last_git_sha = parent.to_string();
        context.parent.scoped_git_checkpoint = Some(parent.to_string());
        (roles, context)
    }

    async fn preview(f: &Fixture) -> anyhow::Result<LiveChangedChildApplicabilityReview> {
        super::super::capture_changed_child_applicability_with_sources(
            &f.db,
            "child",
            Some(f.client.clone()),
            |_| Ok(()),
        )
        .await
    }

    fn choices(
        graph: &crate::pair_refresh::preparation::ChangedChildApplicableGraphReview,
        side: ChangedChildChoice,
    ) -> Vec<ChangedChildConflictChoice> {
        let mut result = Vec::new();
        for path in &graph.paths {
            if matches!(path.disposition, ChangedChildDisposition::Conflict) {
                result.push(ChangedChildConflictChoice {
                    path: path.path.clone(),
                    baseline: path.baseline.clone(),
                    child: path.child.clone(),
                    parent: path.parent.clone(),
                    choice: side,
                });
            }
        }
        result
    }

    fn request(
        current: &LiveChangedChildApplicabilityReview,
        side: ChangedChildChoice,
    ) -> ChangedChildResolutionCandidateRequest {
        ChangedChildResolutionCandidateRequest {
            schema: 1,
            policy: CHANGED_CHILD_RESOLUTION_CANDIDATE_POLICY.into(),
            manifest_digest: current.manifest().manifest_digest.clone(),
            choices: choices(&current.manifest().graph, side),
        }
    }

    async fn resolve(
        f: &Fixture,
        request: &ChangedChildResolutionCandidateRequest,
    ) -> anyhow::Result<LiveChangedChildResolutionCandidateReview> {
        super::super::capture_changed_child_resolution_candidate_with_sources(
            &f.db,
            "child",
            request,
            Some(f.client.clone()),
            |_| Ok(()),
        )
        .await
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fresh_live_base_child_parent_choices_bind_exact_candidate() {
        let _serial = SERIAL.lock().await;
        let f = Fixture::new();
        let engine = f.engine();
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let ready = ready(&f, &engine).await;
        let current = preview(&f).await.unwrap();
        assert_eq!(
            choices(&current.manifest().graph, ChangedChildChoice::Base).len(),
            1
        );
        let before = storage(&f.db);
        let target = files(f.target.path());
        let mut digests = std::collections::BTreeSet::new();
        for (side, expected_tree) in [
            (
                ChangedChildChoice::Base,
                current.manifest().graph.roles.integrated_child.tree.clone(),
            ),
            (
                ChangedChildChoice::Child,
                current.manifest().graph.roles.current_child.tree.clone(),
            ),
            (
                ChangedChildChoice::Parent,
                current.manifest().graph.roles.current_parent.tree.clone(),
            ),
        ] {
            let intent = request(&current, side);
            let result = resolve(&f, &intent).await.unwrap();
            let manifest = result.manifest();
            assert_eq!(
                manifest.applicability_manifest_digest,
                intent.manifest_digest
            );
            assert_eq!(manifest.graph.candidate_tree, expected_tree);
            assert_eq!(manifest.graph.choices, intent.choices);
            assert_eq!(manifest.graph.source_graph, current.manifest().graph);
            assert_eq!(manifest.graph.paths.len(), 1);
            let original = &current.manifest().graph.paths[0];
            let expected = match side {
                ChangedChildChoice::Base => original.baseline.clone(),
                ChangedChildChoice::Child => original.child.clone(),
                ChangedChildChoice::Parent => original.parent.clone(),
            };
            assert_eq!(
                manifest.graph.paths[0].disposition,
                ChangedChildDisposition::Selected { entry: expected }
            );
            assert_eq!(manifest.candidate_digest.len(), 64);
            assert_eq!(manifest.graph.candidate_digest.len(), 64);
            assert!(!manifest.execution_eligible);
            assert!(!manifest.graph.execution_eligible);
            assert!(!result.execution_eligible());
            assert!(!result.applicability_review().execution_eligible());
            assert!(manifest.graph.source_graph.context.actor_policy.is_none());
            assert!(manifest
                .unverified
                .iter()
                .any(|g| g == "authenticated_changed_child_resolution"));
            assert!(manifest
                .unverified
                .iter()
                .any(|g| g == "resolution_choices_not_actor_authorized"));
            digests.insert(manifest.candidate_digest.clone());
            assert_eq!(storage(&f.db), before);
            assert_eq!(storage(engine.db()), before);
            assert_eq!(files(f.target.path()), target);
        }
        assert_eq!(digests.len(), 3);
        assert_eq!(
            f.db.pair_refresh_operation(ready.history.id).unwrap(),
            ready.history
        );
        let graph = &current.manifest().graph;
        let inputs = ChangedChildReviewInputs {
            schema: 1,
            policy: CHANGED_CHILD_REVIEW_POLICY.into(),
            baseline: graph.roles.parent_anchor.clone(),
            child: graph.roles.current_child.clone(),
            parent: graph.roles.current_parent.clone(),
            context: graph.context.clone(),
        };
        let old_request = ChangedChildResolution {
            schema: 1,
            policy: CHANGED_CHILD_REVIEW_POLICY.into(),
            inputs: inputs.clone(),
            manifest_digest: "0".repeat(64),
            choices: choices(graph, ChangedChildChoice::Child),
        };
        let error = match resolve_changed_child_review(
            current.source.candidate.preview.git.repository(),
            &inputs,
            &old_request,
        ) {
            Ok(_) => panic!("M1 must still refuse the retained R merge"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("nonlinear"), "{error}");
        assert_eq!(storage(&f.db), before);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn object_resolution_keeps_uncontested_deltas_and_canonical_choice_order() {
        let _serial = SERIAL.lock().await;
        let f = Fixture::new();
        let engine = f.engine();
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let _ready = ready(&f, &engine).await;
        let current = preview(&f).await.unwrap();
        let original = &current.manifest().graph;
        let r = Oid::from_str(&original.roles.integrated_child.commit).unwrap();
        let p1 = Oid::from_str(&original.roles.parent_anchor.commit).unwrap();
        let child = object_with_entries(
            &f.target,
            &[r],
            &[
                ("file.txt", b"child conflict", 0o100644),
                ("overlap.bin", b"\0child overlap", 0o100644),
                ("child.bin", b"\0child only\xff", 0o100755),
            ],
        );
        let parent = object_with_entries(
            &f.target,
            &[p1],
            &[
                ("file.txt", b"parent conflict", 0o100644),
                ("overlap.bin", b"\0parent overlap", 0o100644),
                ("parent.bin", b"\0parent only\xfe", 0o100644),
            ],
        );
        let (roles, context) =
            object_roles(&f.target, &original.roles, &original.context, child, parent);
        let graph = inspect_changed_child_applicable_graph(
            &f.target,
            &original.historical_merge,
            &roles,
            &context,
        )
        .unwrap();
        let mut selected = choices(&graph, ChangedChildChoice::Parent);
        assert_eq!(selected.len(), 2);
        selected
            .iter_mut()
            .find(|c| c.path == "overlap.bin")
            .unwrap()
            .choice = ChangedChildChoice::Base;
        let expected = object_with_entries(
            &f.target,
            &[child],
            &[
                ("file.txt", b"parent conflict", 0o100644),
                ("child.bin", b"\0child only\xff", 0o100755),
                ("parent.bin", b"\0parent only\xfe", 0o100644),
            ],
        );
        let expected_tree = f
            .target
            .find_commit(expected)
            .unwrap()
            .tree_id()
            .to_string();
        let before = files(f.target.path());
        let first = resolve_changed_child_applicable_graph(
            &f.target,
            &graph.historical_merge,
            &roles,
            &context,
            &graph.inspection_digest,
            &selected,
        )
        .unwrap();
        selected.reverse();
        let second = resolve_changed_child_applicable_graph(
            &f.target,
            &graph.historical_merge,
            &roles,
            &context,
            &graph.inspection_digest,
            &selected,
        )
        .unwrap();
        assert_eq!(first, second);
        assert_eq!(first.candidate_tree, expected_tree);
        assert_eq!(files(f.target.path()), before);
        assert!(!first.execution_eligible);
        for path in &graph.paths {
            if !matches!(path.disposition, ChangedChildDisposition::Conflict) {
                assert_eq!(
                    &first
                        .paths
                        .iter()
                        .find(|p| p.path == path.path)
                        .unwrap()
                        .disposition,
                    &path.disposition
                );
            }
        }
        assert!(matches!(
            first
                .paths
                .iter()
                .find(|p| p.path == "overlap.bin")
                .unwrap()
                .disposition,
            ChangedChildDisposition::Selected {
                entry: ChangedChildEntry::Absent {}
            }
        ));
        assert!(matches!(
            first
                .paths
                .iter()
                .find(|p| p.path == "child.bin")
                .unwrap()
                .disposition,
            ChangedChildDisposition::Selected {
                entry: ChangedChildEntry::File { mode: 0o100755, .. }
            }
        ));
        // Empty choices are complete when both independent deltas are uncontested.
        let base_tree = f.target.find_commit(r).unwrap().tree().unwrap();
        let base_blob = f
            .target
            .find_blob(base_tree.get_name("file.txt").unwrap().id())
            .unwrap();
        let clean_child = object_with_entries(
            &f.target,
            &[r],
            &[
                ("file.txt", base_blob.content(), 0o100644),
                ("child.bin", b"child uncontested", 0o100755),
            ],
        );
        let clean_parent = object_with_entries(
            &f.target,
            &[p1],
            &[
                ("file.txt", base_blob.content(), 0o100644),
                ("parent.bin", b"parent uncontested", 0o100644),
            ],
        );
        let (clean_roles, clean_context) = object_roles(
            &f.target,
            &original.roles,
            &original.context,
            clean_child,
            clean_parent,
        );
        let clean_graph = inspect_changed_child_applicable_graph(
            &f.target,
            &original.historical_merge,
            &clean_roles,
            &clean_context,
        )
        .unwrap();
        assert!(choices(&clean_graph, ChangedChildChoice::Base).is_empty());
        let before_clean = files(f.target.path());
        let clean_result = resolve_changed_child_applicable_graph(
            &f.target,
            &clean_graph.historical_merge,
            &clean_roles,
            &clean_context,
            &clean_graph.inspection_digest,
            &[],
        )
        .unwrap();
        assert!(clean_result.choices.is_empty());
        assert_eq!(
            Some(clean_result.candidate_tree.as_str()),
            clean_graph.candidate_tree.as_deref()
        );
        assert_eq!(clean_result.paths, clean_graph.paths);
        assert_eq!(files(f.target.path()), before_clean);
        // A refused file/directory graph cannot become a choice candidate.
        let mut nested = f.target.treebuilder(None).unwrap();
        nested
            .insert("leaf", f.target.blob(b"nested").unwrap(), 0o100644)
            .unwrap();
        let mut root = f.target.treebuilder(None).unwrap();
        root.insert("file.txt", nested.write().unwrap(), 0o040000)
            .unwrap();
        let tree = f.target.find_tree(root.write().unwrap()).unwrap();
        let c = f.target.find_commit(child).unwrap();
        let directory_child = f
            .target
            .commit(
                None,
                &c.author(),
                &c.committer(),
                "directory control",
                &tree,
                &[&c],
            )
            .unwrap();
        let (bad_roles, bad_context) = object_roles(
            &f.target,
            &original.roles,
            &original.context,
            directory_child,
            parent,
        );
        let refused = inspect_changed_child_applicable_graph(
            &f.target,
            &original.historical_merge,
            &bad_roles,
            &bad_context,
        )
        .unwrap();
        assert!(refused
            .paths
            .iter()
            .any(|p| matches!(p.disposition, ChangedChildDisposition::Refused { .. })));
        let before = files(f.target.path());
        let error = match resolve_changed_child_applicable_graph(
            &f.target,
            &refused.historical_merge,
            &bad_roles,
            &bad_context,
            &refused.inspection_digest,
            &choices(&refused, ChangedChildChoice::Base),
        ) {
            Ok(_) => panic!("side choice must not resolve refused topology"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("unsupported topology"), "{error}");
        assert_eq!(files(f.target.path()), before);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn choice_set_requires_complete_unique_exact_conflict_triples() {
        let _serial = SERIAL.lock().await;
        let f = Fixture::new();
        let engine = f.engine();
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let _ready = ready(&f, &engine).await;
        let current = preview(&f).await.unwrap();
        let graph = &current.manifest().graph;
        let correct = choices(graph, ChangedChildChoice::Child);
        assert_eq!(correct.len(), 1);
        let mut duplicate = correct.clone();
        duplicate.push(correct[0].clone());
        let mut unknown = correct.clone();
        unknown[0].path = "unknown.txt".into();
        let mut malformed = correct.clone();
        malformed[0].path = "../file.txt".into();
        let mut stale_base = correct.clone();
        stale_base[0].baseline = ChangedChildEntry::Absent {};
        let mut stale_child = correct.clone();
        stale_child[0].child = ChangedChildEntry::File {
            blob: "0".repeat(40),
            mode: 0o100755,
        };
        let mut stale_parent = correct.clone();
        stale_parent[0].parent = ChangedChildEntry::Absent {};
        let repository = current.source.candidate.preview.git.repository();
        let before = files(repository.path());
        for (label, selected) in [
            ("missing", Vec::new()),
            ("duplicate", duplicate),
            ("unknown", unknown),
            ("malformed", malformed),
            ("stale baseline", stale_base),
            ("stale child", stale_child),
            ("stale parent", stale_parent),
        ] {
            assert!(
                resolve_changed_child_applicable_graph(
                    repository,
                    &graph.historical_merge,
                    &graph.roles,
                    &graph.context,
                    &graph.inspection_digest,
                    &selected,
                )
                .is_err(),
                "{label}"
            );
        }
        assert!(resolve_changed_child_applicable_graph(
            repository,
            &graph.historical_merge,
            &graph.roles,
            &graph.context,
            &"0".repeat(64),
            &correct,
        )
        .is_err());
        assert_eq!(files(repository.path()), before);
        // An existing uncontested entry is also not a conflict-choice slot.
        let r = Oid::from_str(&graph.roles.integrated_child.commit).unwrap();
        let p1 = Oid::from_str(&graph.roles.parent_anchor.commit).unwrap();
        let child = object_with_entries(
            &f.target,
            &[r],
            &[
                ("file.txt", b"child conflict", 0o100644),
                ("only.txt", b"child only", 0o100644),
            ],
        );
        let parent = object_with_entries(
            &f.target,
            &[p1],
            &[("file.txt", b"parent conflict", 0o100644)],
        );
        let (roles, context) = object_roles(&f.target, &graph.roles, &graph.context, child, parent);
        let mixed = inspect_changed_child_applicable_graph(
            &f.target,
            &graph.historical_merge,
            &roles,
            &context,
        )
        .unwrap();
        let mut selected = choices(&mixed, ChangedChildChoice::Child);
        let only = mixed.paths.iter().find(|p| p.path == "only.txt").unwrap();
        selected.push(ChangedChildConflictChoice {
            path: only.path.clone(),
            baseline: only.baseline.clone(),
            child: only.child.clone(),
            parent: only.parent.clone(),
            choice: ChangedChildChoice::Base,
        });
        let before = files(f.target.path());
        let error = match resolve_changed_child_applicable_graph(
            &f.target,
            &mixed.historical_merge,
            &roles,
            &context,
            &mixed.inspection_digest,
            &selected,
        ) {
            Ok(_) => panic!("uncontested child delta must not be a choice slot"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("unknown or non-conflict"), "{error}");
        assert_eq!(files(f.target.path()), before);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn request_schema_policy_unknown_fields_and_approval_assertions_refuse() {
        let valid = ChangedChildResolutionCandidateRequest {
            schema: 1,
            policy: CHANGED_CHILD_RESOLUTION_CANDIDATE_POLICY.into(),
            manifest_digest: "a".repeat(64),
            choices: Vec::new(),
        };
        require_changed_child_resolution_request_header(&valid).unwrap();
        let json = serde_json::to_value(&valid).unwrap();
        assert_eq!(
            serde_json::from_value::<ChangedChildResolutionCandidateRequest>(json.clone()).unwrap(),
            valid
        );
        for field in [
            "actor_approval",
            "actor_policy",
            "execution_eligible",
            "source_manifest",
        ] {
            let mut value = json.clone();
            value[field] = serde_json::json!(true);
            assert!(
                serde_json::from_value::<ChangedChildResolutionCandidateRequest>(value).is_err(),
                "{field}"
            );
        }
        for (schema, policy, digest) in [
            (0, CHANGED_CHILD_RESOLUTION_CANDIDATE_POLICY, "a".repeat(64)),
            (2, CHANGED_CHILD_RESOLUTION_CANDIDATE_POLICY, "a".repeat(64)),
            (1, CHANGED_CHILD_REVIEW_POLICY, "a".repeat(64)),
            (1, CHANGED_CHILD_RESOLUTION_CANDIDATE_POLICY, String::new()),
            (1, CHANGED_CHILD_RESOLUTION_CANDIDATE_POLICY, "A".repeat(64)),
        ] {
            let request = ChangedChildResolutionCandidateRequest {
                schema,
                policy: policy.into(),
                manifest_digest: digest,
                choices: Vec::new(),
            };
            assert!(require_changed_child_resolution_request_header(&request).is_err());
        }
        let duplicate = format!(
            "{{\"schema\":1,\"schema\":1,\"policy\":\"{}\",\"manifest_digest\":\"{}\",\"choices\":[]}}",
            valid.policy, valid.manifest_digest,
        );
        assert!(
            serde_json::from_str::<ChangedChildResolutionCandidateRequest>(&duplicate).is_err()
        );
        let entry = serde_json::json!({"kind":"absent"});
        let choice = serde_json::json!({
            "path":"file.txt", "baseline":entry.clone(), "child":entry.clone(),
            "parent":entry, "choice":"child",
        });
        let mut with_choice = json.clone();
        with_choice["choices"] = serde_json::json!([choice]);
        assert!(
            serde_json::from_value::<ChangedChildResolutionCandidateRequest>(with_choice.clone())
                .is_ok()
        );
        let mut unknown_side = with_choice.clone();
        unknown_side["choices"][0]["choice"] = serde_json::json!("manual");
        assert!(
            serde_json::from_value::<ChangedChildResolutionCandidateRequest>(unknown_side).is_err()
        );
        let mut unknown_entry = with_choice.clone();
        unknown_entry["choices"][0]["baseline"]["approval"] = serde_json::json!(true);
        assert!(
            serde_json::from_value::<ChangedChildResolutionCandidateRequest>(unknown_entry)
                .is_err()
        );
        let mut unknown_choice_field = with_choice;
        unknown_choice_field["choices"][0]["approved"] = serde_json::json!(true);
        assert!(
            serde_json::from_value::<ChangedChildResolutionCandidateRequest>(unknown_choice_field)
                .is_err()
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stale_manifest_and_changed_mapping_or_policy_require_fresh_review() {
        let _serial = SERIAL.lock().await;
        let f = Fixture::new();
        let engine = f.engine();
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let _ready = ready(&f, &engine).await;
        let original = preview(&f).await.unwrap();
        let old_request = request(&original, ChangedChildChoice::Child);
        let mut wrong_pin = old_request.clone();
        wrong_pin.manifest_digest = "0".repeat(64);
        let before = storage(&f.db);
        let error = match resolve(&f, &wrong_pin).await {
            Ok(_) => panic!("wrong manifest pin must not select a result"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("stale or different"), "{error}");
        assert_eq!(storage(&f.db), before);
        let pin = &original.manifest().configuration.child;
        assert_eq!(f.db.conn().execute(
            "UPDATE sync_records SET message=coalesce(message,'')||' review metadata' WHERE repo_id='child' AND git_sha=?1 AND svn_rev=?2",
            rusqlite::params![pin.last_git_sha, pin.last_svn_rev],
        ).unwrap(), 1);
        let injected = storage(&f.db);
        let fresh = preview(&f).await.unwrap();
        assert_ne!(
            fresh.manifest().manifest_digest,
            original.manifest().manifest_digest
        );
        let error = match resolve(&f, &old_request).await {
            Ok(_) => panic!("old manifest pin must not adopt a new mapping snapshot"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("stale or different"), "{error}");
        let new_request = request(&fresh, ChangedChildChoice::Child);
        let new_result = resolve(&f, &new_request).await.unwrap();
        assert_eq!(
            new_result.manifest().applicability_manifest_digest,
            fresh.manifest().manifest_digest
        );
        assert_eq!(storage(&f.db), injected);
        assert_eq!(storage(engine.db()), injected);
        assert_eq!(
            f.db.conn()
                .execute(
                    "UPDATE repositories SET allowed_paths='[\"file.txt\"]' WHERE id='child'",
                    []
                )
                .unwrap(),
            1
        );
        let policy_fault = storage(&f.db);
        assert!(resolve(&f, &new_request).await.is_err());
        assert_eq!(storage(&f.db), policy_fault);
        assert_eq!(storage(engine.db()), policy_fault);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn late_resolution_source_database_and_history_drift_refuse() {
        let _serial = SERIAL.lock().await;
        for fault in ["mapping", "generation", "history", "git", "svn"] {
            let f = Fixture::new();
            let engine = f.engine();
            let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
            let ready = ready(&f, &engine).await;
            let current = preview(&f).await.unwrap();
            let intent = request(&current, ChangedChildChoice::Parent);
            let pin = &current.manifest().configuration.child;
            let pending = commit(
                &f.target,
                b"late resolution Git work\n",
                &[Oid::from_str(&pin.last_git_sha).unwrap()],
            );
            let reached = std::cell::Cell::new(false);
            let injected = std::cell::RefCell::new(None);
            let retained_git = std::cell::RefCell::new(None);
            let retained_svn = std::cell::Cell::new(0);
            let result = super::super::capture_changed_child_resolution_candidate_with_sources(
                &f.db, "child", &intent, Some(f.client.clone()), |db| {
                    match fault {
                        "mapping" => {
                            assert_eq!(db.conn().execute(
                                "UPDATE sync_records SET message=coalesce(message,'')||' after choice' WHERE repo_id='child' AND git_sha=?1 AND svn_rev=?2",
                                rusqlite::params![pin.last_git_sha, pin.last_svn_rev],
                            )?, 1);
                        }
                        "generation" => db.set_state(
                            &format!("repo_echo_generation_{}", pin.scope_uuid),
                            &(pin.echo_generation + 1).to_string(),
                        )?,
                        "history" => db.set_state(
                            &format!("pair_refresh_execution_v3:authority:{}", ready.history.id), "{}",
                        )?,
                        "git" => {
                            f.target.reference("refs/heads/feature", pending, true, "late resolution fault")?;
                            *retained_git.borrow_mut() = Some(files(f.target.path()));
                        }
                        "svn" => {
                            svn(&["update"], Some(&ready.child_wc));
                            svn(&["propset", "unreviewed:resolution", "retained", "."], Some(&ready.child_wc));
                            svn(&["commit", "--username", "m2d-fixture-author", "-m", "late resolution property"], Some(&ready.child_wc));
                            retained_svn.set(svn_revision(&local_source_url(&f, "branches/f")));
                        }
                        _ => unreachable!(),
                    }
                    reached.set(true);
                    *injected.borrow_mut() = Some(storage(db));
                    Ok(())
                },
            ).await;
            assert!(
                reached.get(),
                "actual post-resolution {fault} boundary not reached"
            );
            assert!(result.is_err(), "late {fault} must not return a candidate");
            assert_eq!(storage(&f.db), *injected.borrow().as_ref().unwrap());
            assert_eq!(storage(engine.db()), *injected.borrow().as_ref().unwrap());
            if fault == "git" {
                assert_eq!(
                    f.target
                        .find_reference("refs/heads/feature")
                        .unwrap()
                        .target(),
                    Some(pending)
                );
                assert_eq!(
                    files(f.target.path()),
                    *retained_git.borrow().as_ref().unwrap()
                );
            }
            if fault == "svn" {
                assert!(
                    retained_svn.get() > current.manifest().observed_svn.inspected_head_revision
                );
                assert_eq!(
                    svn_revision(&local_source_url(&f, "branches/f")),
                    retained_svn.get()
                );
                let value = run(Command::new("svn").args([
                    "propget",
                    "unreviewed:resolution",
                    ready.child_wc.to_str().unwrap(),
                    "--non-interactive",
                    "--no-auth-cache",
                ]));
                assert_eq!(value.trim(), "retained");
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fresh_candidate_capture_never_adopts_pending_git_or_svn_work() {
        let _serial = SERIAL.lock().await;
        let f = Fixture::new();
        let engine = f.engine();
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let ready = ready(&f, &engine).await;
        let current = preview(&f).await.unwrap();
        let intent = request(&current, ChangedChildChoice::Child);
        let tip = Oid::from_str(&current.manifest().graph.roles.current_child.commit).unwrap();
        let pending = commit(&f.target, b"pending candidate Git work\n", &[tip]);
        f.target
            .reference(
                "refs/heads/feature",
                pending,
                true,
                "pending candidate fault",
            )
            .unwrap();
        let before = storage(&f.db);
        let target = files(f.target.path());
        assert!(resolve(&f, &intent).await.is_err());
        assert_eq!(storage(&f.db), before);
        assert_eq!(files(f.target.path()), target);
        assert_eq!(
            f.target
                .find_reference("refs/heads/feature")
                .unwrap()
                .target(),
            Some(pending)
        );
        f.target
            .reference(
                "refs/heads/feature",
                tip,
                true,
                "restore disposable candidate ref",
            )
            .unwrap();
        svn(&["update"], Some(&ready.child_wc));
        std::fs::write(
            ready.child_wc.join("file.txt"),
            b"pending candidate SVN work\n",
        )
        .unwrap();
        svn(
            &[
                "commit",
                "--username",
                "m2d-fixture-author",
                "-m",
                "pending candidate work",
            ],
            Some(&ready.child_wc),
        );
        let revision = svn_revision(&local_source_url(&f, "branches/f"));
        let before = storage(&f.db);
        let wc = files(&ready.child_wc);
        assert!(resolve(&f, &intent).await.is_err());
        assert_eq!(storage(&f.db), before);
        assert_eq!(storage(engine.db()), before);
        assert_eq!(files(&ready.child_wc), wc);
        assert_eq!(svn_revision(&local_source_url(&f, "branches/f")), revision);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn candidate_capture_preserves_shared_storage_and_owned_source_lifetimes() {
        let _serial = SERIAL.lock().await;
        let f = Fixture::new();
        let engine = f.engine();
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let ready = ready(&f, &engine).await;
        let current = preview(&f).await.unwrap();
        let intent = request(&current, ChangedChildChoice::Child);
        let before = storage(&f.db);
        let target = files(f.target.path());
        let parent_wc = files(&f._parent_wc);
        let child_wc = files(&ready.child_wc);
        let parent_bridge = files(&ready.parent_bridge);
        let child_bridge = files(&ready.child_bridge);
        let requests = std::fs::read(&f.log).unwrap();
        let result = resolve(&f, &intent).await.unwrap();
        let repeated = resolve(&f, &intent).await.unwrap();
        assert_eq!(
            result.manifest().candidate_digest,
            repeated.manifest().candidate_digest
        );
        assert_eq!(result.manifest().graph, repeated.manifest().graph);
        assert!(!result.execution_eligible());
        let json = serde_json::to_value(result.manifest()).unwrap();
        assert_eq!(json["execution_eligible"], serde_json::json!(false));
        assert!(json.get("actor_approval").is_none());
        assert!(json.get("actor_policy").is_none());
        assert!(result
            .manifest()
            .graph
            .source_graph
            .context
            .actor_policy
            .is_none());
        let source = &result.applicability.source;
        let snapshots = source
            .snapshots
            .iter()
            .map(|s| s.path().parent().unwrap().to_owned())
            .collect::<Vec<_>>();
        let git = source.candidate.preview.git.repository().path().to_owned();
        assert_eq!(snapshots.len(), 4);
        assert!(snapshots.iter().all(|p| p.is_dir()));
        assert!(git.is_dir());
        let reads = std::fs::read(&f.log).unwrap();
        assert!(reads.starts_with(&requests));
        let added = std::str::from_utf8(&reads[requests.len()..]).unwrap();
        assert!(added.contains("git-upload-pack"));
        assert!(!added.contains("git-receive-pack"));
        assert_eq!(storage(&f.db), before);
        assert_eq!(storage(engine.db()), before);
        assert_eq!(files(f.target.path()), target);
        assert_eq!(files(&f._parent_wc), parent_wc);
        assert_eq!(files(&ready.child_wc), child_wc);
        assert_eq!(files(&ready.parent_bridge), parent_bridge);
        assert_eq!(files(&ready.child_bridge), child_bridge);
        drop(result);
        assert!(snapshots.iter().all(|p| !p.exists()));
        assert!(!git.exists());
        assert_eq!(
            repeated.manifest().applicability_manifest_digest,
            intent.manifest_digest
        );
        assert_eq!(current.manifest().manifest_digest, intent.manifest_digest);
    }
}

mod authenticated_review_service_journey {
    use super::*;
    use crate::db::pair_refresh_operations::{PairRefreshOperation, PairRefreshPhase};
    use crate::pair_refresh::service::{
        RefreshReviewCandidateRequest, RefreshReviewResponse, RefreshService,
    };
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Condvar, Mutex};

    // Derived qualified setup helpers, adding two distinct uncontested files.
    // Real receipt/publication and ordinary cycles create all mapping evidence.
    type Storage = BTreeMap<String, Vec<Vec<rusqlite::types::Value>>>;

    fn storage(db: &Database) -> Storage {
        use rusqlite::types::Value;
        let conn = db.conn();
        let mut snapshot = BTreeMap::new();
        let schema = conn
            .prepare("SELECT type,name,tbl_name,rootpage,sql FROM sqlite_master ORDER BY type,name")
            .unwrap()
            .query_map([], |row| {
                (0..5)
                    .map(|i| row.get::<_, Value>(i))
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        snapshot.insert("schema".into(), schema);
        let tables = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        for table in tables {
            let sql = format!("SELECT * FROM \"{}\"", table.replace('"', "\"\""));
            let mut query = conn.prepare(&sql).unwrap();
            let columns = query.column_count();
            let mut values = query
                .query_map([], |row| {
                    (0..columns)
                        .map(|i| row.get::<_, Value>(i))
                        .collect::<rusqlite::Result<Vec<_>>>()
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            values.sort_by_cached_key(|row| format!("{row:?}"));
            snapshot.insert(table, values);
        }
        snapshot
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

    async fn completed(
        f: &Fixture,
        engine: &crate::sync_engine::SyncEngine,
        request: &str,
    ) -> PairRefreshOperation {
        let (actor, _workspace, value) = prepared(f, engine).await;
        let approved = actor.prepared_approval_digest(engine, &value).unwrap();
        let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
        let op = super::super::super::execution::complete_owned_refresh_with_client(
            engine,
            &actor,
            &value,
            request,
            &approved,
            &busy,
            f.client.clone(),
        )
        .await
        .unwrap();
        assert_eq!(op.phase, PairRefreshPhase::Completed);
        assert_eq!(f.db.pair_refresh_operation(op.id).unwrap(), op);
        op
    }

    fn producer_engine(f: &Fixture, id: &str) -> (crate::sync_engine::SyncEngine, PathBuf) {
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
        config.daemon.data_dir = f.temp.path().to_owned();
        let svn_branch = if id == "child" { "branches/f" } else { "trunk" };
        let svn_url = format!("{}/{svn_branch}", f.root);
        config.svn.url = svn_url.clone();
        config.svn.layout = crate::config::SvnLayout::Custom;
        config.svn.password = Some(String::new());
        config.github.default_branch = branch.to_owned();
        config.github.token = Some(String::new());
        config.sync.auto_merge = false;
        let mut engine = crate::sync_engine::SyncEngine::new(
            config,
            Database::new(f.temp.path().join("reposync.db")).unwrap(),
            crate::svn::SvnClient::new(svn_url, "", ""),
            crate::git::GitClient::new(&path).unwrap(),
            std::sync::Arc::new(
                crate::identity::mapper::IdentityMapper::new(&crate::config::IdentityConfig {
                    email_domain: Some("example.invalid".into()),
                    ..Default::default()
                })
                .unwrap(),
            ),
        );
        engine.set_repo_id(id.to_owned());
        engine.set_git_credential_apply_clean(true);
        (engine, path)
    }

    fn parent_install_bootstrap(f: &Fixture, history: &PairRefreshOperation) {
        use crate::db::repo_scope_identity as identity;

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
            crate::echo_receipt_scope::read_team_inbound_git_checkpoint_kv(&f.db, "parent")
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
        let local = crate::svn::SvnClient::new(&local_url, "", "");
        let entries = local.strict_log_range(revision, revision).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].revision, revision);
        assert_eq!(entries[0].author, "m2d-fixture-author");
        let registered = crate::svn::SvnClient::new(format!("{}/{branch}", f.root), "", "");
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

    async fn ready(f: &Fixture, engine: &crate::sync_engine::SyncEngine) -> Ready {
        let fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
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

    fn install_bridges(f: &Fixture) -> [PathBuf; 2] {
        let configuration =
            f.db.capture_pair_refresh_execution_configuration("child")
                .unwrap();
        let mut paths = Vec::new();
        for (id, branch) in [("child", "feature"), ("parent", "main")] {
            let path = f.temp.path().join("repos").join(id).join("git-repo");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let repository = git2::build::RepoBuilder::new()
                .branch(branch)
                .clone(f.target.path().to_str().unwrap(), &path)
                .unwrap();
            repository
                .remote_set_url("origin", &configuration.pin().git_remote_url)
                .unwrap();
            paths.push(path);
        }
        paths.try_into().unwrap()
    }

    fn intent(response: &RefreshReviewResponse, side: &str) -> RefreshReviewCandidateRequest {
        let manifest = response.applicability.as_ref().unwrap();
        let choices = manifest["graph"]["paths"].as_array().unwrap().iter()
            .filter(|row| row["disposition"]["kind"] == "conflict")
            .map(|row| serde_json::json!({
                "path":row["path"], "baseline":row["baseline"], "child":row["child"], "parent":row["parent"], "choice":side
            })).collect::<Vec<_>>();
        assert_eq!(
            choices.len(),
            1,
            "real two-side changes must expose a conflict"
        );
        serde_json::from_value(serde_json::json!({
            "review_pin": response.review_pin,
            "resolution": {"schema":1,"policy":crate::pair_refresh::preparation::CHANGED_CHILD_RESOLUTION_CANDIDATE_POLICY,"manifest_digest":manifest["manifest_digest"],"choices":choices}
        })).unwrap()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn registered_review_service_pins_complete_choices_without_writes() {
        let _serial = SERIAL.lock().await;
        let f = Fixture::new();
        let engine = Arc::new(f.engine());
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let ready = ready(&f, &engine).await;
        assert_eq!(ready.history.phase, PairRefreshPhase::Completed);
        assert!(
            ready.child_wc.is_dir() && ready.parent_bridge.is_dir() && ready.child_bridge.is_dir()
        );
        let bridges = install_bridges(&f);
        let indexes = bridges
            .iter()
            .map(|path| std::fs::read(path.join(".git/index")).unwrap())
            .collect::<Vec<_>>();
        let before = storage(&f.db);
        let source_before = fixture_files(f.target.path());
        let svn_before = svn_revision(&f.root);
        let log_before = std::fs::read_to_string(&f.log).unwrap();
        let service = RefreshService::with_fixture_client(engine.clone(), f.client.clone());
        let response = service
            .review("disposable-workspace-session".into(), "child".into())
            .await
            .unwrap();
        assert_eq!(response.state, "ready");
        assert!(response.blocked.is_empty());
        assert!(!response.actor_approval && !response.execution_eligible);
        assert_eq!(response.pending.child.git.pending_count, Some(0));
        assert_eq!(response.pending.parent.git.pending_count, Some(0));
        assert!(response
            .pending
            .child
            .svn
            .observed_pending_revisions
            .is_empty());
        assert!(response
            .pending
            .parent
            .svn
            .observed_pending_revisions
            .is_empty());
        let manifest = response.applicability.as_ref().unwrap();
        for (side, role) in [
            ("base", "integrated_child"),
            ("child", "current_child"),
            ("parent", "current_parent"),
        ] {
            let result = service
                .review_candidate(
                    "disposable-workspace-session".into(),
                    "child".into(),
                    intent(&response, side),
                )
                .await
                .unwrap();
            assert_eq!(result.state, "candidate");
            assert!(!result.execution_eligible && !result.actor_approval);
            assert_eq!(result.review_pin, response.review_pin);
            let candidate = result.candidate.unwrap();
            assert_eq!(
                candidate["graph"]["candidate_tree"],
                expected_candidate_tree(&f, manifest, role)
            );
            assert_eq!(
                candidate["applicability_manifest_digest"],
                manifest["manifest_digest"]
            );
        }
        // A viewing pin has no matching execution/preparation identity.
        let refused = service
            .act(
                "disposable-workspace-session".into(),
                "child",
                "view-is-not-approval",
                Uuid::new_v4(),
                response.review_pin.as_ref().unwrap().clone(),
                crate::pair_refresh::service::RefreshAction::Execute,
            )
            .await;
        assert!(refused.is_err());
        service.shutdown().await;
        assert_eq!(
            service.review_shutdown_record(),
            crate::pair_refresh::service::ShutdownRecord::Completed
        );
        assert_eq!(storage(&f.db), before);
        assert_eq!(fixture_files(f.target.path()), source_before);
        assert_eq!(svn_revision(&f.root), svn_before);
        let log_after = std::fs::read_to_string(&f.log).unwrap();
        assert!(log_after.starts_with(&log_before));
        assert!(
            !log_after[log_before.len()..].contains("git-receive-pack"),
            "read phase must not attempt ordinary Git publication"
        );
        for (path, bytes) in bridges.iter().zip(indexes) {
            assert_eq!(std::fs::read(path.join(".git/index")).unwrap(), bytes);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn registered_review_reports_both_sides_unhandled_and_local_work_as_blocked() {
        let _serial = SERIAL.lock().await;
        let f = Fixture::new();
        let engine = Arc::new(f.engine());
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let ready = ready(&f, &engine).await;
        let bridges = install_bridges(&f);
        for branch in ["main", "feature"] {
            let reference = format!("refs/heads/{branch}");
            let old = f
                .target
                .find_reference(&reference)
                .unwrap()
                .target()
                .unwrap();
            let new = commit(&f.target, b"unhandled remote work\n", &[old]);
            f.target
                .reference(&reference, new, true, "unhandled fixture work")
                .unwrap();
        }
        for path in [&f._parent_wc, &ready.child_wc] {
            svn(&["update"], Some(path));
            std::fs::write(path.join("file.txt"), b"unhandled SVN work\n").unwrap();
            svn(
                &[
                    "commit",
                    "--username",
                    "review-fixture-author",
                    "-m",
                    "unhandled review work",
                ],
                Some(path),
            );
        }
        for path in &bridges {
            std::fs::write(path.join("untracked.txt"), b"preserve local work\n").unwrap();
        }
        let before = storage(&f.db);
        let service = RefreshService::with_fixture_client(engine, f.client.clone());
        let response = service
            .review("disposable-workspace-session".into(), "child".into())
            .await
            .unwrap();
        assert_eq!(response.state, "blocked");
        assert!(
            response.review_pin.is_none()
                && response.applicability.is_none()
                && response.candidate.is_none()
        );
        for side in [&response.pending.child, &response.pending.parent] {
            assert_ne!(
                side.git.observed_tip.as_deref(),
                Some(side.git.checkpoint.as_str())
            );
            assert_eq!(
                side.git.pending_count, None,
                "changed tip is not an invented commit count"
            );
            assert!(!side.git.complete && !side.svn.complete);
            assert!(!side.svn.observed_pending_revisions.is_empty());
            assert!(side
                .local_bridge
                .paths
                .iter()
                .any(|row| row.path == b"untracked.txt"));
            assert!(side.local_bridge.blocked.is_some());
        }
        service.shutdown().await;
        assert_eq!(storage(&f.db), before);
        for path in bridges {
            assert_eq!(
                std::fs::read(path.join("untracked.txt")).unwrap(),
                b"preserve local work\n"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn registered_review_rechecks_actor_and_local_drift_after_observation() {
        let _serial = SERIAL.lock().await;
        let f = Fixture::new();
        let engine = Arc::new(f.engine());
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let _ready = ready(&f, &engine).await;
        let bridges = install_bridges(&f);
        let service = RefreshService::with_fixture_client(engine.clone(), f.client.clone());
        let path = bridges[0].join("late.txt");
        let written = path.clone();
        let drift = service
            .review_at_boundary(
                "disposable-workspace-session".into(),
                "child".into(),
                Arc::new(move || {
                    std::fs::write(&written, b"late local change").unwrap();
                }),
            )
            .await;
        assert!(
            drift.is_err(),
            "late local work must not escape as a ready response"
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"late local change");
        std::fs::remove_file(path).unwrap(); // Fixture-owned mutation, not product cleanup.
        let db = Database::new(f.temp.path().join("reposync.db")).unwrap();
        let revoked = service
            .review_at_boundary(
                "disposable-workspace-session".into(),
                "child".into(),
                Arc::new(move || {
                    assert_eq!(
                        db.conn()
                            .execute("UPDATE users SET enabled=0 WHERE id='admin'", [])
                            .unwrap(),
                        1
                    );
                }),
            )
            .await;
        assert!(
            revoked.is_err(),
            "a revoked viewer cannot receive a post-observation result"
        );
        service.shutdown().await;
    }

    struct Gate {
        entered: AtomicBool,
        released: Mutex<bool>,
        changed: Condvar,
    }
    impl Gate {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                entered: AtomicBool::new(false),
                released: Mutex::new(false),
                changed: Condvar::new(),
            })
        }
        fn wait(&self) {
            self.entered.store(true, Ordering::Release);
            let mut released = self.released.lock().unwrap();
            while !*released {
                released = self.changed.wait(released).unwrap();
            }
        }
        fn release(&self) {
            *self.released.lock().unwrap_or_else(|p| p.into_inner()) = true;
            self.changed.notify_all();
        }
    }
    struct ReleaseGates(Vec<Arc<Gate>>);
    impl Drop for ReleaseGates {
        fn drop(&mut self) {
            for gate in &self.0 {
                gate.release();
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn registered_review_disconnect_capacity_and_shutdown_retain_real_workers() {
        let _serial = SERIAL.lock().await;
        let f = Fixture::new();
        let engine = Arc::new(f.engine());
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let _ready = ready(&f, &engine).await;
        let _bridges = install_bridges(&f);
        let before = storage(&f.db);
        let service = RefreshService::with_fixture_client(engine, f.client.clone());
        let release = ReleaseGates((0..4).map(|_| Gate::new()).collect());
        let mut observers = Vec::new();
        for gate in &release.0 {
            let gate = gate.clone();
            let service = service.clone();
            observers.push(tokio::spawn(async move {
                service
                    .review_at_boundary(
                        "disposable-workspace-session".into(),
                        "child".into(),
                        Arc::new(move || gate.wait()),
                    )
                    .await
            }));
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !release
            .0
            .iter()
            .all(|gate| gate.entered.load(Ordering::Acquire))
        {
            assert!(
                std::time::Instant::now() < deadline,
                "real source workers did not reach controlled boundary"
            );
            assert!(observers.iter().all(|task| !task.is_finished()));
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(service
            .review("disposable-workspace-session".into(), "child".into())
            .await
            .is_err());
        for task in &observers {
            task.abort();
        }
        for task in observers {
            match task.await {
                Err(error) => assert!(error.is_cancelled()),
                Ok(_) => panic!("observer unexpectedly completed before release"),
            }
        }
        let shutdown = {
            let service = service.clone();
            tokio::spawn(async move { service.shutdown().await })
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while service.review_shutdown_record() == crate::pair_refresh::service::ShutdownRecord::Idle
        {
            assert!(std::time::Instant::now() < deadline);
            tokio::task::yield_now().await;
        }
        assert!(
            !shutdown.is_finished(),
            "receiver loss is not real worker completion"
        );
        assert_eq!(
            service.review_shutdown_record(),
            crate::pair_refresh::service::ShutdownRecord::Pending
        );
        assert!(service
            .review("disposable-workspace-session".into(), "child".into())
            .await
            .is_err());
        drop(release);
        tokio::time::timeout(std::time::Duration::from_secs(60), shutdown)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            service.review_shutdown_record(),
            crate::pair_refresh::service::ShutdownRecord::Completed
        );
        assert_eq!(storage(&f.db), before);
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn registered_review_refuses_suppressed_tracked_and_sparse_indexes_without_repairs() {
        let _serial = SERIAL.lock().await;
        let f = Fixture::new();
        let engine = Arc::new(f.engine());
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let _ready = ready(&f, &engine).await;
        let bridges = install_bridges(&f);
        let service = RefreshService::with_fixture_client(engine, f.client.clone());
        assert_eq!(
            service
                .review("disposable-workspace-session".into(), "child".into())
                .await
                .unwrap()
                .state,
            "ready"
        );
        let db_before = storage(&f.db);
        for (role, bridge) in [("child", &bridges[0]), ("parent", &bridges[1])] {
            let index_path = bridge.join(".git/index");
            let config_path = bridge.join(".git/config");
            let tracked_path = bridge.join("file.txt");
            let original_index = std::fs::read(&index_path).unwrap();
            let original_config = std::fs::read(&config_path).unwrap();
            let original_file = std::fs::read(&tracked_path).unwrap();
            let configuration =
                f.db.capture_pair_refresh_execution_configuration("child")
                    .unwrap();
            let pin = configuration.pin();
            let (branch, expected) = if role == "child" {
                ("feature", pin.child.last_git_sha.as_str())
            } else {
                ("main", pin.parent.last_git_sha.as_str())
            };
            for variant in [
                "assume_unchanged",
                "skip_worktree",
                "sparse_policy",
                "ordinary_bytes",
            ] {
                // Restore fixture-owned bytes only between independent faults.
                // The reader must never clear these flags or repair user work.
                std::fs::write(&index_path, &original_index).unwrap();
                std::fs::write(&config_path, &original_config).unwrap();
                std::fs::write(&tracked_path, &original_file).unwrap();
                let repository = Repository::open(bridge).unwrap();
                let mut index = repository.index().unwrap();
                let mut entry = index.get_path(Path::new("file.txt"), 0).unwrap();
                match variant {
                    "assume_unchanged" => entry.flags |= git2::IndexEntryFlag::VALID.bits(),
                    "skip_worktree" => {
                        entry.flags |= git2::IndexEntryFlag::EXTENDED.bits();
                        entry.flags_extended |= git2::IndexEntryExtendedFlag::SKIP_WORKTREE.bits();
                    }
                    "sparse_policy" => repository
                        .config()
                        .unwrap()
                        .set_bool("core.sparseCheckout", true)
                        .unwrap(),
                    _ => {}
                }
                index.add(&entry).unwrap();
                index.write().unwrap();
                index.read(true).unwrap();
                let persisted = index.get_path(Path::new("file.txt"), 0).unwrap();
                if variant == "assume_unchanged" {
                    assert_ne!(persisted.flags & git2::IndexEntryFlag::VALID.bits(), 0);
                }
                if variant == "skip_worktree" {
                    assert_ne!(
                        persisted.flags_extended
                            & git2::IndexEntryExtendedFlag::SKIP_WORKTREE.bits(),
                        0
                    );
                }
                // Pure policy control: a sparse-directory entry is unsupported.
                // This is not a claim to have generated a real sparse-index file.
                let mut sparse = index.get_path(Path::new("file.txt"), 0).unwrap();
                sparse.flags &= 0x0fff;
                sparse.flags_extended = 0;
                sparse.mode = 0o040000;
                assert!(!super::super::review_supported_index_entry(&sparse));
                let mut changed_file = original_file.clone();
                changed_file[0] ^= 1; // Same byte length; still real tracked work.
                std::fs::write(&tracked_path, &changed_file).unwrap();
                let before_index = std::fs::read(&index_path).unwrap();
                let before_config = std::fs::read(&config_path).unwrap();
                let before_head = repository.head().unwrap().target();
                for _ in 0..2 {
                    assert!(
                        super::super::review_local_bridge(
                            f.temp.path(),
                            role,
                            branch,
                            &pin.git_remote_url,
                            Some(expected)
                        )
                        .is_err(),
                        "{role}/{variant} cannot be a complete clean observation"
                    );
                    assert_eq!(std::fs::read(&index_path).unwrap(), before_index);
                }
                let response = service
                    .review("disposable-workspace-session".into(), "child".into())
                    .await
                    .unwrap();
                assert_eq!(response.state, "blocked", "{role}/{variant}");
                let observed = if role == "child" {
                    &response.pending.child
                } else {
                    &response.pending.parent
                };
                assert!(!observed.local_bridge.complete);
                assert!(observed.local_bridge.blocked.is_some());
                assert!(response.review_pin.is_none() && response.candidate.is_none());
                // Blocking a different source must not manufacture SVN absence:
                // no strict correspondence proof ran, so completeness is unknown.
                for side in [&response.pending.child, &response.pending.parent] {
                    assert!(!side.svn.complete);
                    assert!(side.svn.visibility.is_none());
                }
                assert_eq!(std::fs::read(&index_path).unwrap(), before_index);
                assert_eq!(std::fs::read(&config_path).unwrap(), before_config);
                assert_eq!(std::fs::read(&tracked_path).unwrap(), changed_file);
                assert_eq!(repository.head().unwrap().target(), before_head);
                assert_eq!(storage(&f.db), db_before);
            }
            std::fs::write(index_path, original_index).unwrap();
            std::fs::write(config_path, original_config).unwrap();
            std::fs::write(tracked_path, original_file).unwrap();
        }
        svn(
            &[
                "mkdir",
                &format!("{}/unrelated-review", f.root),
                "-m",
                "unrelated repository head",
            ],
            None,
        );
        let head = svn_revision(&f.root);
        let fresh = service
            .review("disposable-workspace-session".into(), "child".into())
            .await
            .unwrap();
        assert_eq!(fresh.state, "ready");
        for (role, side) in [("C", &fresh.pending.child), ("P2", &fresh.pending.parent)] {
            assert!(side.svn.complete && side.svn.observed_pending_revisions.is_empty());
            let proof = side.svn.visibility.as_ref().unwrap();
            assert_eq!(proof.role, role);
            assert_eq!(proof.revision, head);
            assert!(proof.max_changed_revision <= side.svn.checkpoint);
        }
        assert_eq!(storage(&f.db), db_before);
        service.shutdown().await;
        assert_eq!(
            service.review_shutdown_record(),
            crate::pair_refresh::service::ShutdownRecord::Completed
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn registered_review_uses_owner_execute_with_filemode_false_without_repairs() {
        use std::os::unix::fs::PermissionsExt;

        let _serial = SERIAL.lock().await;
        let f = Fixture::new();
        let engine = Arc::new(f.engine());
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let _ready = ready(&f, &engine).await;
        // Genuine descendant commits change only the disposable remote's mode.
        // They deliberately remain unhandled: never invent executable mappings
        // or claim default-refresh eligibility merely to test a local observer.
        let mut tips = BTreeMap::new();
        for (id, branch) in [("child", "feature"), ("parent", "main")] {
            let reference = format!("refs/heads/{branch}");
            let parent = f
                .target
                .find_commit(
                    f.target
                        .find_reference(&reference)
                        .unwrap()
                        .target()
                        .unwrap(),
                )
                .unwrap();
            let old_tree = parent.tree().unwrap();
            let mut builder = f.target.treebuilder(Some(&old_tree)).unwrap();
            builder
                .insert(
                    "file.txt",
                    old_tree.get_name("file.txt").unwrap().id(),
                    0o100755,
                )
                .unwrap();
            let tree = f.target.find_tree(builder.write().unwrap()).unwrap();
            let signature = Signature::new(
                "Disposable executable fixture",
                "fixture@example.invalid",
                &Time::new(1_790_000_002, 0),
            )
            .unwrap();
            let tip = f
                .target
                .commit(
                    None,
                    &signature,
                    &signature,
                    "unhandled executable-mode fixture",
                    &tree,
                    &[&parent],
                )
                .unwrap();
            f.target
                .reference(&reference, tip, true, "owned fixture descendant")
                .unwrap();
            tips.insert(id, tip.to_string());
        }
        let bridges = install_bridges(&f);
        let configuration =
            f.db.capture_pair_refresh_execution_configuration("child")
                .unwrap();
        let service = RefreshService::with_fixture_client(engine, f.client.clone());
        let db_before = storage(&f.db);
        let source_before = fixture_files(f.target.path());
        let svn_before = svn_revision(&f.root);
        for (role, branch, bridge) in [
            ("child", "feature", &bridges[0]),
            ("parent", "main", &bridges[1]),
        ] {
            let repository = Repository::open(bridge).unwrap();
            repository
                .config()
                .unwrap()
                .set_bool("core.filemode", false)
                .unwrap();
            assert!(!repository
                .config()
                .unwrap()
                .get_bool("core.filemode")
                .unwrap());
            let path = bridge.join("file.txt");
            let original = std::fs::read(&path).unwrap();
            let head = repository.head().unwrap().target();
            assert_eq!(head.unwrap().to_string(), tips[role]);
            assert_eq!(
                repository
                    .head()
                    .unwrap()
                    .peel_to_tree()
                    .unwrap()
                    .get_name("file.txt")
                    .unwrap()
                    .filemode(),
                0o100755
            );
            assert_eq!(
                repository
                    .index()
                    .unwrap()
                    .get_path(Path::new("file.txt"), 0)
                    .unwrap()
                    .mode,
                0o100755
            );
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            let index_before = std::fs::read(bridge.join(".git/index")).unwrap();
            let config_before = std::fs::read(bridge.join(".git/config")).unwrap();
            let positive = super::super::review_local_bridge(
                f.temp.path(),
                role,
                branch,
                &configuration.pin().git_remote_url,
                Some(tips[role].as_str()),
            )
            .unwrap();
            assert!(
                positive.complete && positive.blocked.is_none(),
                "supported executable local observation"
            );
            assert_eq!(
                std::fs::read(bridge.join(".git/index")).unwrap(),
                index_before
            );
            assert_eq!(std::fs::read(&path).unwrap(), original);
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o755
            );
            for mode in [0o654, 0o645] {
                // Only the fixture chmods. Group/other execute is retained while
                // owner execute is absent; bytes and genuine HEAD/index match.
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
                assert_eq!(mode & 0o100, 0);
                assert_ne!(mode & 0o011, 0);
                let mut options = git2::StatusOptions::new();
                options.no_refresh(true).update_index(false);
                assert!(
                    repository.statuses(Some(&mut options)).unwrap().is_empty(),
                    "core.filemode=false must exercise status-suppressed mode difference"
                );
                assert_eq!(std::fs::read(&path).unwrap(), original);
                assert!(super::super::review_local_bridge(
                    f.temp.path(),
                    role,
                    branch,
                    &configuration.pin().git_remote_url,
                    Some(tips[role].as_str())
                )
                .is_err());
                let response = service
                    .review("disposable-workspace-session".into(), "child".into())
                    .await
                    .unwrap();
                assert_eq!(response.state, "blocked");
                let observed = if role == "child" {
                    &response.pending.child
                } else {
                    &response.pending.parent
                };
                assert!(!observed.local_bridge.complete && observed.local_bridge.blocked.is_some());
                assert!(
                    response.review_pin.is_none()
                        && response.applicability.is_none()
                        && response.candidate.is_none()
                );
                assert!(!response.actor_approval && !response.execution_eligible);
                assert_eq!(std::fs::read(&path).unwrap(), original);
                assert_eq!(
                    std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                    mode
                );
                assert_eq!(
                    std::fs::read(bridge.join(".git/index")).unwrap(),
                    index_before
                );
                assert_eq!(
                    std::fs::read(bridge.join(".git/config")).unwrap(),
                    config_before
                );
                assert_eq!(repository.head().unwrap().target(), head);
                assert_eq!(storage(&f.db), db_before);
                assert_eq!(fixture_files(f.target.path()), source_before);
                assert_eq!(svn_revision(&f.root), svn_before);
            }
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            let restored = super::super::review_local_bridge(
                f.temp.path(),
                role,
                branch,
                &configuration.pin().git_remote_url,
                Some(tips[role].as_str()),
            )
            .unwrap();
            assert!(restored.complete && restored.blocked.is_none());
            assert_eq!(
                std::fs::read(bridge.join(".git/index")).unwrap(),
                index_before
            );
            assert_eq!(
                std::fs::read(bridge.join(".git/config")).unwrap(),
                config_before
            );
        }
        service.shutdown().await;
        assert_eq!(
            service.review_shutdown_record(),
            crate::pair_refresh::service::ShutdownRecord::Completed
        );
    }
}

mod review_svn_intake_completeness {
    use super::super::review_svn_pending;
    use super::fixture_process as process_fixture;
    use crate::svn::SvnClient;
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use std::time::Duration;

    // Exact existing disposable svnserve setup; no server-policy or OS-function
    // replacement is injected into production. Authz files are fixture-owned.
    struct Fixture {
        // Stop and reap the server before removing its repository.
        _server: process_fixture::OwnedProcessGroup,
        root: tempfile::TempDir,
        client: SvnClient,
        repository: PathBuf,
    }

    fn command(args: &[&str]) -> String {
        let output = Command::new(args[0]).args(&args[1..]).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    impl Fixture {
        async fn new(hidden: bool) -> Self {
            let root = tempfile::tempdir().unwrap();
            let repository = root.path().join("repo");
            command(&["svnadmin", "create", repository.to_str().unwrap()]);
            let seed = root.path().join("seed");
            std::fs::create_dir_all(seed.join("trunk/hidden-dir")).unwrap();
            std::fs::write(seed.join("trunk/visible"), "visible").unwrap();
            std::fs::write(seed.join("trunk/hidden-file"), "hidden-file").unwrap();
            std::fs::write(seed.join("trunk/hidden-dir/nested"), "hidden-dir").unwrap();
            command(&[
                "svn",
                "import",
                seed.to_str().unwrap(),
                reqwest::Url::from_directory_path(&repository)
                    .unwrap()
                    .as_str(),
                "-m",
                "disposable visibility fixture",
                "--non-interactive",
            ]);
            std::fs::write(repository.join("conf/svnserve.conf"),
            "[general]\nanon-access = none\nauth-access = write\npassword-db = passwd\nauthz-db = authz\nrealm = disposable visibility\n").unwrap();
            std::fs::write(
                repository.join("conf/passwd"),
                "[users]\nreader = disposable-reader\n",
            )
            .unwrap();
            let authz = if hidden {
                "[repo:/]\nreader = r\n[repo:/trunk/hidden-dir]\nreader =\n[repo:/trunk/hidden-file]\nreader =\n"
            } else {
                "[repo:/]\nreader = r\n"
            };
            std::fs::write(repository.join("conf/authz"), authz).unwrap();
            let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let port = listener.local_addr().unwrap().port();
            drop(listener);
            let server = process_fixture::OwnedProcessGroup::spawn(
                Command::new("svnserve")
                    .args([
                        "-d",
                        "--foreground",
                        "--listen-host",
                        "127.0.0.1",
                        "--listen-port",
                        &port.to_string(),
                        "-r",
                        root.path().to_str().unwrap(),
                    ])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null()),
            )
            .unwrap();
            let client = SvnClient::new_isolated_refresh(
                &format!("svn://127.0.0.1:{port}/repo/trunk"),
                "reader",
                Some("disposable-reader"),
            )
            .unwrap();
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            loop {
                if client.info().await.is_ok() {
                    break;
                }
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "owned fixture server unavailable"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Self {
                _server: server,
                root,
                client,
                repository,
            }
        }

        fn youngest(&self) -> String {
            command(&["svnlook", "youngest", self.repository.to_str().unwrap()])
        }
    }
    fn add_unrelated_revision(f: &Fixture) {
        let url = reqwest::Url::from_directory_path(&f.repository)
            .unwrap()
            .join("unrelated")
            .unwrap();
        command(&[
            "svn",
            "mkdir",
            url.as_str(),
            "-m",
            "unrelated repository work",
            "--non-interactive",
        ]);
        assert_eq!(f.youngest().trim(), "2");
    }

    #[tokio::test]
    async fn filtered_visible_older_history_cannot_become_complete_no_work() {
        let _serial = super::SERIAL.lock().await;
        let f = Fixture::new(true).await;
        add_unrelated_revision(&f);
        let visible = f.client.copy_history_at_rev(2).await.unwrap();
        assert!(
            !visible.is_empty(),
            "exercise an internally valid nonempty visible list"
        );
        assert!(visible.iter().all(|entry| entry.revision <= 1));
        let before = f.youngest();
        let observation = review_svn_pending(&f.client, f.client.url(), 1)
            .await
            .unwrap();
        assert_eq!(observation.observed_head, Some(2));
        assert!(observation.observed_pending_revisions.is_empty());
        assert!(
            !observation.complete,
            "empty visible pending list is not visibility proof"
        );
        assert!(observation.visibility.is_none());
        assert_eq!(
            observation.blocked.as_deref(),
            Some("subtree_visibility_unproved")
        );
        assert_eq!(f.youngest(), before);
    }

    #[tokio::test]
    async fn proven_unrelated_head_and_real_visible_pending_are_distinct() {
        let _serial = super::SERIAL.lock().await;
        let f = Fixture::new(false).await;
        add_unrelated_revision(&f);
        let before = f.youngest();
        let observation = review_svn_pending(&f.client, f.client.url(), 1)
            .await
            .unwrap();
        assert!(
            !observation.complete,
            "raw history alone remains incomplete even on a visible server"
        );
        assert_eq!(observation.observed_head, Some(2));
        assert!(observation.observed_pending_revisions.is_empty());
        assert!(observation.visibility.is_none());
        // Independent native evidence proves this is genuinely unrelated work,
        // not an invented HEAD-minus-checkpoint pending count. Only the full
        // registered correspondence factory may promote the service response.
        let checkout = crate::pair_refresh::svn_visibility::observe_subtree(&f.client, 2)
            .await
            .unwrap();
        checkout.require_unchanged_since(1).unwrap();
        assert_eq!(checkout.info().latest_rev, 2);
        assert_eq!(f.youngest(), before);
        let admin = f.root.path().join("admin");
        let url = reqwest::Url::from_directory_path(&f.repository)
            .unwrap()
            .join("trunk")
            .unwrap();
        command(&[
            "svn",
            "checkout",
            url.as_str(),
            admin.to_str().unwrap(),
            "--non-interactive",
        ]);
        std::fs::write(admin.join("visible"), "actual pending work").unwrap();
        command(&[
            "svn",
            "commit",
            admin.to_str().unwrap(),
            "-m",
            "visible branch change",
            "--non-interactive",
        ]);
        assert_eq!(f.youngest().trim(), "3");
        let before = f.youngest();
        let pending = review_svn_pending(&f.client, f.client.url(), 1)
            .await
            .unwrap();
        assert_eq!(pending.observed_pending_revisions, vec![3]);
        assert!(!pending.complete);
        assert!(pending.visibility.is_none() && pending.blocked.is_some());
        assert_eq!(f.youngest(), before);
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn blocked_other_source_still_reports_unproved_svn_truthfully() {
        let _serial = super::SERIAL.lock().await;
        let f = super::Fixture::new();
        let engine = std::sync::Arc::new(f.engine());
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
        let repository = f.temp.path().join("svn-repository");
        std::fs::write(repository.join("conf/svnserve.conf"), "[general]\nanon-access = none\nauth-access = read\npassword-db = passwd\nauthz-db = authz\nrealm = disposable read intake\n").unwrap();
        std::fs::write(
            repository.join("conf/passwd"),
            "[users]\nreader = disposable-reader\n",
        )
        .unwrap();
        std::fs::write(repository.join("conf/authz"), "[svn-repository:/]\nreader = r\n[svn-repository:/trunk/file.txt]\nreader =\n[svn-repository:/branches/f/file.txt]\nreader =\n").unwrap();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let _server = process_fixture::OwnedProcessGroup::spawn(
            Command::new("svnserve")
                .args([
                    "-d",
                    "--foreground",
                    "--listen-host",
                    "127.0.0.1",
                    "--listen-port",
                    &port.to_string(),
                    "-r",
                    f.temp.path().to_str().unwrap(),
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        )
        .unwrap();
        let root = format!("svn://127.0.0.1:{port}/svn-repository");
        let reader = SvnClient::new_isolated_refresh(
            &format!("{root}/trunk"),
            "reader",
            Some("disposable-reader"),
        )
        .unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            if reader.info().await.is_ok() {
                break;
            }
            assert!(tokio::time::Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // Real fixture-only re-registration, not fabricated history/mappings.
        // No applicable historical capability is needed for a blocked report.
        assert_eq!(f.db.conn().execute("UPDATE repositories SET svn_url=?1,svn_username='reader' WHERE id IN ('child','parent')", [&root]).unwrap(), 2);
        for id in ["child", "parent"] {
            f.db.set_state(&format!("secret_svn_password_{id}"), "disposable-reader")
                .unwrap();
        }
        assert!(!f.temp.path().join("repos/child/git-repo").exists());
        let before = f.state();
        let revision = command(&["svnlook", "youngest", repository.to_str().unwrap()]);
        let service = crate::pair_refresh::service::RefreshService::with_fixture_client(
            engine,
            f.client.clone(),
        );
        let response = service
            .review("disposable-workspace-session".into(), "child".into())
            .await
            .unwrap();
        assert_eq!(response.state, "blocked");
        assert!(!response.pending.child.local_bridge.complete);
        for side in [&response.pending.child, &response.pending.parent] {
            assert!(
                !side.svn.complete,
                "a different blocked source is not permission to claim SVN completeness"
            );
            assert!(side.svn.visibility.is_none() && side.svn.blocked.is_some());
        }
        assert!(
            response.applicability.is_none()
                && response.candidate.is_none()
                && response.review_pin.is_none()
        );
        service.shutdown().await;
        assert_eq!(f.state(), before);
        assert_eq!(
            command(&["svnlook", "youngest", repository.to_str().unwrap()]),
            revision
        );
    }
}
