//! Operation-local HTTP bridge for libgit2. Its redirect setting permits
//! same-host redirects, so requests pass through a client that rejects every
//! redirect and forwards only the pinned Git service endpoints.

use std::future::IntoFuture;
use std::sync::Arc;
use std::thread::JoinHandle;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, Method, Response, StatusCode};
use axum::routing::any;
use axum::Router;
use tokio::sync::oneshot;

use crate::errors::GitError;

#[derive(Clone)]
struct PinnedHttp {
    endpoint: reqwest::Url,
    client: reqwest::Client,
    token: Option<String>,
    local_prefix: String,
}

pub(super) struct HttpTransport {
    pub endpoint: String,
    stop: Option<oneshot::Sender<()>>,
    worker: Option<JoinHandle<()>>,
}

fn transport_error() -> GitError {
    git2::Error::from_str("could not prepare pinned HTTP publication transport").into()
}

impl HttpTransport {
    pub fn new(
        remote_url: &str,
        token: Option<&str>,
        client: reqwest::Client,
    ) -> Result<Self, GitError> {
        let endpoint = reqwest::Url::parse(remote_url).map_err(|_| transport_error())?;
        if endpoint.as_str() != remote_url {
            return Err(transport_error());
        }
        let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(|_| transport_error())?;
        listener
            .set_nonblocking(true)
            .map_err(|_| transport_error())?;
        let address = listener.local_addr().map_err(|_| transport_error())?;
        let local_prefix = format!("/{}/repo.git", uuid::Uuid::new_v4());
        let local_endpoint = format!("http://{address}{local_prefix}");
        let state = Arc::new(PinnedHttp {
            endpoint,
            client,
            token: token.map(str::to_owned),
            local_prefix,
        });
        let (stop, stopped) = oneshot::channel();
        let (ready, initialized) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::Builder::new()
            .name("pair-refresh-http".into())
            .spawn(move || {
                let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                else {
                    let _ = ready.send(false);
                    return;
                };
                runtime.block_on(async move {
                    let Ok(listener) = tokio::net::TcpListener::from_std(listener) else {
                        let _ = ready.send(false);
                        return;
                    };
                    let app = Router::new()
                        .route("/*path", any(forward))
                        .with_state(state);
                    let serving = axum::serve(listener, app).into_future();
                    let _ = ready.send(true);
                    // Stop promptly even if an upstream request has an uncertain
                    // outcome. The journal owner retains the operation hold.
                    tokio::select! {
                        _ = serving => {},
                        _ = stopped => {},
                    }
                });
            })
            .map_err(|_| transport_error())?;
        let transport = Self {
            endpoint: local_endpoint,
            stop: Some(stop),
            worker: Some(worker),
        };
        if initialized.recv().ok() != Some(true) {
            return Err(transport_error());
        }
        Ok(transport)
    }
}

impl Drop for HttpTransport {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn refusal(status: StatusCode) -> Response<Body> {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = status;
    response
}

async fn forward(State(state): State<Arc<PinnedHttp>>, request: Request) -> Response<Body> {
    let Some(service) = request.uri().path().strip_prefix(&state.local_prefix) else {
        return refusal(StatusCode::NOT_FOUND);
    };
    let (operation, advertisement) = match (request.method(), service, request.uri().query()) {
        (&Method::GET, "/info/refs", Some("service=git-receive-pack")) => {
            ("git-receive-pack", true)
        }
        (&Method::GET, "/info/refs", Some("service=git-upload-pack")) => ("git-upload-pack", true),
        (&Method::POST, "/git-receive-pack", None) => ("git-receive-pack", false),
        (&Method::POST, "/git-upload-pack", None) => ("git-upload-pack", false),
        _ => return refusal(StatusCode::BAD_REQUEST),
    };
    let mut endpoint = state.endpoint.clone();
    endpoint.set_path(&format!(
        "{}{}",
        state.endpoint.path().trim_end_matches('/'),
        service
    ));
    endpoint.set_query(advertisement.then_some(&format!("service={operation}")));
    let (parts, body) = request.into_parts();
    let mut outgoing = state.client.request(parts.method, endpoint);
    if let Some(token) = &state.token {
        outgoing = outgoing.basic_auth("x-access-token", Some(token));
    }
    if !advertisement {
        outgoing = outgoing.header(
            header::CONTENT_TYPE,
            format!("application/x-{operation}-request"),
        );
        if let Some(length) = parts.headers.get(header::CONTENT_LENGTH) {
            outgoing = outgoing.header(header::CONTENT_LENGTH, length);
        }
        outgoing = outgoing.body(reqwest::Body::wrap_stream(body.into_data_stream()));
    }
    let Ok(upstream) = outgoing.send().await else {
        return refusal(StatusCode::BAD_GATEWAY);
    };
    // Never forward a Location header or perform a redirect, including a
    // same-host path/port redirect accepted by libgit2's native HTTP transport.
    if upstream.status().is_redirection() {
        return refusal(StatusCode::BAD_GATEWAY);
    }
    if !upstream.status().is_success() {
        return refusal(upstream.status());
    }
    let expected_type = format!(
        "application/x-{operation}-{}",
        if advertisement {
            "advertisement"
        } else {
            "result"
        }
    );
    if upstream
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_none_or(|value| value.split(';').next() != Some(expected_type.as_str()))
    {
        return refusal(StatusCode::BAD_GATEWAY);
    }
    Response::builder()
        .status(upstream.status())
        .header(header::CONTENT_TYPE, expected_type)
        .body(Body::from_stream(upstream.bytes_stream()))
        .unwrap_or_else(|_| refusal(StatusCode::BAD_GATEWAY))
}
