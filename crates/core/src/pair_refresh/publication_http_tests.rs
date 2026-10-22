use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[path = "../../tests/fixtures/owned_process_group.rs"]
mod fixture_process;
use fixture_process::OwnedProcessGroup;

use git2::{Oid, Repository, Signature};

use super::{
    observe_http_with_client, observe_pinned_child_with_policy, publish_http_with_client,
    publish_pinned_child_with_policy, GitPublicationObservation, PinnedPushPolicy,
};

struct Server(OwnedProcessGroup);

struct HttpFixture {
    _server: Server,
    source: Repository,
    target: Repository,
    url: String,
    cert_der: Option<Vec<u8>>,
    initial: Oid,
    child: Oid,
    parent: Oid,
    prepared: Oid,
    temp: tempfile::TempDir,
}

fn commit(repo: &Repository, parents: &[Oid], message: &str) -> Oid {
    let tree = repo.treebuilder(None).unwrap().write().unwrap();
    let tree = repo.find_tree(tree).unwrap();
    let parents: Vec<_> = parents
        .iter()
        .map(|id| repo.find_commit(*id).unwrap())
        .collect();
    let parents: Vec<_> = parents.iter().collect();
    let signature = Signature::now("Pair HTTP fixture", "fixture@example.invalid").unwrap();
    repo.commit(None, &signature, &signature, message, &tree, &parents)
        .unwrap()
}

fn run(binary: &str, args: &[&str]) {
    let output = Command::new(binary).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "{binary}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn server(
    root: &Path,
    cert: Option<(&Path, &Path)>,
    token: Option<&str>,
    redirect: Option<(&str, bool)>,
    log: &Path,
) -> (Server, String) {
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pair_refresh_http_server.py");
    let mut command = Command::new("python3");
    command.arg(script).arg(root).arg("--request-log").arg(log);
    if let Some((cert, key)) = cert {
        command.arg("--cert").arg(cert).arg("--key").arg(key);
    }
    if let Some(token) = token {
        command.arg("--token").arg(token);
    }
    if let Some((url, after_push)) = redirect {
        command.arg("--redirect").arg(url);
        if after_push {
            command.arg("--redirect-after-push");
        }
    }
    let mut server = Server(
        OwnedProcessGroup::spawn(command.stdout(Stdio::piped()).stderr(Stdio::inherit())).unwrap(),
    );
    let mut line = String::new();
    BufReader::new(server.0.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let ready: serde_json::Value = serde_json::from_str(&line).unwrap();
    (server, ready["url"].as_str().unwrap().to_owned())
}

impl HttpFixture {
    fn new(tls: bool, token: Option<&str>) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let source = Repository::init_bare(temp.path().join("source.git")).unwrap();
        let target = Repository::init_bare(temp.path().join("target.git")).unwrap();
        target
            .config()
            .unwrap()
            .set_bool("http.receivepack", true)
            .unwrap();
        let initial = commit(&source, &[], "initial");
        let child = commit(&source, &[initial], "child");
        let parent = commit(&source, &[initial], "parent");
        let prepared = commit(&source, &[child, parent], "prepared reconciliation");
        source
            .remote_anonymous(target.path().to_str().unwrap())
            .unwrap()
            .push(&[&format!("{prepared}:refs/heads/child")], None)
            .unwrap();
        target
            .reference("refs/heads/child", child, true, "fixture pinned input")
            .unwrap();
        let cert = temp.path().join("cert.pem");
        let key = temp.path().join("key.pem");
        let mut cert_der = None;
        if tls {
            let config = temp.path().join("cert.cnf");
            std::fs::write(&config, "[req]\ndistinguished_name=dn\nx509_extensions=ext\nprompt=no\n[dn]\nCN=localhost\n[ext]\nsubjectAltName=DNS:localhost,IP:127.0.0.1\nkeyUsage=digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\nbasicConstraints=CA:false\n").unwrap();
            run(
                "openssl",
                &[
                    "req",
                    "-x509",
                    "-newkey",
                    "rsa:2048",
                    "-days",
                    "1",
                    "-nodes",
                    "-keyout",
                    key.to_str().unwrap(),
                    "-out",
                    cert.to_str().unwrap(),
                    "-config",
                    config.to_str().unwrap(),
                ],
            );
            std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
            let der = temp.path().join("cert.der");
            run(
                "openssl",
                &[
                    "x509",
                    "-in",
                    cert.to_str().unwrap(),
                    "-outform",
                    "DER",
                    "-out",
                    der.to_str().unwrap(),
                ],
            );
            cert_der = Some(std::fs::read(der).unwrap());
        }
        let (_server, url) = server(
            temp.path(),
            tls.then_some((cert.as_path(), key.as_path())),
            token,
            None,
            &temp.path().join("requests.jsonl"),
        );
        Self {
            _server,
            source,
            target,
            url,
            cert_der,
            initial,
            child,
            parent,
            prepared,
            temp,
        }
    }

    fn publish(&self, token: Option<&str>) -> Result<Oid, crate::errors::GitError> {
        self.publish_to(&self.url, token)
    }

    fn publish_to(&self, url: &str, token: Option<&str>) -> Result<Oid, crate::errors::GitError> {
        // Only this test module trusts the exact ephemeral fixture certificate.
        // The production wrapper retains ordinary CA and hostname verification.
        let mut builder = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(5));
        if let Some(der) = &self.cert_der {
            let cert = reqwest::Certificate::from_der(der)
                .map_err(|_| git2::Error::from_str("invalid fixture certificate pin"))?;
            builder = builder.add_root_certificate(cert);
        }
        let client = builder
            .build()
            .map_err(|_| git2::Error::from_str("invalid fixture TLS client"))?;
        if url.starts_with("http://") {
            // Plain HTTP is a protocol fixture only. Production policy admits
            // HTTPS; this test-only path permits strictly loopback listeners.
            let parsed = super::canonical_url(url)?;
            if !matches!(parsed.host_str(), Some("localhost" | "127.0.0.1")) {
                return Err(git2::Error::from_str("non-loopback fixture endpoint").into());
            }
            super::checked_anonymous_remote(&self.source, url)?;
            return super::publish_via_http_transport(
                &self.source,
                url,
                "child",
                self.child,
                self.prepared,
                token,
                client,
            );
        }
        publish_http_with_client(
            &self.source,
            url,
            "child",
            self.child,
            self.prepared,
            token,
            client,
        )
    }

    fn tip(&self) -> Oid {
        self.target.refname_to_id("refs/heads/child").unwrap()
    }

    fn backend_stderr(&self) -> String {
        match std::fs::read(self.temp.path().join("http-backend-stderr.txt")) {
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

fn exact_tip(tls: bool) {
    let fixture = HttpFixture::new(tls, None);
    assert_eq!(fixture.publish(None).unwrap(), fixture.prepared);
    assert_eq!(fixture.tip(), fixture.prepared);
    let commit = fixture.target.find_commit(fixture.prepared).unwrap();
    assert_eq!(commit.parent_id(0).unwrap(), fixture.child);
    assert_eq!(commit.parent_id(1).unwrap(), fixture.parent);
}

fn ancestor_drift(tls: bool) {
    let fixture = HttpFixture::new(tls, None);
    fixture
        .target
        .reference(
            "refs/heads/child",
            fixture.initial,
            true,
            "external movement",
        )
        .unwrap();
    assert!(fixture.publish(None).is_err());
    assert_eq!(fixture.tip(), fixture.initial);
}

fn second_parent_drift(tls: bool) {
    let fixture = HttpFixture::new(tls, None);
    fixture
        .target
        .reference(
            "refs/heads/child",
            fixture.parent,
            true,
            "external movement",
        )
        .unwrap();
    assert!(fixture.publish(None).is_err());
    assert_eq!(fixture.tip(), fixture.parent);
}

fn advertisement_race(tls: bool) {
    let fixture = HttpFixture::new(tls, None);
    fixture.hook(&format!(
        "#!/bin/sh\nenv -u GIT_QUARANTINE_PATH git update-ref refs/heads/child {}\n",
        fixture.initial
    ));
    let before = fixture.tip();
    let published = fixture.publish(None);
    let after = fixture.tip();
    assert!(
        published.is_err(),
        "publish={published:?} before={before} after={after} {} stderr={}",
        exec_witness("hook", &fixture.target.path().join("hooks/pre-receive")),
        fixture.backend_stderr()
    );
    assert_eq!(fixture.tip(), fixture.initial);
}

#[test]
fn http_exact_tip() {
    exact_tip(false);
}
#[test]
fn http_ancestor_drift() {
    ancestor_drift(false);
}
#[test]
fn http_second_parent_drift() {
    second_parent_drift(false);
}
#[test]
fn http_advertisement_race() {
    advertisement_race(false);
}
#[test]
fn https_exact_tip() {
    exact_tip(true);
}
#[test]
fn https_ancestor_drift() {
    ancestor_drift(true);
}
#[test]
fn https_second_parent_drift() {
    second_parent_drift(true);
}
#[test]
fn https_advertisement_race() {
    advertisement_race(true);
}

#[test]
fn production_tls_rejects_untrusted_fixture_certificate() {
    let fixture = HttpFixture::new(true, None);
    assert!(publish_pinned_child_with_policy(
        &fixture.source,
        &fixture.url,
        "child",
        fixture.child,
        fixture.prepared,
        None,
        PinnedPushPolicy::SmartHttpV1
    )
    .is_err());
    assert_eq!(fixture.tip(), fixture.child);
}

#[test]
fn fixture_trust_requires_exact_certificate_pin() {
    let mut fixture = HttpFixture::new(true, None);
    fixture.cert_der.as_mut().unwrap()[0] ^= 1;
    assert!(fixture.publish(None).is_err());
    assert_eq!(fixture.tip(), fixture.child);
}

#[test]
fn https_token_uses_pinned_client_without_persisting_credentials() {
    let fixture = HttpFixture::new(true, Some("synthetic-fixture-token"));
    assert_eq!(
        fixture.publish(Some("synthetic-fixture-token")).unwrap(),
        fixture.prepared
    );
    assert!(
        !std::fs::read_to_string(fixture.source.path().join("config"))
            .unwrap()
            .contains("synthetic-fixture-token")
    );
}

#[test]
fn http_missing_credential_refuses_without_publication() {
    let fixture = HttpFixture::new(false, Some("synthetic-fixture-token"));
    assert!(fixture.publish(None).is_err());
    assert_eq!(fixture.tip(), fixture.child);
}

fn redirect_control(after_push: bool) {
    let fixture = HttpFixture::new(true, None);
    let cert = fixture.temp.path().join("cert.pem");
    let key = fixture.temp.path().join("key.pem");
    let redirected_log: PathBuf = fixture.temp.path().join("redirected.jsonl");
    let (_redirected, redirected_url) = server(
        fixture.temp.path(),
        Some((&cert, &key)),
        None,
        None,
        &redirected_log,
    );
    let destination = redirected_url.strip_suffix("/target.git").unwrap();
    let (_origin, origin_url) = server(
        fixture.temp.path(),
        Some((&cert, &key)),
        None,
        Some((destination, after_push)),
        &fixture.temp.path().join("origin.jsonl"),
    );
    assert!(fixture.publish_to(&origin_url, None).is_err());
    assert_eq!(
        fixture.tip(),
        if after_push {
            fixture.prepared
        } else {
            fixture.child
        }
    );
    assert!(
        !redirected_log.exists(),
        "must not contact redirected destination"
    );
}

#[test]
fn initial_redirect_refuses_before_publication() {
    redirect_control(false);
}
#[test]
fn verification_redirect_reports_uncertain_published_outcome() {
    redirect_control(true);
}

#[test]
fn git_to_http_rewrite_does_not_contact_server_or_present_token() {
    let fixture = HttpFixture::new(false, Some("synthetic-fixture-token"));
    let approved = "git://127.0.0.1:1/target.git";
    fixture
        .source
        .config()
        .unwrap()
        .set_str(&format!("url.{}.insteadOf", fixture.url), approved)
        .unwrap();
    assert!(publish_pinned_child_with_policy(
        &fixture.source,
        approved,
        "child",
        fixture.child,
        fixture.prepared,
        Some("synthetic-fixture-token"),
        PinnedPushPolicy::GitProtocolV1,
    )
    .is_err());
    assert_eq!(fixture.tip(), fixture.child);
    assert!(!fixture.temp.path().join("requests.jsonl").exists());
}

#[test]
fn http_endpoint_rewrite_refuses_before_contacting_either_server() {
    let fixture = HttpFixture::new(false, Some("synthetic-fixture-token"));
    let rewritten_log = fixture.temp.path().join("rewritten.jsonl");
    let (_rewritten, rewritten_url) = server(
        fixture.temp.path(),
        None,
        Some("synthetic-fixture-token"),
        None,
        &rewritten_log,
    );
    fixture
        .source
        .config()
        .unwrap()
        .set_str(&format!("url.{rewritten_url}.pushInsteadOf"), &fixture.url)
        .unwrap();
    assert!(fixture.publish(Some("synthetic-fixture-token")).is_err());
    assert_eq!(fixture.tip(), fixture.child);
    assert!(!fixture.temp.path().join("requests.jsonl").exists());
    assert!(!rewritten_log.exists());
}

#[test]
fn noncanonical_url_cannot_publish_another_repository_or_present_token() {
    let fixture = HttpFixture::new(true, Some("synthetic-fixture-token"));
    let other = Repository::init_bare(fixture.temp.path().join("other.git")).unwrap();
    fixture
        .source
        .remote_anonymous(other.path().to_str().unwrap())
        .unwrap()
        .push(&[&format!("{}:refs/heads/child", fixture.prepared)], None)
        .unwrap();
    other
        .reference(
            "refs/heads/child",
            fixture.child,
            true,
            "fixture original tip",
        )
        .unwrap();
    for suffix in [
        "/%2e%2e/other.git",
        "/../other.git",
        "/./other.git",
        "//other.git",
        "%2F..%2Fother.git",
        "%2F%2E%2E%2Fother.git",
        "%2f..%2fother.git",
        "%5C..%5Cother.git",
        "%5c..%5cother.git",
        "%252F..%252Fother.git",
    ] {
        let error = fixture
            .publish_to(
                &format!("{}{suffix}", fixture.url),
                Some("synthetic-fixture-token"),
            )
            .unwrap_err();
        assert!(error.to_string().contains("noncanonical"));
        assert_eq!(fixture.tip(), fixture.child);
        assert_eq!(
            other.refname_to_id("refs/heads/child").unwrap(),
            fixture.child
        );
        assert!(!fixture.temp.path().join("requests.jsonl").exists());
    }
}

#[test]
fn production_cleartext_http_refuses_before_contact_or_token_disclosure() {
    let fixture = HttpFixture::new(false, Some("synthetic-fixture-token"));
    assert!(publish_pinned_child_with_policy(
        &fixture.source,
        &fixture.url,
        "child",
        fixture.child,
        fixture.prepared,
        Some("synthetic-fixture-token"),
        PinnedPushPolicy::SmartHttpV1
    )
    .is_err());
    assert_eq!(fixture.tip(), fixture.child);
    assert!(!fixture.temp.path().join("requests.jsonl").exists());
}

#[test]
fn proxy_observation_child() {
    let Ok(mode) = std::env::var("REPOSYNC_PAIR_REFRESH_PROXY_CHILD") else {
        return;
    };
    let url = std::env::var("REPOSYNC_PAIR_REFRESH_PROXY_URL").unwrap();
    match mode.as_str() {
        "observe-default" => {
            let runtime = tokio::runtime::Runtime::new().unwrap();
            runtime.block_on(async {
                let response = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(5))
                    .build()
                    .unwrap()
                    .get(&url)
                    .send()
                    .await;
                assert!(response.is_err());
            });
        }
        "publish" => {
            let source =
                Repository::open(std::env::var("REPOSYNC_PAIR_REFRESH_PROXY_REPO").unwrap())
                    .unwrap();
            let old =
                Oid::from_str(&std::env::var("REPOSYNC_PAIR_REFRESH_PROXY_OLD").unwrap()).unwrap();
            let new =
                Oid::from_str(&std::env::var("REPOSYNC_PAIR_REFRESH_PROXY_NEW").unwrap()).unwrap();
            // The production client must contact the pinned HTTPS endpoint
            // directly and refuse this fixture's untrusted certificate.
            assert!(publish_pinned_child_with_policy(
                &source,
                &url,
                "child",
                old,
                new,
                None,
                PinnedPushPolicy::SmartHttpV1
            )
            .is_err());
        }
        _ => panic!("unknown proxy child mode"),
    }
    println!("pair-refresh proxy child exercised mode={mode}");
}

#[test]
fn production_https_ignores_inherited_proxy_with_observed_positive_control() {
    let fixture = HttpFixture::new(true, None);
    let proxy_log = fixture.temp.path().join("proxy.jsonl");
    let (_proxy, proxy_url) = server(fixture.temp.path(), None, None, None, &proxy_log);
    let proxy = proxy_url.strip_suffix("/target.git").unwrap();
    for mode in ["observe-default", "publish"] {
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "pair_refresh::publication::http_tests::proxy_observation_child",
                "--nocapture",
            ])
            .env("REPOSYNC_PAIR_REFRESH_PROXY_CHILD", mode)
            .env("REPOSYNC_PAIR_REFRESH_PROXY_URL", &fixture.url)
            .env("REPOSYNC_PAIR_REFRESH_PROXY_REPO", fixture.source.path())
            .env("REPOSYNC_PAIR_REFRESH_PROXY_OLD", fixture.child.to_string())
            .env(
                "REPOSYNC_PAIR_REFRESH_PROXY_NEW",
                fixture.prepared.to_string(),
            )
            .env("HTTP_PROXY", proxy)
            .env("http_proxy", proxy)
            .env("HTTPS_PROXY", proxy)
            .env("https_proxy", proxy)
            .env("ALL_PROXY", proxy)
            .env("all_proxy", proxy)
            .env_remove("NO_PROXY")
            .env_remove("no_proxy")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout)
            .contains(&format!("pair-refresh proxy child exercised mode={mode}")));
        if mode == "observe-default" {
            assert!(std::fs::read_to_string(&proxy_log)
                .unwrap()
                .contains("CONNECT"));
            std::fs::remove_file(&proxy_log).unwrap();
        } else {
            assert!(
                !proxy_log.exists(),
                "production must not contact inherited proxy"
            );
        }
        assert_eq!(fixture.tip(), fixture.child);
    }
}

impl HttpFixture {
    fn observe_to(
        &self,
        url: &str,
        token: Option<&str>,
    ) -> Result<GitPublicationObservation, crate::errors::GitError> {
        let mut builder = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(5));
        if let Some(der) = &self.cert_der {
            builder = builder.add_root_certificate(reqwest::Certificate::from_der(der).unwrap());
        }
        observe_http_with_client(
            &self.source,
            url,
            "child",
            self.child,
            self.prepared,
            token,
            builder.build().unwrap(),
        )
    }
}

#[test]
fn https_recovery_observes_old_and_published_tips_without_receive_pack_or_token_persistence() {
    let fixture = HttpFixture::new(true, Some("synthetic-fixture-token"));
    let token = Some("synthetic-fixture-token");
    assert_eq!(
        fixture.observe_to(&fixture.url, token).unwrap(),
        GitPublicationObservation::NotObserved
    );
    assert_eq!(fixture.publish(token).unwrap(), fixture.prepared);
    let witness = fixture.temp.path().join("receive-pack-witness");
    fixture.hook(&format!(
        "#!/bin/sh\nprintf 'retry\\n' > '{}'\nexit 1\n",
        witness.display()
    ));
    for _ in 0..2 {
        assert_eq!(
            fixture.observe_to(&fixture.url, token).unwrap(),
            GitPublicationObservation::Observed(fixture.prepared)
        );
        assert!(!witness.exists());
        assert_eq!(fixture.tip(), fixture.prepared);
    }
    assert!(
        !std::fs::read_to_string(fixture.source.path().join("config"))
            .unwrap()
            .contains("synthetic-fixture-token")
    );
}

#[test]
fn https_recovery_redirect_refuses_without_contacting_destination() {
    let fixture = HttpFixture::new(true, Some("synthetic-fixture-token"));
    let cert = fixture.temp.path().join("cert.pem");
    let key = fixture.temp.path().join("key.pem");
    let destination_log = fixture.temp.path().join("recovery-redirected.jsonl");
    let (_destination, destination) = server(
        fixture.temp.path(),
        Some((&cert, &key)),
        Some("synthetic-fixture-token"),
        None,
        &destination_log,
    );
    let (_origin, origin) = server(
        fixture.temp.path(),
        Some((&cert, &key)),
        Some("synthetic-fixture-token"),
        Some((destination.strip_suffix("/target.git").unwrap(), false)),
        &fixture.temp.path().join("recovery-origin.jsonl"),
    );
    assert!(fixture
        .observe_to(&origin, Some("synthetic-fixture-token"))
        .is_err());
    assert!(!destination_log.exists());
    assert_eq!(fixture.tip(), fixture.child);
}

#[test]
fn production_recovery_refuses_cleartext_and_untrusted_tls_without_token_contact() {
    for tls in [false, true] {
        let fixture = HttpFixture::new(tls, Some("synthetic-fixture-token"));
        assert!(observe_pinned_child_with_policy(
            &fixture.source,
            &fixture.url,
            "child",
            fixture.child,
            fixture.prepared,
            Some("synthetic-fixture-token"),
            PinnedPushPolicy::SmartHttpV1
        )
        .is_err());
        assert!(!fixture.temp.path().join("requests.jsonl").exists());
        assert_eq!(fixture.tip(), fixture.child);
    }
}

#[test]
fn recovery_refuses_aliases_and_rewrites_before_contact_or_token() {
    let fixture = HttpFixture::new(true, Some("synthetic-fixture-token"));
    for suffix in [
        "%2F..%2Fother.git",
        "%5C..%5Cother.git",
        "%252F..%252Fother.git",
    ] {
        let error = fixture
            .observe_to(
                &format!("{}{suffix}", fixture.url),
                Some("synthetic-fixture-token"),
            )
            .unwrap_err();
        assert!(error.to_string().contains("noncanonical"), "{error}");
        assert!(!fixture.temp.path().join("requests.jsonl").exists());
        assert_eq!(fixture.tip(), fixture.child);
    }
    fixture
        .source
        .config()
        .unwrap()
        .set_str("url.https://127.0.0.1:1/other.git.insteadOf", &fixture.url)
        .unwrap();
    assert!(fixture
        .observe_to(&fixture.url, Some("synthetic-fixture-token"))
        .is_err());
    assert!(!fixture.temp.path().join("requests.jsonl").exists());
    assert_eq!(fixture.tip(), fixture.child);
}

fn exec_witness(label: &str, path: &Path) -> String {
    let mode = std::fs::metadata(path)
        .map(|meta| format!("{:o}", meta.permissions().mode()))
        .unwrap_or_else(|error| format!("metadata {error}"));
    let test_x = Command::new("test")
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
