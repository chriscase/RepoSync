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
// admit an import or change the registration while this PUT is at its write.
thread_local! {
    static HANDOFF: Cell<Option<(oneshot::Sender<()>, mpsc::Receiver<()>)>> = const { Cell::new(None) };
}

fn wait_for_peer(_: i32) -> bool {
    HANDOFF.take().is_some_and(|(waiting, released)| {
        waiting.send(()).is_ok() && released.recv_timeout(DEADLINE).is_ok()
    })
}

struct PendingPut {
    waited: bool,
    release: mpsc::Sender<()>,
    reply: tokio::task::JoinHandle<Reply>,
}

impl PendingPut {
    async fn finish(self) -> (bool, Reply) {
        let _ = self.release.send(());
        (
            self.waited,
            tokio::time::timeout(DEADLINE, self.reply)
                .await
                .unwrap()
                .unwrap(),
        )
    }
}

async fn put_at_write(state: &AppState, id: &str, body: Value) -> PendingPut {
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
            let app = api::repos::routes().with_state(settings);
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let response = put(
                &authed_client(),
                &format!("http://{addr}/api/repos/{id}"),
                &body,
            )
            .await;
            server.abort();
            response
        });
        // A deferred snapshot upgrade can return BUSY without a callback.
        // Close the sender in that case and report the actual HTTP response.
        drop(HANDOFF.take());
        reply
    });
    let waited = tokio::time::timeout(DEADLINE, waiting_rx)
        .await
        .expect("settings PUT stalled")
        .is_ok();
    reservation.rollback().unwrap();
    PendingPut {
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
    let pending = put_at_write(&state, &id, body.clone()).await;
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
    let (waited, response) = pending.finish().await;
    let held = state.db.get_repository(&id).unwrap().unwrap();
    let held_op = state.db.active_import_operation(&id).unwrap().unwrap();
    drop(gate);
    join_import(&state).await;
    assert_import_refusal(response);
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
    let pending = put_at_write(&state, &id, body.clone()).await;
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
    let (waited, (status, renamed)) = pending.finish().await;
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
