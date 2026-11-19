//! Observe the real scheduler publisher with a local broadcast subscriber.
//! Both repositories use file transports and the receiver runs in memory.
use super::cancellation_tests::scheduler_fixture;
use super::*;
use reposync_core::db::git_push_operations::{GitPushOperation, GitPushOperationState};
use reposync_core::db::lineage_checkpoints::{
    load_canonical_lineage, load_mapping_outcome, record_independently_established_svn_uuid,
    svn_source_key, CheckpointDirection, MappingOutcome,
};
use reposync_core::models::{Repository, SyncDirection, SyncRecord, SyncRecordStatus};
use serde_json::{json, Value};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn command(dir: &Path, program: &str, args: &[&str]) -> String {
    let output = Command::new(program)
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{program} {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .trim_end_matches('\n')
        .to_owned()
}

// Git invokes pre-receive in its bare repository before changing the refs.
// Publish the complete offer atomically and keep the push open until the test
// has read both active journal entries. The hook also exits if the test dies.
const HOLD_PUSH: &str = r#"#!/bin/sh
set -eu
cat > offer.tmp
mv offer.tmp offer
attempt=0
while [ ! -f release ]; do
    [ -f hooks/pre-receive ] || exit 1
    if [ "$attempt" -ge 600 ]; then
        echo 'timed out waiting for both R17 pushes' >&2
        exit 1
    fi
    sleep 0.05
    attempt=$((attempt + 1))
done
"#;

struct Pair {
    row: Repository,
    svn: PathBuf,
    uuid: String,
    bare: PathBuf,
    author: String,
    message: String,
    content: String,
}

impl Pair {
    fn prepare(root: &Path, db: &Database, mut row: Repository, label: &str) -> Self {
        let id = format!("r17-{label}");
        let svn = root.join(format!("{id}-svn"));
        let author = format!("{label}-author");
        let message = format!("{label} subject\n\n{label} private body");
        let content = format!("{label} new payload");
        let baseline_content = format!("{label} original");
        command(root, "svnadmin", &["create", svn.to_str().unwrap()]);
        let uuid = command(root, "svnlook", &["uuid", svn.to_str().unwrap()]);
        let root_url = format!("file://{}", svn.display());
        let wc = root.join(format!("{id}-wc"));
        command(root, "svn", &["checkout", &root_url, wc.to_str().unwrap()]);
        fs::create_dir(wc.join("trunk")).unwrap();
        fs::write(wc.join("trunk/payload.txt"), &baseline_content).unwrap();
        command(&wc, "svn", &["add", "trunk"]);
        command(
            &wc,
            "svn",
            &["commit", "--username", &author, "-m", "baseline"],
        );
        fs::write(wc.join("trunk/payload.txt"), &content).unwrap();
        command(
            &wc,
            "svn",
            &["commit", "--username", &author, "-m", &message],
        );

        let bare = root.join(format!("{id}.git"));
        command(
            root,
            "git",
            &[
                "init",
                "--bare",
                "--initial-branch=main",
                bare.to_str().unwrap(),
            ],
        );
        let bridge = root.join("repos").join(&id).join("git-repo");
        fs::create_dir_all(&bridge).unwrap();
        command(&bridge, "git", &["init", "--initial-branch=main"]);
        fs::write(bridge.join("payload.txt"), baseline_content).unwrap();
        command(&bridge, "git", &["add", "payload.txt"]);
        command(
            &bridge,
            "git",
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-m",
                "baseline",
            ],
        );
        row.id = id.clone();
        row.name = format!("R17 {label} repository");
        row.git_api_url = format!("file://{}", root.display());
        row.git_repo = id.clone();
        row.svn_url = root_url;
        row.svn_username = author.clone();
        // Use the scheduler's derived clone URL even for the pre-existing bridge.
        let remote = reposync_core::git::remote_url::derive_git_remote_url(
            &row.git_api_url,
            None,
            &row.git_repo,
        );
        assert_eq!(remote, format!("file://{}", bare.display()));
        command(&bridge, "git", &["remote", "add", "origin", &remote]);
        for args in [
            &["remote", "get-url", "--all", "origin"][..],
            &["remote", "get-url", "--push", "--all", "origin"],
        ] {
            assert_eq!(command(&bridge, "git", args), remote);
        }
        command(&bridge, "git", &["push", "origin", "main"]);
        row.last_git_sha = command(&bare, "git", &["rev-parse", "refs/heads/main"]);
        row.last_svn_rev = 1;
        if db.get_repository(&id).unwrap().is_some() {
            db.update_repository(&row).unwrap();
        } else {
            db.insert_repository(&row).unwrap();
        }
        record_independently_established_svn_uuid(db, &id, &uuid).unwrap();
        db.set_state(&format!("last_svn_rev_{id}"), "1").unwrap();
        let now = Utc::now();
        reposync_core::echo_receipt_scope::write_scoped_last_git_sha_kv(
            &db.conn(),
            &id,
            &row.last_git_sha,
            &now.to_rfc3339(),
        )
        .unwrap();
        db.insert_sync_record(&SyncRecord {
            id: uuid::Uuid::new_v4().to_string(),
            repo_id: Some(id),
            svn_revision: Some(1),
            git_hash: Some(row.last_git_sha.clone()),
            direction: SyncDirection::SvnToGit,
            author: author.clone(),
            message: "baseline".into(),
            timestamp: now,
            synced_at: now,
            status: SyncRecordStatus::Applied,
        })
        .unwrap();
        let hook = bare.join("hooks/pre-receive");
        fs::write(&hook, HOLD_PUSH).unwrap();
        fs::set_permissions(hook, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            row,
            svn,
            uuid,
            bare,
            author,
            message,
            content,
        }
    }

    async fn start_and_wait_for_push(&self, scheduler: &Scheduler) -> tokio::task::JoinHandle<()> {
        scheduler.maybe_run_repo_cycles().await;
        let mut handles = scheduler.sync_handles.lock().await;
        assert_eq!(handles.len(), 1, "one new worker for {}", self.row.id);
        let handle = handles.pop().unwrap();
        drop(handles);
        time::timeout(Duration::from_secs(15), async {
            while !self.bare.join("offer").is_file() {
                assert!(
                    !handle.is_finished(),
                    "{} stopped before pre-receive: {:?}",
                    self.row.id,
                    scheduler.db.list_audit_log(10, 0).unwrap()
                );
                time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("worker did not enter its pre-receive hook");
        handle
    }

    async fn release_and_receive(
        &self,
        handle: tokio::task::JoinHandle<()>,
        receiver: &mut broadcast::Receiver<String>,
    ) -> Value {
        fs::write(self.bare.join("release"), "").unwrap();
        time::timeout(Duration::from_secs(15), handle)
            .await
            .expect("worker timed out")
            .expect("worker panicked");
        let raw = receiver
            .try_recv()
            .expect("joined worker must have published its event");
        let event: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            event["repo_id"], self.row.id,
            "wrong worker at publication: {event}"
        );
        assert!(
            matches!(
                receiver.try_recv(),
                Err(broadcast::error::TryRecvError::Empty)
            ),
            "expected exactly one event from {}",
            self.row.id
        );
        event
    }

    fn expected_event(&self) -> Value {
        let repo_path = self.svn.to_str().unwrap();
        let svnlook = |args: &[&str]| command(&self.svn, "svnlook", args);
        assert_eq!(svnlook(&["uuid", repo_path]), self.uuid);
        assert_eq!(svnlook(&["youngest", repo_path]), "2");
        let author = svnlook(&["author", "-r", "2", repo_path]);
        let message = svnlook(&["log", "-r", "2", repo_path]);
        assert_eq!(author, self.author);
        assert_eq!(message, self.message);
        assert_eq!(
            svnlook(&["changed", "-r", "2", repo_path]),
            "U   trunk/payload.txt"
        );
        assert_eq!(
            svnlook(&["cat", "-r", "2", repo_path, "trunk/payload.txt"]),
            self.content
        );
        json!({
            "type":"repo_sync_completed", "repo_id":self.row.id, "repo_name":self.row.name,
            "svn_to_git":1, "git_to_svn":0, "conflicts":0,
            "messages":[message.lines().next().unwrap()],
            "commits":[{"direction":"svn_to_git","author":author,"message":message,"files_changed":1,"revision_id":"r2"}]
        })
    }

    fn attest_push(&self, db: &Database, active: GitPushOperation) -> Value {
        let id = &self.row.id;
        let parent = &self.row.last_git_sha;
        let git = |args: &[&str]| command(&self.bare, "git", args);
        let tip = git(&["rev-parse", "main"]);
        let tree = git(&["rev-parse", "main^{tree}"]);
        assert_ne!(&tip, parent);
        assert_eq!(git(&["show", "-s", "--format=%P", "main"]), *parent);
        assert_eq!(git(&["rev-list", "--count", "main"]), "2");
        assert_eq!(
            git(&["ls-tree", "-r", "--name-only", "main"]),
            "payload.txt"
        );
        assert_eq!(git(&["show", "main:payload.txt"]), self.content);
        assert_eq!(
            fs::read_to_string(self.bare.join("offer")).unwrap(),
            format!("{parent} {tip} refs/heads/main\n")
        );
        assert_eq!(active.state, GitPushOperationState::Running);
        assert_eq!(&active.repo_id, id);
        assert_eq!(active.source_svn_rev, 2);
        assert_eq!(active.source_svn_author, self.author);
        assert_eq!(active.source_svn_message, self.message);
        assert_eq!(active.pre_push_git_sha, *parent);
        assert_eq!(active.intended_local_git_sha, tip);
        assert_eq!(active.intended_local_git_tree, tree);
        let done = db.get_git_push_operation(id, &active.id).unwrap().unwrap();
        assert_eq!(done.state, GitPushOperationState::Completed);
        assert_eq!(done.last_confirmed_git_sha.as_ref(), Some(&tip));
        assert_eq!(done.last_confirmed_git_tree.as_ref(), Some(&tree));
        assert!(db.active_git_push_operation(id).unwrap().is_none());
        let row = db.get_repository(id).unwrap().unwrap();
        assert_eq!(
            (
                row.last_svn_rev,
                &row.last_git_sha,
                row.total_syncs,
                row.total_errors
            ),
            (2, &tip, 1, 0)
        );
        let lineage = load_canonical_lineage(db, id).unwrap().unwrap();
        assert_eq!(lineage.svn_uuid, self.uuid);
        let conn = db.conn();
        let mapping = load_mapping_outcome(
            &conn,
            &lineage.scope_uuid,
            lineage.generation,
            CheckpointDirection::SvnToGit,
            &svn_source_key(2),
        )
        .unwrap()
        .unwrap();
        assert_eq!(mapping.outcome, MappingOutcome::AppliedAndVerified);
        assert_eq!(mapping.target_git_sha.as_ref(), Some(&tip));
        let (total, matched): (i64, i64) = conn
            .query_row(
                "SELECT COUNT(*), COUNT(CASE WHEN git_sha=?2 AND author=?3 AND message=?4
                         AND direction='svn_to_git' AND status='applied' THEN 1 END)
             FROM sync_records WHERE repo_id=?1 AND svn_rev=2",
                rusqlite::params![id, tip, self.author, self.message],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!((total, matched), (1, 1));
        json!({"repo_id": id,"svn_uuid": self.uuid,"svn_revision": 2,"parent":parent,"tip":tip,
            "tree":tree,"push_operation_id":done.id,"scope_uuid":lineage.scope_uuid})
    }
}

// Match the entire unordered pair, including nested commit data, exactly once.
fn events_match(actual: &[Value], expected: &[Value; 2]) -> bool {
    actual.len() == expected.len()
        && expected
            .iter()
            .all(|event| actual.iter().filter(|a| *a == event).count() == 1)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn r17_concurrent_sync_events_match_their_repository() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let (mut scheduler, _) = scheduler_fixture(&tmp, "r17-alpha", "2000-01-01T00:00:00Z");
    let local_base = format!("file://{}", root.display());
    scheduler.app_config.github.api_url = local_base.clone();
    scheduler.app_config.github.git_base_url = Some(local_base);
    scheduler.app_config.identity.email_domain = Some("example.invalid".into());
    let template = scheduler.db.get_repository("r17-alpha").unwrap().unwrap();
    let (sender, mut receiver) = broadcast::channel(8);
    scheduler.ws_broadcast = sender;
    // Admit on successive ticks, leaving alpha inside its real push while beta starts.
    let alpha = Pair::prepare(root, &scheduler.db, template.clone(), "alpha");
    let alpha_worker = alpha.start_and_wait_for_push(&scheduler).await;
    let beta = Pair::prepare(root, &scheduler.db, template, "beta");
    let beta_worker = beta.start_and_wait_for_push(&scheduler).await;
    let pairs = [alpha, beta];
    assert_ne!(pairs[0].uuid, pairs[1].uuid);
    assert_ne!(pairs[0].row.last_git_sha, pairs[1].row.last_git_sha);
    // Both offers and journal entries must coexist before either ref changes.
    let [alpha_op, beta_op] = pairs.each_ref().map(|pair| {
        assert_eq!(
            command(&pair.bare, "git", &["rev-parse", "main"]),
            pair.row.last_git_sha
        );
        scheduler
            .db
            .active_git_push_operation(&pair.row.id)
            .unwrap()
            .expect("active push intent")
    });
    assert_ne!(alpha_op.id, beta_op.id);
    assert!(matches!(
        receiver.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
    // Publish and finalize in reverse admission order. Both worker joins must succeed.
    let events = [
        pairs[1]
            .release_and_receive(beta_worker, &mut receiver)
            .await,
        pairs[0]
            .release_and_receive(alpha_worker, &mut receiver)
            .await,
    ];
    let reopened = Database::new(&root.join("reposync.db")).unwrap();
    let published = [
        pairs[0].attest_push(&reopened, alpha_op),
        pairs[1].attest_push(&reopened, beta_op),
    ];
    assert_ne!(published[0]["scope_uuid"], published[1]["scope_uuid"]);
    assert_ne!(published[0]["tip"], published[1]["tip"]);
    let expected = pairs.each_ref().map(Pair::expected_event);
    assert!(
        events_match(&events, &expected),
        "expected {expected:?}, received {events:?}"
    );

    let mut crossed = expected.clone();
    crossed[0]["commits"] = expected[1]["commits"].clone();
    crossed[1]["commits"] = expected[0]["commits"].clone();
    assert!(!events_match(&crossed, &expected));
    assert!(!events_match(
        &[expected[0].clone(), expected[0].clone()],
        &expected
    ));
    eprintln!(
        "R17_SCHEDULER_EVENT_EVIDENCE {}",
        json!({"transport":"in_process_broadcast","events":events,"repositories":published})
    );
}
