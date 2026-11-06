//! Disposable HTTPS upload-pack controls; production TLS verification stays on.
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use git2::{Oid, Repository, Signature};

#[path = "../../tests/fixtures/owned_process_group.rs"]
// Reuse the protected fixture body without changing its private declaration
// in the published publication tests or copying its cleanup implementation.
#[allow(clippy::duplicate_mod)]
mod fixture_process;

use super::{observe_registered_git_sources, observe_with_client};
use crate::db::Database;
use crate::pair_refresh::execution_configuration::LiveExecutionConfiguration;

fn run(command: &mut Command) {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn commit(repo: &Repository, bytes: &[u8], parents: &[Oid], message: &str) -> Oid {
    let mut tree = repo.treebuilder(None).unwrap();
    tree.insert("file.txt", repo.blob(bytes).unwrap(), 0o100644)
        .unwrap();
    let tree = tree.write().unwrap();
    let tree = repo.find_tree(tree).unwrap();
    let signature = Signature::now("Disposable source fixture", "fixture@example.invalid").unwrap();
    let parents = parents
        .iter()
        .map(|id| repo.find_commit(*id).unwrap())
        .collect::<Vec<_>>();
    let parents = parents.iter().collect::<Vec<_>>();
    repo.commit(None, &signature, &signature, message, &tree, &parents)
        .unwrap()
}

fn start(
    root: &Path,
    cert: &Path,
    key: &Path,
    log: &Path,
    redirect: Option<&str>,
) -> (fixture_process::OwnedProcessGroup, String) {
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pair_refresh_http_server.py");
    let mut command = Command::new("python3");
    command
        .arg(script)
        .arg(root)
        .arg("--cert")
        .arg(cert)
        .arg("--key")
        .arg(key)
        .arg("--token")
        .arg("disposable-source-token")
        .arg("--request-log")
        .arg(log)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    if let Some(redirect) = redirect {
        command.arg("--redirect").arg(redirect);
    }
    let mut server = fixture_process::OwnedProcessGroup::spawn(&mut command).unwrap();
    let mut line = String::new();
    BufReader::new(server.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let ready = serde_json::from_str::<serde_json::Value>(&line).unwrap();
    (server, ready["url"].as_str().unwrap().into())
}

struct Fixture {
    _server: fixture_process::OwnedProcessGroup,
    url: String,
    root: Oid,
    child: Oid,
    parent: Oid,
    target: Repository,
    temp: tempfile::TempDir,
    cert: PathBuf,
    key: PathBuf,
    der: Vec<u8>,
    log: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let source = Repository::init_bare(temp.path().join("source.git")).unwrap();
        let target = Repository::init_bare(temp.path().join("target.git")).unwrap();
        let root = commit(&source, b"baseline\n", &[], "initial");
        let child = commit(&source, b"baseline\n", &[root], "child empty commit");
        let parent = commit(&source, b"advanced\n", &[root], "parent advance");
        source
            .remote_anonymous(target.path().to_str().unwrap())
            .unwrap()
            .push(
                &[
                    &format!("{child}:refs/heads/feature"),
                    &format!("{parent}:refs/heads/main"),
                ],
                None,
            )
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
        let der_path = temp.path().join("cert.der");
        run(Command::new("openssl")
            .arg("x509")
            .arg("-in")
            .arg(&cert)
            .args(["-outform", "DER", "-out"])
            .arg(&der_path));
        let der = std::fs::read(der_path).unwrap();
        let log = temp.path().join("requests.jsonl");
        let (_server, url) = start(temp.path(), &cert, &key, &log, None);
        Self {
            _server,
            url,
            root,
            child,
            parent,
            target,
            temp,
            cert,
            key,
            der,
            log,
        }
    }
    fn client(&self) -> reqwest::Client {
        reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(5))
            .add_root_certificate(reqwest::Certificate::from_der(&self.der).unwrap())
            .build()
            .unwrap()
    }
    fn configuration(
        &self,
        child_token: Option<&str>,
        parent_token: Option<&str>,
    ) -> LiveExecutionConfiguration {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        db.set_state("encryption_key", &"41".repeat(32)).unwrap();
        for (id, parent_id, sha, rev, git_branch, svn_branch, token) in [
            (
                "parent",
                None,
                self.parent,
                4,
                "main",
                "trunk",
                parent_token,
            ),
            (
                "child",
                Some("parent"),
                self.child,
                3,
                "feature",
                "branches/f",
                child_token,
            ),
        ] {
            let scope = uuid::Uuid::new_v4().to_string();
            db.conn().execute("INSERT INTO repositories(id,name,svn_url,svn_branch,svn_username,git_provider,git_api_url,git_repo,git_branch,sync_mode,poll_interval_secs,lfs_threshold_mb,auto_merge,enabled,created_at,updated_at,last_svn_rev,last_git_sha,sync_status,total_syncs,total_errors,parent_id,scope_uuid) VALUES(?1,?1,'https://svn.example/repo',?2,'','gitea',?3,'target',?4,'team',5,0,0,1,'t','t',?5,?6,'idle',0,0,?7,?8)", rusqlite::params![id,svn_branch,format!("{}/api/v1", self.url.trim_end_matches("/target.git")),git_branch,rev,sha.to_string(),parent_id,scope]).unwrap();
            db.set_state(&format!("last_svn_rev_{id}"), &rev.to_string())
                .unwrap();
            db.set_state(&format!("last_git_sha_{scope}"), &sha.to_string())
                .unwrap();
            if let Some(token) = token {
                db.set_state(&format!("secret_git_token_{id}"), token)
                    .unwrap();
            }
        }
        db.capture_pair_refresh_execution_configuration("child")
            .unwrap()
    }
    fn refs(&self) -> Vec<(String, Oid)> {
        let mut refs = self
            .target
            .references()
            .unwrap()
            .map(|r| {
                let r = r.unwrap();
                (r.name().unwrap().into(), r.target().unwrap())
            })
            .collect::<Vec<_>>();
        refs.sort();
        refs
    }
    fn reads_only(&self) {
        let log = std::fs::read_to_string(&self.log).unwrap();
        assert!(log.contains("git-upload-pack"));
        assert!(!log.contains("git-receive-pack"));
        assert!(!self.temp.path().join("published").exists());
    }
}

#[test]
fn registered_sources_download_complete_graphs_without_refs_or_receive_pack() {
    let f = Fixture::new();
    let configuration = f.configuration(
        Some("disposable-source-token"),
        Some("disposable-source-token"),
    );
    let before = f.refs();
    let snapshot = observe_with_client(&configuration, f.client()).unwrap();
    assert_eq!(snapshot.pins().child_tip, f.child.to_string());
    assert_eq!(snapshot.pins().parent_tip, f.parent.to_string());
    assert_eq!(snapshot.pins().remote_url, f.url);
    assert!(snapshot.repository().is_bare());
    assert_eq!(snapshot.repository().references().unwrap().count(), 0);
    assert!(!snapshot.repository().path().join("FETCH_HEAD").exists());
    assert!(!snapshot.repository().path().join("shallow").exists());
    for tip in [f.root, f.parent, f.child] {
        snapshot.repository().find_commit(tip).unwrap();
    }
    assert!(snapshot
        .repository()
        .graph_descendant_of(f.child, f.root)
        .unwrap());
    assert!(snapshot
        .repository()
        .graph_descendant_of(f.parent, f.root)
        .unwrap());
    assert!(!serde_json::to_string(snapshot.pins())
        .unwrap()
        .contains("disposable-source-token"));
    assert_eq!(f.refs(), before);
    f.reads_only();
}

#[test]
fn explicit_child_access_never_substitutes_the_parent_token_or_downloaded_objects() {
    let f = Fixture::new();
    let before = f.refs();
    // A missing override legitimately inherits through the strict managed
    // chain. This differs from replacing an explicit wrong credential.
    let inherited = f.configuration(None, Some("disposable-source-token"));
    observe_with_client(&inherited, f.client()).unwrap();
    for (child, parent) in [
        (None, None),
        (Some("disposable-source-token"), None),
        (
            Some("wrong-disposable-token"),
            Some("disposable-source-token"),
        ),
        (
            Some("disposable-source-token"),
            Some("wrong-disposable-token"),
        ),
    ] {
        let configuration = f.configuration(child, parent);
        let error = observe_with_client(&configuration, f.client())
            .err()
            .unwrap()
            .to_string();
        assert!(!error.contains("wrong-disposable-token"));
        assert_eq!(f.refs(), before);
    }
    f.reads_only();
}

#[test]
fn either_ref_drift_and_missing_ref_refuse_the_registered_checkpoint() {
    for branch in ["feature", "main"] {
        for missing in [false, true] {
            let f = Fixture::new();
            let configuration = f.configuration(
                Some("disposable-source-token"),
                Some("disposable-source-token"),
            );
            let refname = format!("refs/heads/{branch}");
            let mut reference = f.target.find_reference(&refname).unwrap();
            if missing {
                reference.delete().unwrap();
            } else {
                let advanced = commit(
                    &f.target,
                    b"later drift\n",
                    &[reference.target().unwrap()],
                    "later remote advance",
                );
                reference
                    .set_target(advanced, "disposable later advance")
                    .unwrap();
            }
            let before = f.refs();
            assert!(observe_with_client(&configuration, f.client()).is_err());
            assert_eq!(f.refs(), before);
            f.reads_only();
        }
    }
}

#[test]
fn redirects_refuse_without_contacting_the_substituted_endpoint() {
    let mut f = Fixture::new();
    let secondary_log = f.temp.path().join("secondary.jsonl");
    let (_secondary, secondary_url) = start(f.temp.path(), &f.cert, &f.key, &secondary_log, None);
    let (_redirect, redirect_url) = start(
        f.temp.path(),
        &f.cert,
        &f.key,
        &f.log,
        Some(secondary_url.trim_end_matches("/target.git")),
    );
    f.url = redirect_url;
    let configuration = f.configuration(
        Some("disposable-source-token"),
        Some("disposable-source-token"),
    );
    let before = f.refs();
    assert!(observe_with_client(&configuration, f.client()).is_err());
    assert_eq!(f.refs(), before);
    assert!(std::fs::read_to_string(secondary_log)
        .unwrap_or_default()
        .is_empty());
    f.reads_only();
}

#[test]
fn production_client_keeps_normal_certificate_verification() {
    let f = Fixture::new();
    let configuration = f.configuration(
        Some("disposable-source-token"),
        Some("disposable-source-token"),
    );
    let before = f.refs();
    assert!(observe_registered_git_sources(&configuration).is_err());
    assert_eq!(f.refs(), before);
    assert!(std::fs::read_to_string(&f.log)
        .unwrap_or_default()
        .is_empty());
}
