//! Owned workspace preparation against disposable HTTPS Git and real SVN.
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use git2::{Oid, Repository, Signature, Time};
use uuid::Uuid;

#[path = "../../tests/fixtures/owned_process_group.rs"]
#[allow(clippy::duplicate_mod)]
mod fixture_process;

use super::*;

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
    parent_wc: PathBuf,
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
            parent_wc,
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prepared_objects_and_working_copy_share_one_owned_retained_resource_root() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = f.engine();
    let context = engine
        .capture_pair_refresh_actor_policy("disposable-workspace-session")
        .unwrap();
    let workspace = context.create_workspace(&engine).unwrap();
    let root = workspace.pin().operation_directory.path.clone();
    let state = f.state();
    let prepared = prepare_registered_in_workspace_for_test(
        &f.db,
        "child",
        workspace.clone(),
        f.client.clone(),
    )
    .await
    .unwrap();
    context.require_current(&engine).unwrap();
    assert_eq!(
        prepared.pins().git.operation_id,
        workspace.pin().operation_id
    );
    assert_eq!(
        prepared.pins().svn.operation_id,
        workspace.pin().operation_id
    );
    assert!(prepared
        .git()
        .repository()
        .path()
        .starts_with(root.join("git")));
    assert!(prepared
        .pins()
        .svn
        .working_copy
        .starts_with(root.join("svn")));
    assert_eq!(
        prepared
            .git()
            .repository()
            .find_commit(Oid::from_str(&prepared.pins().git.prepared_sha).unwrap())
            .unwrap()
            .parent_count(),
        2
    );
    assert!(prepared.pins().svn.working_copy.join(".svn").is_dir());
    assert_eq!(f.state(), state);
    context
        .retain_workspace_before_enrollment(&engine, &workspace)
        .unwrap();
    let commit = prepared.pins().git.prepared_sha.clone();
    let git_path = prepared.git().repository().path().to_owned();
    let wc = prepared.pins().svn.working_copy.clone();
    drop(workspace);
    assert!(root.is_dir());
    drop(prepared);
    assert!(wc.join(".svn").is_dir());
    let reopened = Repository::open_bare(&git_path).unwrap();
    assert!(reopened
        .find_commit(Oid::from_str(&commit).unwrap())
        .is_ok());
    assert_eq!(f.state(), state);
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
async fn source_drift_refuses_owned_preparation_without_a_job_or_publication() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = f.engine();
    let context = engine
        .capture_pair_refresh_actor_policy("disposable-workspace-session")
        .unwrap();
    let workspace = context.create_workspace(&engine).unwrap();
    let root = workspace.pin().operation_directory.path.clone();
    std::fs::write(f.parent_wc.join("file.txt"), b"new unhandled SVN change\n").unwrap();
    svn(&["commit", "-m", "unhandled"], Some(&f.parent_wc));
    let state = f.state();
    assert!(prepare_registered_in_workspace_for_test(
        &f.db,
        "child",
        workspace.clone(),
        f.client.clone()
    )
    .await
    .is_err());
    assert_eq!(f.state(), state);
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
        5
    );
    assert!(!std::fs::read_to_string(&f.log)
        .unwrap()
        .contains("receive-pack"));
    drop(workspace);
    assert!(!root.exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replaced_git_resource_role_is_not_used_retained_or_deleted() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = f.engine();
    let context = engine
        .capture_pair_refresh_actor_policy("disposable-workspace-session")
        .unwrap();
    let workspace = context.create_workspace(&engine).unwrap();
    let root = workspace.pin().operation_directory.path.clone();
    let prepared = prepare_registered_in_workspace_for_test(
        &f.db,
        "child",
        workspace.clone(),
        f.client.clone(),
    )
    .await
    .unwrap();
    let resources = prepared.resource_pin().unwrap();
    assert_eq!(
        resources
            .directories
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec!["git", "svn", "svn-operation", "svn-working-copy"]
    );
    let git = resources.directories["git"].path.clone();
    let moved = git.with_extension("moved");
    let original_commit = prepared.pins().git.prepared_sha.clone();
    std::fs::rename(&git, &moved).unwrap();
    let foreign = tempfile::tempdir().unwrap();
    std::fs::write(foreign.path().join("foreign-marker"), "foreign").unwrap();
    std::os::unix::fs::symlink(foreign.path(), &git).unwrap();
    assert!(prepared.resource_pin().is_err());
    assert!(context
        .retain_workspace_before_enrollment(&engine, &workspace)
        .is_err());
    drop(workspace);
    drop(prepared);
    assert!(root.is_dir());
    assert_eq!(
        std::fs::read_to_string(foreign.path().join("foreign-marker")).unwrap(),
        "foreign"
    );
    assert!(Repository::open_bare(&moved)
        .unwrap()
        .find_commit(Oid::from_str(&original_commit).unwrap())
        .is_ok());
}
