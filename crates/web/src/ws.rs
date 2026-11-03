//! WebSocket endpoint for live status updates.
//!
//! Clients connect to `/ws` and receive JSON messages whenever the sync state
//! changes, a new conflict is detected, or a webhook is received.
//!
//! Authentication is performed via the first message: the client must send
//! `{"type":"auth","token":"<session_token>"}` within 5 seconds of connecting.

use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket};
use axum::extract::{State, WebSocketUpgrade};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use chrono::Utc;
use tokio::sync::broadcast;
use tracing::{debug, warn};

use crate::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/ws", get(ws_handler))
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    // Accept the upgrade unconditionally; auth happens via first message.
    let rx = state.ws_broadcast.subscribe();
    ws.on_upgrade(move |socket| handle_socket(socket, rx, state))
}

/// Validate a session token against both DB and in-memory session stores.
async fn validate_ws_token(state: &Arc<AppState>, token: &str) -> bool {
    // Check DB sessions first
    if let Ok(Some(_)) = state.db.get_session(token) {
        return true;
    }
    // Fallback to in-memory sessions
    let now = Utc::now();
    let sessions = state.sessions.read().await;
    sessions
        .get(token)
        .is_some_and(|expires_at| *expires_at > now)
}

async fn handle_socket(
    mut socket: WebSocket,
    mut rx: broadcast::Receiver<String>,
    state: Arc<AppState>,
) {
    debug!("WebSocket client connected, awaiting auth");

    // Check if auth is required (admin password or users exist)
    let needs_auth = state.config.web.admin_password.is_some()
        || state.db.count_users().unwrap_or(0) > 0;

    if needs_auth {
        // Wait up to 5 seconds for the first message to contain auth token
        let auth_timeout = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            socket.recv(),
        )
        .await;

        let authenticated = match auth_timeout {
            Ok(Some(Ok(Message::Text(text)))) => {
                // Parse {"type":"auth","token":"..."}
                if let Ok(msg) = serde_json::from_str::<serde_json::Value>(&text) {
                    if msg.get("type").and_then(|t| t.as_str()) == Some("auth") {
                        if let Some(token) = msg.get("token").and_then(|t| t.as_str()) {
                            validate_ws_token(&state, token).await
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                } else {
                    false
                }
            }
            _ => false,
        };

        if !authenticated {
            let err = serde_json::json!({
                "type": "error",
                "message": "authentication failed",
            });
            let _ = socket.send(Message::Text(err.to_string())).await;
            let _ = socket.send(Message::Close(None)).await;
            debug!("WebSocket auth failed, closing connection");
            return;
        }
    }

    // Send a welcome message
    let welcome = serde_json::json!({
        "type": "connected",
        "message": "RepoSync live updates",
    });
    if let Err(e) = socket.send(Message::Text(welcome.to_string())).await {
        warn!("failed to send welcome message: {}", e);
        return;
    }

    debug!("WebSocket client authenticated and connected");

    // Forward broadcast messages to the WebSocket client.
    loop {
        tokio::select! {
            result = rx.recv() => {
                match result {
                    Ok(msg) => {
                        if let Err(e) = socket.send(Message::Text(msg)).await {
                            debug!("WebSocket send error (client disconnected?): {}", e);
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!("WebSocket client lagged by {} messages", n);
                        let lag_msg = serde_json::json!({
                            "type": "warning",
                            "message": format!("lagged by {} messages", n),
                        });
                        let _ = socket.send(Message::Text(lag_msg.to_string())).await;
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        debug!("broadcast channel closed, disconnecting WebSocket");
                        break;
                    }
                }
            }
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None => {
                        debug!("WebSocket client disconnected");
                        break;
                    }
                    Some(Ok(Message::Ping(data))) => {
                        if let Err(e) = socket.send(Message::Pong(data)).await {
                            debug!("WebSocket pong error: {}", e);
                            break;
                        }
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => {
                        debug!("WebSocket receive error: {}", e);
                        break;
                    }
                }
            }
        }
    }

    debug!("WebSocket connection closed");
}
