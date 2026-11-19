use super::*;
use reposync_web::{api, WebServer};
use serde_json::json;
use std::cell::Cell;
use std::sync::mpsc;
use tokio::sync::oneshot;

type Reply = (StatusCode, Value);

async fn put(client: &Client, url: &str, body: &Value) -> Reply {
    let response = client
        .put(url)
        .json(body)
        .timeout(DEADLINE)
        .send()
        .await
        .unwrap();
    (response.status(), response.json().await.unwrap())
}

fn save_body(mut body: Value, receipt: bool) -> Value {
    if receipt {
        body["save_request_id"] = json!(uuid::Uuid::new_v4().to_string());
    }
    body
}

async fn lookup(client: &Client, url: &str, body: &Value) -> Reply {
    let key = body["save_request_id"].as_str().unwrap();
    let response = client
        .get(format!("{url}/save-requests/{key}"))
        .timeout(DEADLINE)
        .send()
        .await
        .unwrap();
    (response.status(), response.json().await.unwrap())
}

// A separate loopback server uses the same database file. Its one runtime
// thread owns the SQLite callback; the existing fixture can independently
// admit an import or change the registration while a request is at its write.
thread_local! {
    static HANDOFF: Cell<Option<(oneshot::Sender<()>, mpsc::Receiver<()>)>> = const { Cell::new(None) };
}

fn wait_for_peer(_: i32) -> bool {
    HANDOFF.take().is_some_and(|(waiting, released)| {
        waiting.send(()).is_ok() && released.recv_timeout(DEADLINE).is_ok()
    })
}

enum Request {
    Put(Value),
    StartImport,
}

struct PendingRequest {
    waited: bool,
    release: mpsc::Sender<()>,
    reply: tokio::task::JoinHandle<(Reply, usize)>,
}

impl PendingRequest {
    async fn finish(self) -> (bool, Reply, usize) {
        let _ = self.release.send(());
        let (response, workers) = tokio::time::timeout(DEADLINE, self.reply)
            .await
            .unwrap()
            .unwrap();
        (self.waited, response, workers)
    }
}

async fn request_at_write(state: &AppState, id: &str, request: Request) -> PendingRequest {
    let path = state.config.daemon.data_dir.join("reposync.db");
    let settings = WebServer::new(
        state.config.clone(),
        Database::new(&path).unwrap(),
        state.sync_engine.clone(),
        state.sync_trigger.clone(),
        state.config_path.clone(),
        state.import_progress.clone(),
    )
    .app_state();
    settings
        .sessions
        .write()
        .await
        .clone_from(&*state.sessions.read().await);
    let mut peer = rusqlite::Connection::open(path).unwrap();
    let reservation = peer
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    let (waiting_tx, waiting_rx) = oneshot::channel();
    let (release, released) = mpsc::channel();
    let id = id.to_owned();
    let reply = tokio::task::spawn_blocking(move || {
        HANDOFF.set(Some((waiting_tx, released)));
        settings
            .db
            .conn()
            .busy_handler(Some(wait_for_peer))
            .unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let reply = rt.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let app = api::repos::routes().with_state(settings.clone());
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let client = authed_client();
            let url = format!("http://{addr}/api/repos/{id}");
            let response = match request {
                Request::Put(body) => put(&client, &url, &body).await,
                Request::StartImport => {
                    let response = client
                        .post(format!("{url}/import"))
                        .header("x-request-id", "r04-stale-target")
                        .json(&json!({"import_mode":"full"}))
                        .timeout(DEADLINE)
                        .send()
                        .await
                        .unwrap();
                    (response.status(), response.json().await.unwrap())
                }
            };
            // Let an erroneously admitted worker finish on the disposable
            // target. Closing this runtime early could hide its effects.
            let handles = std::mem::take(&mut *settings.import_handles.lock().await);
            let workers = handles.len();
            for handle in handles {
                tokio::time::timeout(DEADLINE, handle)
                    .await
                    .unwrap()
                    .unwrap();
            }
            server.abort();
            (response, workers)
        });
        // A deferred snapshot upgrade can return BUSY without a callback.
        // Close the sender in that case and report the actual HTTP response.
        drop(HANDOFF.take());
        reply
    });
    let waited = tokio::time::timeout(DEADLINE, waiting_rx)
        .await
        .expect("request did not reach SQLite or return")
        .is_ok();
    reservation.rollback().unwrap();
    PendingRequest {
        waited,
        release,
        reply,
    }
}

fn assert_import_refusal((status, body): Reply) {
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body["error"].as_str().unwrap().contains("import"), "{body}");
    assert!(body.get("save_receipt").is_none());
}

async fn put_races_import(receipt: bool) {
    let (addr, state, server, tmp, id, bare) = import_fixture().await;
    let client = authed_client();
    let url = format!("http://{addr}/api/repos/{id}");
    let before = state.db.get_repository(&id).unwrap().unwrap();
    assert!(before.enabled);
    let known_body = save_body(json!({"name":before.name}), receipt);
    let known = if receipt {
        let (status, result) = put(&client, &url, &known_body).await;
        assert_eq!(status, StatusCode::OK, "{result}");
        Some(result["save_receipt"].clone())
    } else {
        None
    };

    let body = save_body(
        json!({"name":"unapplied settings","enabled":false}),
        receipt,
    );
    let pending = request_at_write(&state, &id, Request::Put(body.clone())).await;
    let gate = ReceiveGate::install(&bare);
    let op_id = start_full_import(&client, &url).await;
    let offer = gate.wait_for_offer().await;
    let op = state.db.active_import_operation(&id).unwrap().unwrap();
    assert_eq!(op.id, op_id);
    assert_eq!(op.state, ImportOperationState::Running);
    let sha = op.intended_git_sha.as_deref().unwrap();
    assert_eq!(offer, format!("{} {sha} {MAIN}\n", "0".repeat(sha.len())));
    assert_eq!(main_ref(&bare), None);
    let held_updated_at = state.db.get_repository(&id).unwrap().unwrap().updated_at;
    assert_import_refusal(put(&client, &url, &body).await);
    if let Some(known) = known {
        let (status, replay) = put(&client, &url, &known_body).await;
        assert_eq!(status, StatusCode::OK, "{replay}");
        assert_eq!(replay["save_receipt"], known);
        assert_eq!(
            lookup(&client, &url, &known_body).await,
            (StatusCode::OK, known)
        );
    }
    let (waited, response, workers) = pending.finish().await;
    let held = state.db.get_repository(&id).unwrap().unwrap();
    let held_op = state.db.active_import_operation(&id).unwrap().unwrap();
    drop(gate);
    join_import(&state).await;
    assert_import_refusal(response);
    assert_eq!(workers, 0);
    assert!(
        waited,
        "the original request must reach its writer reservation"
    );
    assert_eq!(held_op, op);
    assert!(held.enabled);
    assert_eq!(held.name, before.name);
    assert_eq!(held.updated_at, held_updated_at);
    if receipt {
        assert_eq!(lookup(&client, &url, &body).await.0, StatusCode::NOT_FOUND);
    }
    let mapped = assert_completed(tmp.path(), &bare, &id, &op_id, true);
    assert_eq!(mapped[2].1, sha);
    assert_eq!(
        fs::read_to_string(bare.join("hooks/r04-offers")).unwrap(),
        offer
    );
    let case = if receipt {
        "R04_RECEIPTED_PUT_IMPORT_FIRST"
    } else {
        "R04_PLAIN_PUT_IMPORT_FIRST"
    };
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        json!({"case":case,"repo_id":id,
        "operation_id":op_id,"settings_request_id":body["save_request_id"],"receive_offer":offer.trim_end(),
        "mapped_revisions":mapped,"blocked_http":400,"enabled_during_hold":held.enabled})
    );
    server.abort();
}

async fn rename_races_pause(receipt: bool) {
    let (addr, state, server, tmp, id, bare) = import_fixture().await;
    let client = authed_client();
    let url = format!("http://{addr}/api/repos/{id}");
    let body = save_body(json!({"name":"operator rename"}), receipt);
    let pending = request_at_write(&state, &id, Request::Put(body.clone())).await;
    let pause = save_body(json!({"enabled":false,"poll_interval_secs":181}), receipt);
    let (status, paused) = put(&client, &url, &pause).await;
    assert_eq!(status, StatusCode::OK, "{paused}");
    assert_eq!(paused["enabled"], false);
    assert_eq!(paused["poll_interval_secs"], 181);
    assert!(state.db.latest_import_operation(&id).unwrap().is_none());
    assert_eq!(main_ref(&bare), None);
    if receipt {
        assert_eq!(
            lookup(&client, &url, &pause).await,
            (StatusCode::OK, paused["save_receipt"].clone())
        );
    }
    let op_id = start_full_import(&client, &url).await;
    join_import(&state).await;
    let mapped = assert_completed(tmp.path(), &bare, &id, &op_id, false);
    let (waited, (status, renamed), workers) = pending.finish().await;
    assert_eq!(workers, 0);
    assert_eq!(status, StatusCode::OK, "{renamed}");
    assert!(waited, "the rename must run across an intervening pause");
    assert_eq!(renamed["id"], id);
    assert_eq!(renamed["name"], "operator rename");
    assert_eq!(renamed["enabled"], false, "{renamed}");
    assert_eq!(renamed["poll_interval_secs"], 181);
    assert_eq!(renamed["initializing"], false);
    let current = state.db.get_repository(&id).unwrap().unwrap();
    assert_eq!(current.name, "operator rename");
    assert_eq!(current.poll_interval_secs, 181);
    assert_eq!(renamed["status"], current.sync_status);
    assert_eq!(renamed["updated_at"], current.updated_at);
    if receipt {
        assert_eq!(
            renamed["save_receipt"]["request_id"],
            body["save_request_id"]
        );
        assert_eq!(
            renamed["save_receipt"]["configuration_updated_at"],
            current.updated_at
        );
        assert_eq!(
            lookup(&client, &url, &body).await,
            (StatusCode::OK, renamed["save_receipt"].clone())
        );
    } else {
        assert!(renamed.get("save_receipt").is_none());
    }
    assert_eq!(
        assert_completed(tmp.path(), &bare, &id, &op_id, false),
        mapped
    );
    let case = if receipt {
        "R04_RECEIPTED_PUT_PAUSE_FIRST"
    } else {
        "R04_PLAIN_PUT_PAUSE_FIRST"
    };
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        json!({"case":case,"repo_id":id,
        "operation_id":op_id,"settings_request_id":body["save_request_id"],"mapped_revisions":mapped,
        "persisted_enabled":current.enabled,"response_enabled":renamed["enabled"],"response_status":renamed["status"]})
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r04_plain_put_refuses_newly_admitted_import() {
    put_races_import(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r04_receipted_put_refuses_newly_admitted_import() {
    put_races_import(true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r04_plain_rename_preserves_newer_pause_and_import() {
    rename_races_pause(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r04_receipted_rename_preserves_newer_pause_and_import() {
    rename_races_pause(true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r04_changed_target_refuses_stale_import_before_enrollment() {
    const NEXT: &str = "refs/heads/managed-next";
    const CHANGED: &str =
        "repository import target changed before admission; review the current settings and retry";
    let (addr, state, server, tmp, id, bare) = import_fixture().await;
    let client = authed_client();
    let url = format!("http://{addr}/api/repos/{id}");
    let repo = state.db.get_repository(&id).unwrap().unwrap();
    assert_eq!(repo.git_branch, "main");
    let svn_key = format!("last_svn_rev_{id}");
    let initial_svn_kv = state.db.get_state(&svn_key).unwrap();
    let initial_git_kv = inbound_git_sha_kv(&state.db, &id);
    let worktree = tmp.path().join("repos").join(&id).join("git-repo");
    assert!(!worktree.exists());
    // Record every proposal to the bare target without holding its publication.
    drop(ReceiveGate::install(&bare));
    let offers = bare.join("hooks/r04-offers");
    let refs = || {
        rs67_command(
            "git",
            &[
                "--git-dir",
                bare.to_str().unwrap(),
                "for-each-ref",
                "--format=%(refname) %(objectname)",
                "refs/heads",
            ],
        )
    };
    assert!(refs().is_empty());

    let pending = request_at_write(&state, &id, Request::StartImport).await;
    assert!(
        pending.waited,
        "the real start must reach SQLite enrollment"
    );
    assert!(
        reposync_core::busy::is_busy(&id),
        "the handler must hold the worker slot after its initial target read"
    );
    assert!(state.db.latest_import_operation(&id).unwrap().is_none());
    let (status, updated) = put(&client, &url, &json!({"git_branch":"managed-next"})).await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["git_branch"], "managed-next");
    let (_, (status, refused), workers) = pending.finish().await;
    let peer = Database::new(tmp.path().join("reposync.db")).unwrap();
    let observed = String::from_utf8(refs()).unwrap();
    let watermark = peer.get_repo_watermark(&id).unwrap();
    let latest = peer.latest_import_operation(&id).unwrap();
    assert_eq!(status, StatusCode::CONFLICT,
        "stale start: {refused}; admitted workers={workers}; bare refs={observed:?}; durable watermark={watermark:?}; latest={:?}",
        latest.as_ref().map(|op| (&op.id, &op.state)));
    assert_eq!(refused["error"], CHANGED);
    assert!(refused.get("operation_id").is_none());
    assert_eq!(workers, 0);
    assert!(!reposync_core::busy::is_busy(&id));
    assert!(latest.is_none() && peer.active_import_operation(&id).unwrap().is_none());
    let documents: i64 = peer
        .conn()
        .query_row(
            "SELECT count(*) FROM kv_state WHERE key GLOB 'import_operation_v1:document:*'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(documents, 0);
    assert_eq!(watermark, (0, String::new()));
    assert_eq!(peer.get_state(&svn_key).unwrap(), initial_svn_kv);
    assert_eq!(inbound_git_sha_kv(&peer, &id), initial_git_kv);
    assert!(peer.list_commit_map(1).unwrap().is_empty());
    assert_eq!(peer.count_sync_records().unwrap(), 0);
    let current = peer.get_repository(&id).unwrap().unwrap();
    assert_eq!(current.git_branch, "managed-next");
    assert_eq!(updated["updated_at"], current.updated_at);
    assert!(observed.is_empty() && !offers.exists() && !worktree.exists());

    let fresh_id = start_full_import(&client, &url).await;
    join_import(&state).await;
    let mapped = assert_completed_at(tmp.path(), &bare, &id, &fresh_id, true, NEXT);
    let tip = &mapped[2].1;
    assert_eq!(
        String::from_utf8(refs()).unwrap(),
        format!("{NEXT} {tip}\n")
    );
    let fresh_offer = fs::read_to_string(offers).unwrap();
    assert_eq!(
        fresh_offer,
        format!("{} {tip} {NEXT}\n", "0".repeat(tip.len()))
    );
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        json!({
            "case":"R04_TARGET_CHANGE_BEFORE_IMPORT_ADMISSION","repo_id":id,"stale_start_http":status.as_u16(),
            "stale_workers":workers,"stale_operation_documents":documents,"stale_bare_refs":observed,
            "fresh_operation_id":fresh_id,"fresh_receive_offer":fresh_offer.trim_end(),"mapped_revisions":mapped
        })
    );
    server.abort();
}
