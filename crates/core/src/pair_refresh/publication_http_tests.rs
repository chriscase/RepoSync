use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use git2::{Oid, Repository, Signature};

use super::{publish_http_with_client, publish_pinned_child_with_policy, PinnedPushPolicy};

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

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
        command
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
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
        // Only this test module trusts the exact ephemeral fixture certificate.
        // The production wrapper retains ordinary CA and hostname verification.
        let mut builder = reqwest::Client::builder()
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
        publish_http_with_client(
            &self.source,
            &self.url,
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
    assert!(fixture.publish(None).is_err());
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
fn http_token_uses_pinned_client_without_persisting_credentials() {
    let fixture = HttpFixture::new(false, Some("synthetic-fixture-token"));
    assert_eq!(
        publish_pinned_child_with_policy(
            &fixture.source,
            &fixture.url,
            "child",
            fixture.child,
            fixture.prepared,
            Some("synthetic-fixture-token"),
            PinnedPushPolicy::SmartHttpV1
        )
        .unwrap(),
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
    let fixture = HttpFixture::new(false, None);
    let redirected_log: PathBuf = fixture.temp.path().join("redirected.jsonl");
    let (_redirected, redirected_url) =
        server(fixture.temp.path(), None, None, None, &redirected_log);
    let destination = redirected_url.strip_suffix("/target.git").unwrap();
    let (_origin, origin_url) = server(
        fixture.temp.path(),
        None,
        None,
        Some((destination, after_push)),
        &fixture.temp.path().join("origin.jsonl"),
    );
    assert!(publish_pinned_child_with_policy(
        &fixture.source,
        &origin_url,
        "child",
        fixture.child,
        fixture.prepared,
        None,
        PinnedPushPolicy::SmartHttpV1
    )
    .is_err());
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
