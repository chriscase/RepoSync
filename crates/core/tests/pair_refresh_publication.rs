//! Real receive-pack controls for pinned pair-refresh publication.

#![cfg(unix)]

use std::net::TcpListener;
use std::os::fd::OwnedFd;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use git2::{Oid, Repository, Signature};
use reposync_core::pair_refresh::publication::{
    observe_pinned_child_with_policy, publish_pinned_child, require_pinned_push_endpoint,
    require_pinned_push_transport, GitPublicationObservation, PinnedPushPolicy,
};

#[path = "fixtures/owned_process_group.rs"]
mod fixture_process;
use fixture_process::OwnedProcessGroup;

#[test]
fn fixture_drop_stops_descendant_that_ignores_term() {
    let temp = tempfile::tempdir().unwrap();
    let heartbeat = temp.path().join("heartbeat");
    let script = r#"
import pathlib,subprocess,sys,time
worker = '''import pathlib,signal,sys,time
signal.signal(signal.SIGTERM, signal.SIG_IGN)
path = pathlib.Path(sys.argv[1])
while True:
    with path.open("a") as output:
        output.write("beat\\n")
        output.flush()
    time.sleep(0.01)
'''
subprocess.Popen([sys.executable, "-c", worker, sys.argv[1]])
while True:
    time.sleep(1)
"#;
    let group = OwnedProcessGroup::spawn(
        Command::new("python3")
            .args(["-c", script, heartbeat.to_str().unwrap()])
            .stdout(Stdio::null())
            .stderr(Stdio::inherit()),
    )
    .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !heartbeat
        .metadata()
        .is_ok_and(|metadata| metadata.len() >= 10)
    {
        assert!(
            std::time::Instant::now() < deadline,
            "fixture descendant did not start"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(group);
    let stopped = std::fs::read(&heartbeat).unwrap();
    for _ in 0..3 {
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(
            std::fs::read(&heartbeat).unwrap(),
            stopped,
            "fixture descendant survived cleanup"
        );
    }
}

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
            let mut children: Vec<OwnedProcessGroup> = Vec::new();
            while !worker_stopped.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        let output: OwnedFd = stream.try_clone().unwrap().into();
                        let input: OwnedFd = stream.into();
                        // Keep the reserved listener for the entire fixture.
                        // Each connection runs real Git's smart protocol with
                        // the same receive-pack hooks and old-tip checks.
                        let stderr = std::fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(root.join("receive-pack-stderr.txt"))
                            .unwrap();
                        let child = OwnedProcessGroup::spawn(
                            Command::new("git")
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
                                .stderr(Stdio::from(stderr)),
                        )
                        .unwrap();
                        children.push(child);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => panic!("fixture listener failed: {error}"),
                }
                children.retain_mut(|child| child.try_wait().unwrap().is_none());
            }
            drop(children);
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

    fn receive_pack_stderr(&self) -> String {
        match std::fs::read(self._temp.path().join("receive-pack-stderr.txt")) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(error) => format!("stderr unavailable: {error}"),
        }
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
    let before = fixture.tip();
    let published = fixture.publish();
    let after = fixture.tip();
    assert!(
        published.is_err(),
        "publish={published:?} before={before} after={after} {} stderr={}",
        exec_witness(
            "hook",
            &fixture.target.path().join("hooks/pre-receive")
        ),
        fixture.receive_pack_stderr()
    );
    assert_eq!(fixture.tip(), fixture.initial);
}

#[test]
fn receiver_rejection_is_not_a_publication_receipt() {
    let fixture = Fixture::new();
    fixture.hook("#!/bin/sh\nexit 1\n");
    let before = fixture.tip();
    let published = fixture.publish();
    let after = fixture.tip();
    assert!(
        published.is_err(),
        "publish={published:?} before={before} after={after} {} stderr={}",
        exec_witness(
            "hook",
            &fixture.target.path().join("hooks/pre-receive")
        ),
        fixture.receive_pack_stderr()
    );
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

impl Fixture {
    fn observe(&self) -> Result<GitPublicationObservation, reposync_core::errors::GitError> {
        observe_pinned_child_with_policy(
            &self.source,
            &self.url,
            "child",
            self.child,
            self.prepared,
            None,
            PinnedPushPolicy::GitProtocolV1,
        )
    }
}

#[test]
fn recovery_old_tip_is_only_not_observed_and_never_runs_receive_pack() {
    let fixture = Fixture::new();
    let witness = fixture._temp.path().join("receive-pack-witness");
    fixture.hook(&format!(
        "#!/bin/sh\nprintf 'write\\n' > '{}'\nexit 1\n",
        witness.display()
    ));
    assert_eq!(
        fixture.observe().unwrap(),
        GitPublicationObservation::NotObserved
    );
    assert_eq!(fixture.tip(), fixture.child);
    assert!(!witness.exists());
}

#[test]
fn recovery_of_missing_receipt_observes_result_without_second_write_or_local_ref_change() {
    let fixture = Fixture::new();
    // Ignore the returned publication receipt, then recover only by reading.
    let _ = fixture.publish().unwrap();
    let witness = fixture._temp.path().join("second-write-witness");
    fixture.hook(&format!(
        "#!/bin/sh\nprintf 'retry\\n' > '{}'\nexit 1\n",
        witness.display()
    ));
    let refs = || {
        fixture
            .source
            .references()
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (entry.name().unwrap().to_owned(), entry.target())
            })
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    let before = refs();
    for _ in 0..2 {
        assert_eq!(
            fixture.observe().unwrap(),
            GitPublicationObservation::Observed(fixture.prepared)
        );
        assert_eq!(fixture.tip(), fixture.prepared);
        assert_eq!(refs(), before);
        assert!(!witness.exists());
    }
}

#[test]
fn recovery_refuses_older_second_parent_and_later_descendant_tips() {
    let fixture = Fixture::new();
    for tip in [fixture.initial, fixture.parent] {
        fixture
            .target
            .reference("refs/heads/child", tip, true, "disposable drift control")
            .unwrap();
        assert!(fixture.observe().is_err());
        assert_eq!(fixture.tip(), tip);
    }
    fixture
        .target
        .reference(
            "refs/heads/child",
            fixture.prepared,
            true,
            "disposable published state",
        )
        .unwrap();
    let advanced = commit(&fixture.source, &[fixture.prepared], "new child work");
    fixture
        .source
        .remote_anonymous(fixture.target.path().to_str().unwrap())
        .unwrap()
        .push(&[&format!("{advanced}:refs/heads/child")], None)
        .unwrap();
    assert!(fixture.observe().is_err());
    assert_eq!(fixture.tip(), advanced);
}

#[test]
fn recovery_missing_branch_refuses_without_recreating_it() {
    let fixture = Fixture::new();
    fixture
        .target
        .find_reference("refs/heads/child")
        .unwrap()
        .delete()
        .unwrap();
    assert!(fixture.observe().is_err());
    assert!(fixture.target.find_reference("refs/heads/child").is_err());
}

#[test]
fn recovery_effective_url_rewrite_refuses_without_changing_target() {
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
    let error = fixture.observe().unwrap_err();
    assert!(
        error.to_string().contains("rewrites the pinned endpoint"),
        "{error}"
    );
    assert_eq!(fixture.tip(), fixture.child);
}

#[test]
fn recovery_invalid_pins_are_refused_before_dialing() {
    let fixture = Fixture::new();
    for (branch, old, prepared) in [
        ("../child", fixture.child, fixture.prepared),
        ("child", Oid::zero(), fixture.prepared),
        ("child", fixture.child, fixture.child),
        ("child", fixture.child, fixture.parent),
    ] {
        let error = observe_pinned_child_with_policy(
            &fixture.source,
            "git://127.0.0.1:1/target.git",
            branch,
            old,
            prepared,
            None,
            PinnedPushPolicy::GitProtocolV1,
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("invalid pinned child observation inputs"),
            "{error}"
        );
        assert_eq!(fixture.tip(), fixture.child);
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
