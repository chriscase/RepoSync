//! Real receive-pack controls for pinned pair-refresh publication.

#![cfg(unix)]

use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use git2::{Oid, Repository, Signature};
use reposync_core::pair_refresh::publication::{
    publish_pinned_child, require_pinned_push_transport,
};

struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Fixture {
    _daemon: Daemon,
    source: Repository,
    target: Repository,
    url: String,
    initial: Oid,
    child: Oid,
    parent: Oid,
    prepared: Oid,
    _temp: tempfile::TempDir,
}

fn commit(repo: &Repository, parents: &[Oid], message: &str) -> Oid {
    let tree = repo.treebuilder(None).unwrap().write().unwrap();
    let tree = repo.find_tree(tree).unwrap();
    let parents: Vec<_> = parents
        .iter()
        .map(|id| repo.find_commit(*id).unwrap())
        .collect();
    let parents: Vec<_> = parents.iter().collect();
    let signature = Signature::now("Pair refresh fixture", "fixture@example.invalid").unwrap();
    repo.commit(None, &signature, &signature, message, &tree, &parents)
        .unwrap()
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let source = Repository::init_bare(temp.path().join("source.git")).unwrap();
        let target = Repository::init_bare(temp.path().join("target.git")).unwrap();
        let initial = commit(&source, &[], "initial");
        let child = commit(&source, &[initial], "child");
        let parent = commit(&source, &[initial], "parent");
        let prepared = commit(&source, &[child, parent], "prepared reconciliation");
        // Seed all fixture objects. This is an ordinary setup push, before the
        // production primitive is exercised over the smart protocol.
        source
            .remote_anonymous(target.path().to_str().unwrap())
            .unwrap()
            .push(&[&format!("{prepared}:refs/heads/child")], None)
            .unwrap();
        target
            .reference("refs/heads/child", child, true, "fixture pinned input")
            .unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let mut daemon = Daemon(
            Command::new("git")
                .args([
                    "daemon",
                    "--reuseaddr",
                    "--export-all",
                    "--enable=receive-pack",
                    "--listen=127.0.0.1",
                    &format!("--port={port}"),
                    &format!("--base-path={}", temp.path().display()),
                ])
                .arg(temp.path())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let started = Instant::now();
        loop {
            assert!(daemon.0.try_wait().unwrap().is_none(), "git daemon exited");
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                break;
            }
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "git daemon not ready"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        Self {
            _daemon: daemon,
            source,
            target,
            url: format!("git://127.0.0.1:{port}/target.git"),
            initial,
            child,
            parent,
            prepared,
            _temp: temp,
        }
    }

    fn publish(&self) -> Result<Oid, reposync_core::errors::GitError> {
        publish_pinned_child(
            &self.source,
            &self.url,
            "child",
            self.child,
            self.prepared,
            None,
        )
    }

    fn tip(&self) -> Oid {
        self.target.refname_to_id("refs/heads/child").unwrap()
    }

    fn hook(&self, script: &str) {
        let path = self.target.path().join("hooks/pre-receive");
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
}

#[test]
fn exact_old_tip_publishes_two_parent_commit() {
    let fixture = Fixture::new();
    assert_eq!(fixture.publish().unwrap(), fixture.prepared);
    assert_eq!(fixture.tip(), fixture.prepared);
    let commit = fixture.target.find_commit(fixture.prepared).unwrap();
    assert_eq!(commit.parent_count(), 2);
    assert_eq!(commit.parent_id(0).unwrap(), fixture.child);
    assert_eq!(commit.parent_id(1).unwrap(), fixture.parent);
    assert!(fixture.target.find_commit(fixture.initial).is_ok());
}

#[test]
fn older_ancestor_drift_refuses_even_though_update_is_fast_forward() {
    let fixture = Fixture::new();
    fixture
        .target
        .reference("refs/heads/child", fixture.initial, true, "external rewind")
        .unwrap();
    assert!(fixture.publish().is_err());
    assert_eq!(fixture.tip(), fixture.initial);
}

#[test]
fn second_parent_drift_refuses_even_though_update_is_fast_forward() {
    let fixture = Fixture::new();
    fixture
        .target
        .reference(
            "refs/heads/child",
            fixture.parent,
            true,
            "external movement",
        )
        .unwrap();
    assert!(fixture.publish().is_err());
    assert_eq!(fixture.tip(), fixture.parent);
}

#[test]
fn ref_move_after_advertisement_is_rejected_by_receive_pack() {
    let fixture = Fixture::new();
    fixture.hook(&format!(
        "#!/bin/sh\nenv -u GIT_QUARANTINE_PATH git update-ref refs/heads/child {}\n",
        fixture.initial
    ));
    assert!(fixture.publish().is_err());
    assert_eq!(fixture.tip(), fixture.initial);
}

#[test]
fn receiver_rejection_is_not_a_publication_receipt() {
    let fixture = Fixture::new();
    fixture.hook("#!/bin/sh\nexit 1\n");
    assert!(fixture.publish().is_err());
    assert_eq!(fixture.tip(), fixture.child);
}

#[test]
fn local_transport_is_refused_without_changing_target() {
    let fixture = Fixture::new();
    for url in [
        fixture.target.path().to_str().unwrap().to_owned(),
        format!("file://{}", fixture.target.path().display()),
    ] {
        assert!(publish_pinned_child(
            &fixture.source,
            &url,
            "child",
            fixture.child,
            fixture.prepared,
            None,
        )
        .is_err());
        assert_eq!(fixture.tip(), fixture.child);
    }
}

#[test]
fn unrelated_prepared_history_is_refused_without_changing_target() {
    let fixture = Fixture::new();
    let unrelated = commit(&fixture.source, &[], "unrelated history");
    assert!(publish_pinned_child(
        &fixture.source,
        &fixture.url,
        "child",
        fixture.child,
        unrelated,
        None,
    )
    .is_err());
    assert_eq!(fixture.tip(), fixture.child);
}

#[test]
fn admission_rejects_ambiguous_and_embedded_credentials_urls() {
    for url in [
        "../target.git",
        "git@github.com:owner/repo.git",
        "https://github.com/owner/repo.git",
        "http://example.invalid/repo.git",
        "ssh://git@example.invalid/repo.git",
        "file:///tmp/target.git",
        "https://token@github.com/owner/repo.git",
        "https://github.com/owner/repo.git?token=secret",
        "https://github.com/owner/repo.git#branch",
        "ssh://git:secret@example.invalid/repo.git",
    ] {
        assert!(require_pinned_push_transport(url).is_err());
    }
    assert!(require_pinned_push_transport("git://127.0.0.1:9418/target.git").is_ok());
}
