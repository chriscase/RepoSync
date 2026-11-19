use super::r04_import_disable::{main_ref, ReceiveGate};
use super::{
    authed_client, import_fixture, inbound_git_sha_kv, register_import_history, rs67_command,
    seed_import_history,
};
use git2::{Oid, Repository};
use reposync_core::db::import_operations::{ImportOperation, ImportOperationState};
use reposync_core::db::Database;
use reqwest::Client;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

const MAIN: &str = "refs/heads/main";
const REQUEST: &str = "r17-shared-import-request";
const TIMEOUT: Duration = Duration::from_secs(60);

struct Pair {
    id: String,
    import_url: String,
    source: PathBuf,
    bare: PathBuf,
    checkout: PathBuf,
    contents: [&'static str; 2],
}

impl Pair {
    fn new(
        addr: SocketAddr,
        data: &Path,
        id: String,
        source: PathBuf,
        bare: PathBuf,
        contents: [&'static str; 2],
    ) -> Self {
        // Bare repositories normally do not keep reflogs. Record actual ref
        // updates independently of the attempts observed by the receive hook.
        Repository::open_bare(&bare)
            .unwrap()
            .config()
            .unwrap()
            .set_bool("core.logAllRefUpdates", true)
            .unwrap();
        assert!(main_ref(&bare).is_none());
        Self {
            import_url: format!("http://{addr}/api/repos/{id}/import"),
            checkout: data.join("repos").join(&id).join("git-repo"),
            id,
            source,
            bare,
            contents,
        }
    }

    async fn request(&self, client: &Client) -> Value {
        client
            .post(&self.import_url)
            .header("x-request-id", REQUEST)
            .json(&json!({"import_mode":"full"}))
            .timeout(TIMEOUT)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap()
    }

    async fn start(&self, client: &Client) -> String {
        let response = self.request(client).await;
        assert_eq!(response["ok"], true, "{response}");
        assert_eq!(response["import_mode"], "full", "{response}");
        assert_eq!(response["lifecycle"], "queued", "{response}");
        response["operation_id"].as_str().unwrap().to_owned()
    }

    async fn check_status_and_retry(
        &self,
        client: &Client,
        op_id: &str,
        lifecycle: &str,
        tip: &str,
    ) {
        let status: Value = client
            .get(format!("{}/status", self.import_url))
            .timeout(TIMEOUT)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(status["operation_id"], op_id, "{status}");
        assert_eq!(status["lifecycle"], lifecycle, "{status}");
        assert_eq!(status["last_local_git_sha"], tip, "{status}");
        let replay = self.request(client).await;
        assert_eq!(replay["ok"], true, "{replay}");
        assert_eq!(replay["operation_id"], op_id, "{replay}");
        assert_eq!(replay["lifecycle"], lifecycle, "{replay}");
    }

    fn cursor_copies(&self, db: &Database) -> (Option<String>, Option<String>) {
        (
            db.get_state(&format!("last_svn_rev_{}", self.id)).unwrap(),
            inbound_git_sha_kv(db, &self.id),
        )
    }

    fn held(
        &self,
        db: &Database,
        op_id: &str,
        offer: &str,
        original_copies: &(Option<String>, Option<String>),
    ) -> ImportOperation {
        let op = db.active_import_operation(&self.id).unwrap().unwrap();
        assert_eq!(op.id, op_id);
        assert_eq!(op.repo_id, self.id);
        assert_eq!(op.request_id, REQUEST);
        assert_eq!(op.operation_type, "full_import");
        assert_eq!(op.state, ImportOperationState::Running);
        assert_eq!(op.intended_ref.as_deref(), Some(MAIN));
        let tip = op.intended_git_sha.as_deref().unwrap();
        assert_eq!(offer, format!("{} {tip} {MAIN}\n", Oid::zero()));
        assert_eq!(op.last_local_git_sha.as_deref(), Some(tip));
        assert_eq!(op.last_local_svn_rev, Some(3));
        assert_eq!(op.confirmed_batches, 0);
        assert!(op.last_confirmed_git_sha.is_none());
        assert!(op.last_confirmed_svn_rev.is_none());
        assert_eq!(op.pending_commit_maps.len(), 3);
        assert!(main_ref(&self.bare).is_none());
        assert_eq!(db.get_repo_watermark(&self.id).unwrap(), (0, String::new()));
        assert_eq!(&self.cursor_copies(db), original_copies);
        assert!(db
            .list_commit_map(7)
            .unwrap()
            .iter()
            .all(|r| r.repo_id.as_deref() != Some(self.id.as_str())));
        let checkout = Repository::open(&self.checkout).unwrap();
        assert_eq!(checkout.head().unwrap().target().unwrap().to_string(), tip);
        assert_eq!(
            std::fs::read_to_string(self.checkout.join("history.txt")).unwrap(),
            self.contents[1]
        );
        op
    }

    fn completed(&self, db: &Database, held: &ImportOperation, offer: &str) -> Value {
        let op = db.latest_import_operation(&self.id).unwrap().unwrap();
        assert!(db.active_import_operation(&self.id).unwrap().is_none());
        assert_eq!(op.id, held.id);
        assert_eq!(op.repo_id, self.id);
        assert_eq!(op.request_id, REQUEST);
        assert_eq!(op.state, ImportOperationState::Completed);
        assert_eq!(op.confirmed_batches, 1);
        assert!(op.pending_commit_maps.is_empty());
        assert_eq!(op.last_confirmed_svn_rev, Some(3));
        let tip = main_ref(&self.bare).unwrap();
        assert_eq!(op.last_confirmed_git_sha.as_deref(), Some(tip.as_str()));
        assert_eq!(held.intended_git_sha.as_deref(), Some(tip.as_str()));
        let checkpoint = db.get_repo_watermark(&self.id).unwrap();
        assert_eq!(checkpoint, (3, tip.clone()));
        assert_eq!(
            self.cursor_copies(db),
            (Some("3".into()), Some(tip.clone()))
        );

        let git = Repository::open_bare(&self.bare).unwrap();
        let reflog = git.reflog(MAIN).unwrap();
        assert_eq!(reflog.len(), 1);
        let update = reflog.get(0).unwrap();
        assert_eq!(update.id_old(), Oid::zero());
        assert_eq!(update.id_new().to_string(), tip);
        assert_eq!(
            std::fs::read_to_string(self.bare.join("hooks/r04-offers")).unwrap(),
            offer
        );

        let svn = self.source.to_str().unwrap();
        let trunk = format!("file://{svn}/trunk");
        assert_eq!(rs67_command("svnlook", &["youngest", svn]), b"3\n");
        let mut rows: Vec<_> = db
            .list_commit_map(7)
            .unwrap()
            .into_iter()
            .filter(|r| r.repo_id.as_deref() == Some(self.id.as_str()))
            .collect();
        rows.sort_by_key(|r| r.svn_rev);
        assert_eq!(rows.len(), 3);
        let mut parent = None;
        let mut mapped = Vec::new();
        for (index, row) in rows.iter().enumerate() {
            let revision = index + 1;
            assert_eq!(row.svn_rev, revision as i64);
            assert_eq!(row.direction, "svn_to_git");
            let staged = &held.pending_commit_maps[index];
            assert_eq!(
                (staged.svn_rev, &staged.git_sha),
                (row.svn_rev, &row.git_sha)
            );
            let commit = git
                .find_commit(Oid::from_str(&row.git_sha).unwrap())
                .unwrap();
            assert_eq!(
                commit.parent_ids().collect::<Vec<_>>(),
                parent.into_iter().collect::<Vec<_>>()
            );
            parent = Some(commit.id());
            let tree = commit.tree().unwrap();
            let rev = revision.to_string();
            let files = rs67_command("svn", &["list", "-R", "-r", &rev, &trunk]);
            if index == 0 {
                assert!(files.is_empty());
                assert!(tree.is_empty());
            } else {
                assert_eq!(files, b"history.txt\n");
                assert_eq!(tree.len(), 1);
                let blob = git
                    .find_blob(tree.get_name("history.txt").unwrap().id())
                    .unwrap();
                let source =
                    rs67_command("svnlook", &["cat", "-r", &rev, svn, "trunk/history.txt"]);
                assert_eq!(source, self.contents[index - 1].as_bytes());
                assert_eq!(blob.content(), source);
            }
            mapped.push((row.svn_rev, &row.git_sha));
        }
        assert_eq!(parent.unwrap().to_string(), tip);
        json!({"repo_id":self.id,"operation_id":op.id,"receive_offer":offer.trim_end(),
            "final_ref":tip,"mappings":mapped,"checkpoint":checkpoint,"ref_updates":reflog.len(),
            "confirmed_batches":op.confirmed_batches})
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_full_imports_publish_independently() {
    let (addr, state, server, tmp, id, bare) = import_fixture().await;
    let data = tmp.path();
    let alpha = Pair::new(
        addr,
        data,
        id,
        data.join("svn-repo"),
        bare,
        ["first\n", "second\n"],
    );
    let beta_root = data.join("beta");
    let beta_contents = ["beta-first\n", "beta-second\n"];
    let (svn_url, bare) = seed_import_history(&beta_root, "beta", beta_contents);
    let id = register_import_history(addr, &beta_root, &svn_url, "beta").await;
    let beta = Pair::new(
        addr,
        data,
        id,
        beta_root.join("svn-repo"),
        bare,
        beta_contents,
    );
    assert_ne!(alpha.id, beta.id);

    let client = authed_client();
    let alpha_gate = ReceiveGate::install(&alpha.bare);
    let beta_gate = ReceiveGate::install(&beta.bare);
    let database = data.join("reposync.db");
    let db = Database::new(&database).unwrap();
    let alpha_copies = alpha.cursor_copies(&db);
    let beta_copies = beta.cursor_copies(&db);
    let alpha_id = alpha.start(&client).await;
    let alpha_offer = alpha_gate.wait_for_offer().await;
    let beta_id = beta.start(&client).await;
    assert_ne!(alpha_id, beta_id);
    let beta_offer = beta_gate.wait_for_offer().await;
    assert!(db.list_commit_map(7).unwrap().is_empty());
    let alpha_op = alpha.held(&db, &alpha_id, &alpha_offer, &alpha_copies);
    let beta_op = beta.held(&db, &beta_id, &beta_offer, &beta_copies);
    assert!(db
        .get_import_operation(&alpha.id, &beta_id)
        .unwrap()
        .is_none());
    assert!(db
        .get_import_operation(&beta.id, &alpha_id)
        .unwrap()
        .is_none());
    let alpha_tip = alpha_op.intended_git_sha.as_deref().unwrap();
    let beta_tip = beta_op.intended_git_sha.as_deref().unwrap();
    assert_ne!(alpha_tip, beta_tip);
    alpha
        .check_status_and_retry(&client, &alpha_id, "running", alpha_tip)
        .await;
    beta.check_status_and_retry(&client, &beta_id, "running", beta_tip)
        .await;

    // Wait for the second admitted worker to finish while the first is still at Git.
    let mut workers = std::mem::take(&mut *state.import_handles.lock().await);
    assert_eq!(workers.len(), 2);
    drop(beta_gate);
    tokio::time::timeout(TIMEOUT, workers.pop().unwrap())
        .await
        .unwrap()
        .unwrap();
    let mid = Database::new(&database).unwrap();
    assert_eq!(mid.list_commit_map(7).unwrap().len(), 3);
    let beta_receipt = beta.completed(&mid, &beta_op, &beta_offer);
    assert_eq!(
        alpha.held(&mid, &alpha_id, &alpha_offer, &alpha_copies),
        alpha_op
    );
    alpha
        .check_status_and_retry(&client, &alpha_id, "running", alpha_tip)
        .await;
    beta.check_status_and_retry(&client, &beta_id, "completed", beta_tip)
        .await;

    drop(alpha_gate);
    tokio::time::timeout(TIMEOUT, workers.pop().unwrap())
        .await
        .unwrap()
        .unwrap();
    alpha
        .check_status_and_retry(&client, &alpha_id, "completed", alpha_tip)
        .await;
    beta.check_status_and_retry(&client, &beta_id, "completed", beta_tip)
        .await;
    assert!(state.import_handles.lock().await.is_empty());
    let reopened = Database::new(&database).unwrap();
    assert_eq!(reopened.list_commit_map(7).unwrap().len(), 6);
    let alpha_receipt = alpha.completed(&reopened, &alpha_op, &alpha_offer);
    assert_eq!(
        beta.completed(&reopened, &beta_op, &beta_offer),
        beta_receipt
    );
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        json!({
            "case":"R17_MANAGED_FULL_IMPORT_OVERLAP", "status":"PASS", "request_id":REQUEST,
            "completed_while_peer_held":beta_id, "still_held":alpha_id,
            "repositories":[alpha_receipt,beta_receipt]
        })
    );
    server.abort();
}
