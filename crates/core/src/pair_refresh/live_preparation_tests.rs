//! Server-computed preparation against disposable HTTPS Git and real SVN.
//! Fixture publication is used only to obtain a real completed next baseline.
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use git2::{Oid, Repository, Signature, Time};
use uuid::Uuid;

#[path = "../../tests/fixtures/owned_process_group.rs"]
#[allow(clippy::duplicate_mod)]
mod fixture_process;

use super::super::git_sources::observe_registered_git_sources_for_test;
use super::super::svn_observation::{
    observe_prepared_child_svn, refresh_svn_commit_message, SvnPublicationObservation,
};
use super::*;

static SERIAL: super::authenticated_enrollment_tests::WriterTestAdmission =
    super::authenticated_enrollment_tests::WriterTestAdmission;

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
    _owner: crate::data_dir::DataDirOwner,
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
        let (db, owner) = Database::open_with_exclusive_owner(temp.path()).unwrap();
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
            _owner: owner,
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

    fn checkpoints(&self, id: &str, sha: Oid, revision: i64) {
        self.db
            .conn()
            .execute(
                "UPDATE repositories SET last_git_sha=?1,last_svn_rev=?2 WHERE id=?3",
                rusqlite::params![sha.to_string(), revision, id],
            )
            .unwrap();
        self.db
            .set_state(&format!("last_svn_rev_{id}"), &revision.to_string())
            .unwrap();
        crate::echo_receipt_scope::write_scoped_last_git_sha_kv(
            &self.db.conn(),
            id,
            &sha.to_string(),
            "t",
        )
        .unwrap();
    }

    async fn observed(
        &self,
    ) -> (
        LiveExecutionConfiguration,
        GitSourceSnapshot,
        SvnSourceSnapshot,
        Option<PairRefreshOperation>,
    ) {
        let configuration = self
            .db
            .capture_pair_refresh_execution_configuration("child")
            .unwrap();
        let baseline = self
            .db
            .capture_completed_pair_refresh_baseline(&configuration)
            .unwrap();
        let git =
            observe_registered_git_sources_for_test(&configuration, self.client.clone()).unwrap();
        let svn = observe_registered_svn_sources(&configuration)
            .await
            .unwrap();
        (configuration, git, svn, baseline)
    }

    async fn prepare(&self) -> Result<LivePreparedRefresh> {
        let (configuration, git, svn, baseline) = self.observed().await;
        prepare_observed_sources(
            &self.db,
            configuration,
            git,
            svn,
            baseline,
            Uuid::new_v4(),
            1_790_000_000,
        )
        .await
    }

    async fn assert_unpublished(
        &self,
        state: &[(String, String, String)],
        child: Oid,
        head_revision: i64,
    ) {
        assert_eq!(self.state(), state);
        assert_eq!(
            self.target.refname_to_id("refs/heads/feature").unwrap(),
            child
        );
        assert_eq!(
            self.target.refname_to_id("refs/heads/main").unwrap(),
            self.parent
        );
        let client = crate::svn::SvnClient::new(format!("{}/branches/f", self.root), "", "");
        assert_eq!(client.info().await.unwrap().latest_rev, head_revision);
        assert!(!std::fs::read_to_string(&self.log)
            .unwrap()
            .contains("git-receive-pack"));
        crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&self.db.conn(), None)
            .unwrap();
    }
}

#[tokio::test]
async fn first_preparation_computes_both_results_without_publication_or_admission() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let before = f.state();
    let prepared = f.prepare().await.unwrap();
    let pins = prepared.pins();
    assert_eq!(pins.git.pins.parent_baseline_tip, f.baseline.to_string());
    assert_eq!(pins.svn.pins.lineage.child_copy.created_revision, 3);
    assert_eq!(pins.svn.pins.lineage.child_copy.copy_from_revision, 2);
    assert_eq!(pins.svn.mergeinfo, "/trunk:3-4");
    assert_eq!(pins.git.tree_sha, pins.svn.result_git_tree);
    let commit = prepared
        .git()
        .repository()
        .find_commit(Oid::from_str(&pins.git.prepared_sha).unwrap())
        .unwrap();
    assert_eq!(commit.parent_count(), 2);
    assert_eq!(commit.parent_id(0).unwrap(), f.baseline);
    assert_eq!(commit.parent_id(1).unwrap(), f.parent);
    assert_eq!(
        std::fs::read(pins.svn.working_copy.join("file.txt")).unwrap(),
        b"advanced\n"
    );
    let workspace = pins.svn.working_copy.clone();
    drop(commit);
    f.assert_unpublished(&before, f.baseline, 4).await;
    drop(prepared);
    assert!(!workspace.exists());
}

#[tokio::test]
async fn first_preparation_refuses_equal_tree_child_without_shared_current_commit() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let child = commit(&f.target, b"baseline\n", &[f.baseline]);
    f.target
        .reference("refs/heads/feature", child, true, "fixture")
        .unwrap();
    f.checkpoints("child", child, 3);
    let before = f.state();
    assert!(f
        .prepare()
        .await
        .err()
        .unwrap()
        .to_string()
        .contains("shared baseline"));
    f.assert_unpublished(&before, child, 4).await;
}

#[tokio::test]
async fn first_preparation_refuses_child_commit_after_original_copy_even_with_equal_bytes() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let child_wc = f.temp.path().join("changed-child-wc");
    svn(
        &[
            "checkout",
            &format!("{}/branches/f", f.root),
            child_wc.to_str().unwrap(),
        ],
        None,
    );
    svn(
        &["propset", "fixture:unapproved", "value", "."],
        Some(&child_wc),
    );
    svn(&["commit", "-m", "child property"], Some(&child_wc));
    f.checkpoints("child", f.baseline, 5);
    let before = f.state();
    assert!(f
        .prepare()
        .await
        .err()
        .unwrap()
        .to_string()
        .contains("unchanged original SVN copy"));
    f.assert_unpublished(&before, f.baseline, 5).await;
}

#[tokio::test]
async fn full_preparation_refuses_cross_scm_tree_disagreement_and_registration_drift() {
    let _serial = SERIAL.lock().await;
    for change in ["tree", "registration"] {
        let f = Fixture::new();
        if change == "tree" {
            let wrong_parent = commit(&f.target, b"different Git bytes\n", &[f.baseline]);
            f.target
                .reference("refs/heads/main", wrong_parent, true, "fixture")
                .unwrap();
            f.checkpoints("parent", wrong_parent, 4);
        }
        let (configuration, git, svn, baseline) = f.observed().await;
        if change == "registration" {
            f.db.set_state("secret_git_token_parent", "disposable-rotated-token")
                .unwrap();
        }
        let before = f.state();
        let error = prepare_observed_sources(
            &f.db,
            configuration,
            git,
            svn,
            baseline,
            Uuid::new_v4(),
            1_790_000_000,
        )
        .await
        .err()
        .unwrap()
        .to_string();
        assert!(
            error.contains(if change == "tree" {
                "SVN content differs"
            } else {
                "changed during preparation"
            }),
            "{error}"
        );
        assert_eq!(f.state(), before);
        assert_eq!(
            f.target.refname_to_id("refs/heads/feature").unwrap(),
            f.baseline
        );
        assert!(!std::fs::read_to_string(&f.log)
            .unwrap()
            .contains("git-receive-pack"));
    }
}

#[tokio::test]
async fn newly_admitted_ordinary_writer_refuses_preparation_without_releasing_its_hold() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let (configuration, git, svn, baseline) = f.observed().await;
    let writer = f.db.begin_ordinary_writer_window(Some("child")).unwrap();
    let before = f.state();
    let error = prepare_observed_sources(
        &f.db,
        configuration,
        git,
        svn,
        baseline,
        Uuid::new_v4(),
        1_790_000_000,
    )
    .await
    .err()
    .unwrap()
    .to_string();
    assert!(error.contains("ordinary writer"), "{error}");
    assert_eq!(f.state(), before);
    assert_eq!(
        f.target.refname_to_id("refs/heads/feature").unwrap(),
        f.baseline
    );
    assert_eq!(f.target.refname_to_id("refs/heads/main").unwrap(), f.parent);
    let child =
        crate::db::pair_refresh_operations::capture_refresh_registration(&f.db.conn(), "child")
            .unwrap();
    assert!(
        crate::db::ordinary_writer_windows::require_quiet_for_refresh(
            &f.db.conn(),
            "child",
            &child.scope_uuid
        )
        .is_err()
    );
    writer.finish_preparation().unwrap();
    crate::db::ordinary_writer_windows::require_quiet_for_refresh(
        &f.db.conn(),
        "child",
        &child.scope_uuid,
    )
    .unwrap();
}

#[tokio::test]
async fn next_preparation_uses_real_completed_receipt_and_preserves_original_copy() {
    let _serial = SERIAL.lock().await;
    let mut f = Fixture::new();
    let first = f.prepare().await.unwrap();
    let pins = first.pins().clone();
    let busy = crate::busy::try_acquire_pair("parent", "child").unwrap();
    let operation =
        f.db.admit_pair_refresh_with_registered_sources(
            "admin",
            "fixture-first",
            &pins.approval_digest().unwrap(),
            pins.clone(),
            &busy,
        )
        .unwrap();
    assert_eq!(operation.id, pins.git.operation_id);
    f.db.record_pair_refresh_svn_intent(operation.id).unwrap();
    first
        .svn()
        .child_client()
        .commit(
            &pins.svn.working_copy,
            &refresh_svn_commit_message(operation.id).unwrap(),
            "",
        )
        .await
        .unwrap();
    let observation = f.temp.path().join("receipt-observation");
    std::fs::create_dir(&observation).unwrap();
    let receipt = match observe_prepared_child_svn(
        first.svn().child_client(),
        first.svn().parent_client(),
        first.git().repository().path(),
        &observation,
        &pins.svn,
    )
    .await
    .unwrap()
    {
        SvnPublicationObservation::Observed(receipt) => *receipt,
        SvnPublicationObservation::NotObserved => panic!("fixture publication absent"),
    };
    assert_eq!(receipt.revision, 5);
    f.db.record_pair_refresh_svn_receipt(operation.id, receipt)
        .unwrap();
    f.db.record_pair_refresh_git_intent(operation.id).unwrap();
    first
        .git()
        .repository()
        .remote_anonymous(f.target.path().to_str().unwrap())
        .unwrap()
        .push(
            &[&format!("{}:refs/heads/feature", pins.git.prepared_sha)],
            None,
        )
        .unwrap();
    f.db.record_pair_refresh_git_receipt(operation.id, &pins.git.prepared_sha)
        .unwrap();
    f.db.finalize_pair_refresh(operation.id).unwrap();
    drop(busy);
    let child = Oid::from_str(&pins.git.prepared_sha).unwrap();
    drop(first);
    svn(&["update"], Some(&f.parent_wc));
    std::fs::write(f.parent_wc.join("file.txt"), b"second advance\n").unwrap();
    svn(
        &["commit", "-m", "second parent advance"],
        Some(&f.parent_wc),
    );
    f.parent = commit(&f.target, b"second advance\n", &[f.parent]);
    f.target
        .reference("refs/heads/main", f.parent, true, "fixture")
        .unwrap();
    f.checkpoints("parent", f.parent, 6);
    let before = f.state();
    let next = f.prepare().await.unwrap();
    assert_eq!(next.pins().git.pins.child_tip, child.to_string());
    assert_eq!(
        next.pins().git.pins.parent_baseline_tip,
        pins.parent.last_git_sha
    );
    assert_eq!(
        next.pins().svn.pins.previous_mergeinfo.as_deref(),
        Some("/trunk:3-4")
    );
    assert_eq!(next.pins().svn.mergeinfo, "/trunk:3-6");
    assert_eq!(next.pins().svn.pins.lineage.child_copy.created_revision, 3);
    assert_eq!(
        next.pins().svn.pins.lineage.child_copy.copy_from_revision,
        2
    );
    assert_eq!(next.pins().git.tree_sha, next.pins().svn.result_git_tree);
    assert_eq!(
        std::fs::read(next.pins().svn.working_copy.join("file.txt")).unwrap(),
        b"second advance\n"
    );
    f.assert_unpublished(&before, child, 6).await;
}
