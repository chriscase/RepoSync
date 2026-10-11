//! Real receive-pack controls for pinned pair-refresh publication.

#![cfg(unix)]

use std::net::TcpListener;
use std::os::fd::OwnedFd;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use git2::{Oid, Repository, Signature};
use reposync_core::pair_refresh::publication::{
    publish_pinned_child, require_pinned_push_endpoint, require_pinned_push_transport,
};

struct Daemon {
    stopped: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Daemon {
    fn start(listener: TcpListener, root: PathBuf) -> Self {
        listener.set_nonblocking(true).unwrap();
        let stopped = Arc::new(AtomicBool::new(false));
        let worker_stopped = stopped.clone();
        let worker = std::thread::spawn(move || {
            let mut children: Vec<Child> = Vec::new();
            while !worker_stopped.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        let output: OwnedFd = stream.try_clone().unwrap().into();
                        let input: OwnedFd = stream.into();
                        // Keep the reserved listener for the entire fixture.
                        // Each connection runs real Git's smart protocol with
                        // the same receive-pack hooks and old-tip checks.
                        let child = Command::new("git")
                            .args([
                                "daemon",
                                "--inetd",
                                "--export-all",
                                "--enable=receive-pack",
                                &format!("--base-path={}", root.display()),
                            ])
                            .arg(&root)
                            .stdin(Stdio::from(input))
                            .stdout(Stdio::from(output))
                            .stderr(Stdio::null())
                            .spawn()
                            .unwrap();
                        children.push(child);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("fixture listener failed: {error}"),
                }
                children.retain_mut(|child| child.try_wait().unwrap().is_none());
            }
            for mut child in children {
                let _ = child.kill();
                let _ = child.wait();
            }
        });
        Self {
            stopped,
            worker: Some(worker),
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
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
        let daemon = Daemon::start(listener, temp.path().to_owned());
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

#[test]
fn configured_fetch_url_rewrite_cannot_bypass_transport_admission() {
    let fixture = Fixture::new();
    fixture
        .source
        .config()
        .unwrap()
        .set_str(
            &format!("url.file://{}.insteadOf", fixture.target.path().display()),
            &fixture.url,
        )
        .unwrap();
    assert!(require_pinned_push_endpoint(&fixture.source, &fixture.url).is_err());
    assert!(fixture.publish().is_err());
    assert_eq!(fixture.tip(), fixture.child);
}

#[test]
fn configured_push_url_rewrite_cannot_bypass_transport_admission() {
    let fixture = Fixture::new();
    fixture
        .source
        .config()
        .unwrap()
        .set_str(
            &format!(
                "url.file://{}.pushInsteadOf",
                fixture.target.path().display()
            ),
            &fixture.url,
        )
        .unwrap();
    assert!(require_pinned_push_endpoint(&fixture.source, &fixture.url).is_err());
    assert!(fixture.publish().is_err());
    assert_eq!(fixture.tip(), fixture.child);
}

#[test]
fn shallow_history_refuses_before_publication() {
    let fixture = Fixture::new();
    std::fs::write(
        fixture.source.path().join("shallow"),
        format!("{}\n", fixture.child),
    )
    .unwrap();
    assert!(fixture.source.is_shallow());
    assert!(fixture.publish().is_err());
    assert_eq!(fixture.tip(), fixture.child);
}

#[test]
fn graft_metadata_refuses_before_publication() {
    let fixture = Fixture::new();
    std::fs::create_dir_all(fixture.source.path().join("info")).unwrap();
    std::fs::write(
        fixture.source.path().join("info/grafts"),
        format!("{} {}\n", fixture.child, fixture.parent),
    )
    .unwrap();
    assert!(fixture.publish().is_err());
    assert_eq!(fixture.tip(), fixture.child);
}

#[test]
fn replace_reference_refuses_before_publication() {
    let fixture = Fixture::new();
    fixture
        .source
        .reference(
            &format!("refs/replace/{}", fixture.child),
            fixture.parent,
            false,
            "fixture graph overlay",
        )
        .unwrap();
    assert!(fixture.publish().is_err());
    assert_eq!(fixture.tip(), fixture.child);
}

#[test]
fn concurrent_fixtures_keep_unique_live_listeners() {
    let barrier = Arc::new(std::sync::Barrier::new(8));
    let workers: Vec<_> = (0..8)
        .map(|_| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let fixture = Fixture::new();
                barrier.wait();
                assert_eq!(fixture.publish().unwrap(), fixture.prepared);
                assert_eq!(fixture.tip(), fixture.prepared);
                fixture.url.clone()
            })
        })
        .collect();
    let urls: std::collections::HashSet<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    assert_eq!(urls.len(), 8);
}

#[test]
fn linked_worktree_common_grafts_refuse_before_publication() {
    let fixture = Fixture::new();
    let linked_path = fixture._temp.path().join("linked-worktree");
    let output = Command::new("git")
        .arg(format!("--git-dir={}", fixture.source.path().display()))
        .args(["worktree", "add", "--detach"])
        .arg(&linked_path)
        .arg(fixture.prepared.to_string())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let linked = Repository::open(linked_path).unwrap();
    assert!(linked.is_worktree());
    std::fs::create_dir_all(fixture.source.path().join("info")).unwrap();
    std::fs::write(
        fixture.source.path().join("info/grafts"),
        format!("{} {}\n", fixture.child, fixture.parent),
    )
    .unwrap();
    assert!(!linked.path().join("info/grafts").exists());
    assert!(reposync_core::pair_refresh::publication::require_complete_git_graph(&linked).is_err());
    assert!(publish_pinned_child(
        &linked,
        &fixture.url,
        "child",
        fixture.child,
        fixture.prepared,
        None
    )
    .is_err());
    assert_eq!(fixture.tip(), fixture.child);
}
