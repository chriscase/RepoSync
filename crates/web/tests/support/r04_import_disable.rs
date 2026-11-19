use super::{authed_client, git_show, import_fixture, inbound_git_sha_kv, rs67_command};
use reposync_core::db::{import_operations::ImportOperationState, Database};
use reposync_web::AppState;
use reqwest::{Client, StatusCode};
use serde_json::Value;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

const MAIN: &str = "refs/heads/main";
const DEADLINE: Duration = Duration::from_secs(60);

// Git runs pre-receive in the bare repository. Capture its actual old/new/ref
// lines before any ref moves. Both the test and the hook have bounded waits.
struct ReceiveGate(PathBuf);

impl ReceiveGate {
    fn install(bare: &Path) -> Self {
        let hooks = bare.join("hooks");
        fs::create_dir_all(&hooks).unwrap();
        let hook = hooks.join("pre-receive");
        assert!(!hook.exists());
        fs::write(
            &hook,
            r#"#!/bin/sh
set -eu
cat >> hooks/r04-offers
: > hooks/r04-ready
remaining=1200
while [ ! -f hooks/r04-release ]; do
    [ -d hooks ] && [ "$remaining" -gt 0 ] || exit 1
    remaining=$((remaining - 1))
    sleep 0.05
done
"#,
        )
        .unwrap();
        fs::set_permissions(hook, fs::Permissions::from_mode(0o700)).unwrap();
        Self(hooks)
    }

    async fn wait_for_offer(&self) -> String {
        tokio::time::timeout(DEADLINE, async {
            while !self.0.join("r04-ready").exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the real Git receive hook did not observe a push");
        fs::read_to_string(self.0.join("r04-offers")).unwrap()
    }
}

impl Drop for ReceiveGate {
    fn drop(&mut self) {
        let _ = fs::write(self.0.join("r04-release"), b"release");
    }
}

async fn start_full_import(client: &Client, repo_url: &str) -> String {
    let response = client
        .post(format!("{repo_url}/import"))
        .header("x-request-id", "r04-full-import")
        .json(&serde_json::json!({"import_mode": "full"}))
        .timeout(DEADLINE)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body: Value = response.json().await.unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["import_mode"], "full");
    assert_eq!(body["lifecycle"], "queued");
    body["operation_id"].as_str().unwrap().to_owned()
}

async fn disable(client: &Client, repo_url: &str) -> (StatusCode, Value) {
    let response = client
        .delete(repo_url)
        .timeout(DEADLINE)
        .send()
        .await
        .unwrap();
    (response.status(), response.json().await.unwrap())
}

async fn join_import(state: &AppState) {
    let mut handles = std::mem::take(&mut *state.import_handles.lock().await);
    assert_eq!(handles.len(), 1, "expected exactly one admitted worker");
    tokio::time::timeout(DEADLINE, handles.pop().unwrap())
        .await
        .expect("import worker timed out")
        .expect("import worker panicked");
}

fn assert_idle_disable(status: StatusCode, body: Value) {
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["action"], "disable");
    assert_eq!(body["enabled"], false);
    assert_eq!(body["managed_removal"], false);
}

fn main_ref(bare: &Path) -> Option<String> {
    let repo = git2::Repository::open_bare(bare).unwrap();
    let result = repo.refname_to_id(MAIN);
    match result {
        Ok(oid) => Some(oid.to_string()),
        Err(error) if error.code() == git2::ErrorCode::NotFound => None,
        Err(error) => panic!("cannot inspect the bare main ref: {error}"),
    }
}

// Read the persisted receipt, both checkpoint copies, the scoped mapping
// for each commit, and the source and destination history independently.
fn assert_completed(
    tmp: &Path,
    bare: &Path,
    id: &str,
    op_id: &str,
    enabled: bool,
) -> Vec<(i64, String)> {
    let reopened = Database::new(tmp.join("reposync.db")).unwrap();
    let repo = reopened.get_repository(id).unwrap().unwrap();
    assert_eq!(repo.enabled, enabled);
    assert!(reopened.managed_removal(id).unwrap().is_none());
    assert!(reopened.removal_tombstone(id).unwrap().is_none());
    assert!(reopened.active_import_operation(id).unwrap().is_none());
    let op = reopened.latest_import_operation(id).unwrap().unwrap();
    assert_eq!(op.id, op_id);
    assert_eq!(op.state, ImportOperationState::Completed);
    assert_eq!(op.confirmed_batches, 1);
    assert_eq!(op.last_confirmed_svn_rev, Some(3));
    assert!(op.pending_commit_maps.is_empty());

    let history = String::from_utf8(rs67_command(
        "git",
        &[
            "--git-dir",
            bare.to_str().unwrap(),
            "rev-list",
            "--reverse",
            MAIN,
        ],
    ))
    .unwrap();
    let commits: Vec<_> = history.lines().collect();
    assert_eq!(commits.len(), 3);
    let tip = commits[2];
    assert_eq!(op.last_confirmed_git_sha.as_deref(), Some(tip));
    assert_eq!((repo.last_svn_rev, repo.last_git_sha.as_str()), (3, tip));
    assert_eq!(
        reopened
            .get_state(&format!("last_svn_rev_{id}"))
            .unwrap()
            .as_deref(),
        Some("3")
    );
    assert_eq!(inbound_git_sha_kv(&reopened, id).as_deref(), Some(tip));
    let mapping = reopened.list_commit_map(4).unwrap();
    assert_eq!(mapping.len(), commits.len());
    let svn = tmp.join("svn-repo");
    let svn = svn.to_str().unwrap();
    let trunk = format!("file://{svn}/trunk");
    assert_eq!(rs67_command("svnlook", &["youngest", svn]), b"3\n");
    for (index, (row, sha)) in mapping.iter().rev().zip(commits).enumerate() {
        let rev = index + 1;
        assert_eq!(row.repo_id.as_deref(), Some(id));
        assert_eq!(row.direction, "svn_to_git");
        assert_eq!(row.svn_rev, rev as i64);
        assert_eq!(row.git_sha, sha);
        let source_files = rs67_command(
            "svn",
            &[
                "list",
                "--non-interactive",
                "-R",
                "-r",
                &rev.to_string(),
                &trunk,
            ],
        );
        let target_files = rs67_command(
            "git",
            &[
                "--git-dir",
                bare.to_str().unwrap(),
                "ls-tree",
                "-r",
                "--name-only",
                sha,
            ],
        );
        assert_eq!(
            source_files,
            if rev == 1 {
                &b""[..]
            } else {
                &b"history.txt\n"[..]
            }
        );
        assert_eq!(target_files, source_files, "inventory at r{rev}");
        if rev > 1 {
            let source = rs67_command(
                "svnlook",
                &["cat", "-r", &rev.to_string(), svn, "trunk/history.txt"],
            );
            assert_eq!(
                source,
                if rev == 2 {
                    &b"first\n"[..]
                } else {
                    &b"second\n"[..]
                }
            );
            assert_eq!(git_show(bare, &format!("{sha}:history.txt")), source);
        }
    }
    let worktree = git2::Repository::open(tmp.join("repos").join(id).join("git-repo")).unwrap();
    assert_eq!(worktree.head().unwrap().target().unwrap().to_string(), tip);
    mapping
        .into_iter()
        .rev()
        .map(|r| (r.svn_rev, r.git_sha))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r04_active_import_excludes_even_a_prechecked_legacy_disable() {
    let (addr, state, server, tmp, id, bare) = import_fixture().await;
    let client = authed_client();
    let url = format!("http://{addr}/api/repos/{id}");
    let prechecked = Database::new(tmp.path().join("reposync.db")).unwrap();
    let originally_enabled = prechecked.get_repository(&id).unwrap().unwrap().enabled;
    assert!(originally_enabled);
    let initial_ref = main_ref(&bare);
    assert_eq!(initial_ref, None);
    // First half of disable_repo_inner on an independent SQLite connection.
    assert!(prechecked.active_import_operation(&id).unwrap().is_none());
    let initial_svn_kv = prechecked.get_state(&format!("last_svn_rev_{id}")).unwrap();
    let initial_git_kv = inbound_git_sha_kv(&prechecked, &id);
    let gate = ReceiveGate::install(&bare);
    let op_id = start_full_import(&client, &url).await;
    let offer = gate.wait_for_offer().await;
    let op = state.db.active_import_operation(&id).unwrap().unwrap();
    assert_eq!(op.id, op_id);
    assert_eq!(op.state, ImportOperationState::Running);
    assert_eq!(op.intended_ref.as_deref(), Some(MAIN));
    let expected = op.intended_git_sha.as_deref().unwrap();
    assert_eq!(
        offer,
        format!("{} {expected} {MAIN}\n", "0".repeat(expected.len()))
    );
    assert_eq!(op.last_local_svn_rev, Some(3));
    assert_eq!(op.last_local_git_sha.as_deref(), Some(expected));
    assert_eq!(op.confirmed_batches, 0);
    assert_eq!(op.last_confirmed_git_sha, None);
    assert_eq!(main_ref(&bare), initial_ref);
    assert!(prechecked.list_commit_map(1).unwrap().is_empty());
    assert_eq!(
        prechecked.get_repo_watermark(&id).unwrap(),
        (0, String::new())
    );
    assert_eq!(
        prechecked.get_state(&format!("last_svn_rev_{id}")).unwrap(),
        initial_svn_kv
    );
    assert_eq!(inbound_git_sha_kv(&prechecked, &id), initial_git_kv);

    let (status, body) = disable(&client, &url).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body["error"].as_str().unwrap().contains("import"), "{body}");
    // Second half of the older request: drop its stale precheck and test the
    // actual writer while this very same offer and active journal are held.
    let write = prechecked.legacy_disable_repository(&id);
    let still_enabled = state.db.get_repository(&id).unwrap().unwrap().enabled;
    drop(gate);
    join_import(&state).await;
    let error = write.expect_err("a prechecked legacy disable committed over an active import");
    assert!(error.to_string().contains("import"), "{error}");
    assert!(still_enabled);
    let mapped = assert_completed(tmp.path(), &bare, &id, &op_id, true);
    assert_eq!(mapped[2].1, expected);
    let (status, body) = disable(&client, &url).await;
    assert_idle_disable(status, body);
    assert_eq!(
        assert_completed(tmp.path(), &bare, &id, &op_id, false),
        mapped
    );
    assert_eq!(
        fs::read_to_string(bare.join("hooks/r04-offers")).unwrap(),
        offer
    );
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"R04_IMPORT_FIRST", "repo_id":id, "operation_id":op_id, "mapped_revisions":mapped,
            "bare_ref_before":initial_ref, "bare_ref_after":main_ref(&bare), "receive_offer":offer.trim_end(),
            "enabled_before":originally_enabled, "enabled_during_hold":still_enabled,
            "enabled_after_idle_delete":state.db.get_repository(&id).unwrap().unwrap().enabled
        })
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r04_idle_disable_allows_explicit_import_on_paused_registration() {
    let (addr, state, server, tmp, id, bare) = import_fixture().await;
    let client = authed_client();
    let url = format!("http://{addr}/api/repos/{id}");
    let originally_enabled = state.db.get_repository(&id).unwrap().unwrap().enabled;
    assert!(originally_enabled);
    let initial_ref = main_ref(&bare);
    assert_eq!(initial_ref, None);
    let (status, body) = disable(&client, &url).await;
    assert_idle_disable(status, body);
    let paused = state.db.get_repository(&id).unwrap().unwrap();
    assert!(!paused.enabled);
    assert_eq!(main_ref(&bare), initial_ref);
    assert!(state.db.latest_import_operation(&id).unwrap().is_none());
    // The repository detail import card consults this flag for an explicit
    // admin start, including when normal sync has been disabled.
    let status: Value = client
        .get(format!("{url}/import/status"))
        .timeout(DEADLINE)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["can_start"], true);
    let op_id = start_full_import(&client, &url).await;
    join_import(&state).await;
    let mapped = assert_completed(tmp.path(), &bare, &id, &op_id, false);
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"R04_DISABLE_FIRST", "repo_id":id, "operation_id":op_id, "mapped_revisions":mapped,
            "bare_ref_before":initial_ref, "bare_ref_after":main_ref(&bare),
            "enabled_before":originally_enabled, "enabled_after_idle_delete":paused.enabled,
            "can_start_while_disabled":status["can_start"],
            "enabled_after_explicit_import":state.db.get_repository(&id).unwrap().unwrap().enabled
        })
    );
    server.abort();
}
