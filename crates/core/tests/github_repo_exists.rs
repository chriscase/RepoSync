//! Repository existence must distinguish absence from unsupported HEAD and API failures.
//! All HTTP requests stay on disposable loopback listeners with fixture credentials.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::Response;
use axum::Router;
use reposync_core::config::GitProvider;
use reposync_core::errors::GitHubError;
use reposync_core::git::github::GitHubClient;

#[derive(Debug, PartialEq)]
struct Request {
    method: Method,
    path: String,
    authorization: String,
}

struct Reply {
    status: u16,
    location: Option<&'static str>,
}

impl From<u16> for Reply {
    fn from(status: u16) -> Self {
        Self {
            status,
            location: None,
        }
    }
}

#[derive(Default)]
struct Script {
    replies: VecDeque<Reply>,
    requests: Vec<Request>,
}

struct Api {
    url: String,
    script: Arc<Mutex<Script>>,
    task: tokio::task::JoinHandle<()>,
}

impl Api {
    async fn start(replies: impl IntoIterator<Item = Reply>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let script = Arc::new(Mutex::new(Script {
            replies: replies.into_iter().collect(),
            requests: Vec::new(),
        }));
        let app = Router::new().fallback(respond).with_state(script.clone());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self { url, script, task }
    }

    fn assert_requests(&self, provider: &GitProvider, expected: &[(&str, &str)]) {
        let authorization = match provider {
            GitProvider::GitHub => "Bearer fixture-token",
            GitProvider::Gitea => "token fixture-token",
        };
        let script = self.script.lock().unwrap();
        let expected: Vec<_> = expected
            .iter()
            .map(|(method, path)| Request {
                method: method.parse().unwrap(),
                path: (*path).into(),
                authorization: authorization.into(),
            })
            .collect();
        assert_eq!(script.requests, expected);
        assert!(
            script.replies.is_empty(),
            "scripted response was not consumed"
        );
    }

    fn client(&self, provider: &GitProvider) -> GitHubClient {
        GitHubClient::new(&self.url, "fixture-token", provider.clone())
    }
}

impl Drop for Api {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn respond(
    State(script): State<Arc<Mutex<Script>>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    let mut script = script.lock().unwrap();
    script.requests.push(Request {
        method,
        path: uri.path().into(),
        authorization: headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("missing")
            .into(),
    });
    let reply = script.replies.pop_front().unwrap_or_else(|| 500.into());
    let mut response = Response::builder()
        .status(StatusCode::from_u16(reply.status).unwrap())
        .header("x-ratelimit-reset", "fixture-reset");
    if let Some(location) = reply.location {
        response = response.header("location", location);
    }
    response.body(Body::from("fixture response")).unwrap()
}

fn assert_api_error(status: u16, error: GitHubError) {
    match (status, error) {
        (401 | 403, GitHubError::AuthenticationFailed(message)) => {
            assert!(message.contains(&format!("HTTP {status}")));
        }
        (429, GitHubError::RateLimited { reset_at }) => assert_eq!(reset_at, "fixture-reset"),
        (expected, GitHubError::ApiError { status, .. }) => assert_eq!(status, expected),
        (status, error) => panic!("wrong error for HTTP {status}: {error:?}"),
    }
}

#[tokio::test]
async fn head_200_uses_one_authenticated_request() {
    for provider in [GitProvider::GitHub, GitProvider::Gitea] {
        let api = Api::start([200.into()]).await;
        assert!(api
            .client(&provider)
            .repo_exists("fixture/repo")
            .await
            .unwrap());
        api.assert_requests(&provider, &[("HEAD", "/repos/fixture/repo")]);
    }
}

#[tokio::test]
async fn head_405_get_200_recognizes_existing_repo_with_provider_auth() {
    for provider in [GitProvider::GitHub, GitProvider::Gitea] {
        let api = Api::start([405.into(), 200.into()]).await;
        assert!(api
            .client(&provider)
            .repo_exists("fixture/repo")
            .await
            .unwrap());
        api.assert_requests(
            &provider,
            &[
                ("HEAD", "/repos/fixture/repo"),
                ("GET", "/repos/fixture/repo"),
            ],
        );
    }
}

#[tokio::test]
async fn direct_and_fallback_404_are_the_only_missing_responses() {
    for provider in [GitProvider::GitHub, GitProvider::Gitea] {
        for fallback in [false, true] {
            let mut replies = Vec::new();
            let mut requests = vec![("HEAD", "/repos/fixture/repo")];
            if fallback {
                replies.push(405.into());
                requests.push(("GET", "/repos/fixture/repo"));
            }
            replies.push(404.into());
            let api = Api::start(replies).await;
            assert!(!api
                .client(&provider)
                .repo_exists("fixture/repo")
                .await
                .unwrap());
            api.assert_requests(&provider, &requests);
        }
    }
}

#[tokio::test]
async fn head_auth_rate_limit_server_and_410_errors_are_not_absence() {
    for provider in [GitProvider::GitHub, GitProvider::Gitea] {
        for status in [401, 403, 429, 500, 503, 410] {
            let api = Api::start([status.into()]).await;
            let error = api
                .client(&provider)
                .repo_exists("fixture/repo")
                .await
                .unwrap_err();
            assert_api_error(status, error);
            api.assert_requests(&provider, &[("HEAD", "/repos/fixture/repo")]);
        }
    }
}

#[tokio::test]
async fn fallback_get_auth_rate_limit_server_410_and_405_errors_are_not_absence() {
    for provider in [GitProvider::GitHub, GitProvider::Gitea] {
        for status in [401, 403, 429, 500, 503, 410, 405] {
            let api = Api::start([405.into(), status.into()]).await;
            let error = api
                .client(&provider)
                .repo_exists("fixture/repo")
                .await
                .unwrap_err();
            assert_api_error(status, error);
            api.assert_requests(
                &provider,
                &[
                    ("HEAD", "/repos/fixture/repo"),
                    ("GET", "/repos/fixture/repo"),
                ],
            );
        }
    }
}

#[tokio::test]
async fn same_origin_redirects_preserve_auth_and_use_the_final_status() {
    for provider in [GitProvider::GitHub, GitProvider::Gitea] {
        for fallback in [false, true] {
            for final_status in [200, 404, 401, 410] {
                let mut replies = Vec::new();
                let mut requests = vec![("HEAD", "/repos/fixture/repo")];
                let method = if fallback {
                    replies.push(405.into());
                    requests.push(("GET", "/repos/fixture/repo"));
                    "GET"
                } else {
                    "HEAD"
                };
                replies.push(Reply {
                    status: 307,
                    location: Some("/repos/fixture/moved"),
                });
                replies.push(final_status.into());
                requests.push((method, "/repos/fixture/moved"));
                let api = Api::start(replies).await;
                let result = api.client(&provider).repo_exists("fixture/repo").await;
                match final_status {
                    200 => assert!(result.unwrap()),
                    404 => assert!(!result.unwrap()),
                    status => assert_api_error(status, result.unwrap_err()),
                }
                api.assert_requests(&provider, &requests);
            }
        }
    }
}

#[tokio::test]
async fn unresolvable_redirects_are_errors_without_get_fallback() {
    for provider in [GitProvider::GitHub, GitProvider::Gitea] {
        let api = Api::start([302.into()]).await;
        assert_api_error(
            302,
            api.client(&provider)
                .repo_exists("fixture/repo")
                .await
                .unwrap_err(),
        );
        api.assert_requests(&provider, &[("HEAD", "/repos/fixture/repo")]);

        let api = Api::start((0..11).map(|_| Reply {
            status: 302,
            location: Some("/repos/fixture/repo"),
        }))
        .await;
        let error = api
            .client(&provider)
            .repo_exists("fixture/repo")
            .await
            .unwrap_err();
        assert!(matches!(error, GitHubError::HttpError(ref error) if error.is_redirect()));
        api.assert_requests(&provider, &[("HEAD", "/repos/fixture/repo"); 11]);
    }
}

#[tokio::test]
async fn head_and_fallback_transport_errors_are_not_absence() {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    for provider in [GitProvider::GitHub, GitProvider::Gitea] {
        for fallback in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let server = if fallback {
                Some(std::thread::spawn(move || {
                    let (mut stream, _) = listener.accept().unwrap();
                    drop(listener); // The authenticated GET must encounter a transport error.
                    stream
                        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                        .unwrap();
                    let mut request = Vec::new();
                    while !request.ends_with(b"\r\n\r\n") {
                        let mut byte = [0];
                        stream.read_exact(&mut byte).unwrap();
                        request.push(byte[0]);
                        assert!(request.len() < 16_384);
                    }
                    assert!(request.starts_with(b"HEAD /repos/fixture/repo "));
                    stream.write_all(b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
                }))
            } else {
                drop(listener);
                None
            };
            let result = GitHubClient::new(url, "fixture-token", provider.clone())
                .repo_exists("fixture/repo")
                .await;
            assert!(
                matches!(result, Err(GitHubError::HttpError(_))),
                "{result:?}"
            );
            if let Some(server) = server {
                server.join().unwrap();
            }
        }
    }
}
