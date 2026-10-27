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

// Case handles refer to process-wide storage; dropping a local observer never
// detaches the child, thread, or case directory that it was observing.
struct CaseHttpRoot { _root: std::sync::Arc<tempfile::TempDir> }

#[derive(Clone, Copy, PartialEq, Eq)]
enum HttpChildRole { Server, CertificateTool }

struct CaseHttpChild {
    context: crate::process::CaseContextId,
    role: HttpChildRole,
    child: std::process::Child,
    group: i32,
    _root: std::sync::Arc<tempfile::TempDir>,
    stopping: Option<std::time::Instant>,
    killed: bool,
    status: Option<std::process::ExitStatus>,
    error: Option<String>,
}

impl CaseHttpChild {
    fn signal(&self, signal: i32) -> Result<(), String> {
        if self.group <= 0 { return Err("HTTP child has no owned group".into()); }
        let result = unsafe { libc::kill(-self.group, signal) };
        if result == 0 { return Ok(()); }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) { Ok(()) }
        else { Err(format!("HTTP owned group signal failed: {error}")) }
    }

    fn request_stop(&mut self) -> Result<(), String> {
        if self.stopping.is_none() {
            // A server that failed before the requested stop is not converted
            // into successful cleanup by sending TERM to an absent leader.
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    self.status = Some(status);
                    if !status.success() {
                        self.error = Some(format!("HTTP child exited before stop: {status}"));
                    }
                }
                Ok(None) => {}
                Err(error) => self.error = Some(format!("HTTP pre-stop wait failed: {error}")),
            }
            self.stopping = Some(std::time::Instant::now());
            if let Err(error) = self.signal(libc::SIGTERM) {
                self.error = Some(error.clone());
                return Err(error);
            }
        }
        match &self.error { Some(error) => Err(error.clone()), None => Ok(()) }
    }

    fn observe_exit(&mut self) -> Result<Option<std::process::ExitStatus>, String> {
        if let Some(error) = &self.error { return Err(error.clone()); }
        if self.status.is_none() {
            match self.child.try_wait() {
                Ok(status) => self.status = status,
                Err(error) => {
                    let error = format!("HTTP child wait failed: {error}");
                    self.error = Some(error.clone());
                    return Err(error);
                }
            }
        }
        if self.status.is_none() { return Ok(None); }
        match crate::process::owned_group_quiescent(self.group) {
            Ok(true) => Ok(self.status),
            Ok(false) => Ok(None),
            Err(error) => {
                let error = format!("HTTP group observation failed: {error}");
                self.error = Some(error.clone());
                Err(error)
            }
        }
    }

    fn observe_stop(&mut self) -> Result<bool, String> {
        let started = self.stopping.ok_or_else(|| "HTTP stop was not requested".to_string())?;
        if !self.killed && started.elapsed() >= std::time::Duration::from_millis(500) {
            if let Err(error) = self.signal(libc::SIGKILL) {
                self.error = Some(error.clone());
                return Err(error);
            }
            self.killed = true;
        }
        Ok(self.observe_exit()?.is_some())
    }
}

struct CaseHttpThread<T> {
    context: crate::process::CaseContextId,
    handle: Option<std::thread::JoinHandle<T>>,
    ready: Option<T>,
    error: Option<String>,
}

impl<T> CaseHttpThread<T> {
    fn observe(&mut self) -> Result<bool, String> {
        if let Some(error) = &self.error { return Err(error.clone()); }
        if self.ready.is_some() { return Ok(true); }
        let handle = self.handle.as_ref()
            .ok_or_else(|| "HTTP thread handle is missing".to_string())?;
        if !handle.is_finished() { return Ok(false); }
        let handle = self.handle.take().ok_or_else(|| "HTTP thread was consumed".to_string())?;
        match handle.join() {
            Ok(value) => { self.ready = Some(value); Ok(true) }
            Err(_) => {
                let error = "HTTP thread panicked; consumed handle stays terminal".to_string();
                self.error = Some(error.clone());
                crate::process::retain_case_context(self.context);
                Err(error)
            }
        }
    }
}

struct CheckedHttpJoin { context: crate::process::CaseContextId }
struct CheckedHttpPublication { join: CheckedHttpJoin }
struct CheckedHttpBlocking { context: crate::process::CaseContextId }
struct CheckedHttpServer { context: crate::process::CaseContextId }
struct CheckedHttpChild { context: crate::process::CaseContextId, role: HttpChildRole }

fn spawn_http_work<T: Send + 'static>(
    context: crate::process::CaseContextId,
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<crate::process::CaseResourceHandle<CaseHttpThread<T>>, String> {
    crate::process::register_case_resource(context, || {
        let handle = std::thread::Builder::new().name("reposync-http-case".into())
            .spawn(work).map_err(|error| format!("HTTP thread spawn failed: {error}"))?;
        Ok(CaseHttpThread { context, handle: Some(handle), ready: None, error: None })
    })
}

fn join_http_work<T: Send + 'static>(
    handle: crate::process::CaseResourceHandle<CaseHttpThread<T>>,
    budget: std::time::Duration,
) -> Result<(T, CheckedHttpJoin), String> {
    let deadline = std::time::Instant::now() + budget;
    loop {
        let ready = crate::process::with_case_resource(handle, |slot| {
            if !slot.observe()? { return Ok(None); }
            if std::time::Instant::now() >= deadline {
                return Err("HTTP join budget expired; real result stays retained".into());
            }
            let value = slot.ready.take().ok_or_else(|| "HTTP joined result is missing".to_string())?;
            Ok::<_, String>(Some((value, slot.context)))
        })??;
        if let Some((value, context)) = ready {
            crate::process::finish_case_resource(handle)?;
            return Ok((value, CheckedHttpJoin { context }));
        }
        if std::time::Instant::now() >= deadline {
            return Err("HTTP thread observation timed out; handle and result remain owned".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn stop_http_child(
    handle: crate::process::CaseResourceHandle<CaseHttpChild>,
) -> Result<CheckedHttpChild, String> {
    stop_http_child_until(handle, std::time::Instant::now() + std::time::Duration::from_secs(5))
}

fn stop_http_child_until(
    handle: crate::process::CaseResourceHandle<CaseHttpChild>,
    deadline: std::time::Instant,
) -> Result<CheckedHttpChild, String> {
    crate::process::with_case_resource(handle, |child| child.request_stop())??;
    loop {
        if std::time::Instant::now() >= deadline {
            return Err("HTTP child cleanup budget expired; resource stays retained".into());
        }
        let (complete, context, role) = crate::process::with_case_resource(handle,
            |child| child.observe_stop().map(|complete| (complete, child.context, child.role)))??;
        if complete {
            if std::time::Instant::now() >= deadline {
                return Err("HTTP child observation exceeded its budget; resource stays retained".into());
            }
            crate::process::finish_case_resource(handle)?;
            return Ok(CheckedHttpChild { context, role });
        }
        if std::time::Instant::now() >= deadline {
            return Err("HTTP child cleanup is unconfirmed; child and root remain owned".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

type HttpPublicationResult = (HttpFixture, Result<Oid, crate::errors::GitError>);

fn observe_http_threads<T: Send + 'static>(
    context: crate::process::CaseContextId,
    deadline: std::time::Instant,
) -> Result<(), String> {
    for handle in crate::process::case_resource_handles::<CaseHttpThread<T>>(context) {
        loop {
            if std::time::Instant::now() >= deadline {
                return Err("HTTP retained thread is still unresolved".into());
            }
            if crate::process::with_case_resource(handle, |slot| slot.observe())?? {
                if std::time::Instant::now() >= deadline {
                    return Err("HTTP retained thread observation exceeded its budget".into());
                }
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        // Keep the real result and its context for the original qualification
        // owner. Physical observation cannot reconstruct skipped assertions.
    }
    Ok(())
}

fn run_case_tool(
    context: crate::process::CaseContextId,
    root: &std::sync::Arc<tempfile::TempDir>,
    label: &str,
    args: &[&str],
) -> Result<(), String> {
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::process::CommandExt;
    let output = std::fs::OpenOptions::new().write(true).create_new(true)
        .custom_flags(libc::O_NOFOLLOW).open(root.path().join(format!("{label}.output")))
        .map_err(|error| format!("HTTP tool log: {error}"))?;
    let mut command = Command::new("openssl");
    command.args(args).stdout(output.try_clone().map_err(|error| error.to_string())?)
        .stderr(output).process_group(0);
    for key in ["REPOSYNC_LAUNCH_BINDING_V1", "REPOSYNC_FIXTURE_EXEC_MODE",
        "REPOSYNC_EXEC_EXPECTED_ENTRY", "REPOSYNC_EXEC_EXPECTED_CASE",
        "REPOSYNC_EXEC_EXPECTED_INSTANCE", "REPOSYNC_EXEC_EXPECTED_ROOT_ID",
        "REPOSYNC_EXEC_EXPECTED_DATA_SHA256", "REPOSYNC_EXEC_EXPECTED_MANIFEST_SHA256"] {
        command.env_remove(key);
    }
    let retained_root = std::sync::Arc::clone(root);
    let handle = crate::process::register_case_resource(context, || {
        let child = command.spawn().map_err(|error| format!("HTTP tool spawn: {error}"))?;
        // An invalid group fails observation while retaining the actual child.
        let group = i32::try_from(child.id()).unwrap_or(0);
        Ok(CaseHttpChild { context, role: HttpChildRole::CertificateTool, child, group, _root: retained_root,
            stopping: None, killed: false, status: None, error: None })
    })?;
    observe_case_tool_until(handle, std::time::Instant::now() + std::time::Duration::from_secs(45))
}

// The real certificate path and fixed native probes share result/cleanup
// interpretation. Production still spawns only its fixed OpenSSL command.
fn observe_case_tool_until(
    handle: crate::process::CaseResourceHandle<CaseHttpChild>,
    deadline: std::time::Instant,
) -> Result<(), String> {
    loop {
        if std::time::Instant::now() >= deadline {
            let cleanup = match stop_http_child(handle) {
                Ok(_) => "confirmed stop, not command success".to_string(),
                Err(error) => format!("unconfirmed: {error}"),
            };
            return Err(format!("HTTP certificate tool timed out; cleanup {cleanup}"));
        }
        if let Some(status) = crate::process::with_case_resource(handle, |child| child.observe_exit())?? {
            if !status.success() {
                return Err(format!("HTTP certificate tool failed: {status}; owned evidence retained"));
            }
            if std::time::Instant::now() >= deadline {
                return Err("HTTP certificate observation exceeded its budget; evidence retained".into());
            }
            crate::process::finish_case_resource(handle)?;
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

enum Server {
    Native(OwnedProcessGroup),
    Case(crate::process::CaseResourceHandle<CaseHttpChild>),
}

struct HttpFixture {
    _server: Option<Server>,
    source: Repository,
    target: Repository,
    url: String,
    cert_der: Option<Vec<u8>>,
    initial: Oid,
    child: Oid,
    parent: Oid,
    prepared: Oid,
    temp: std::sync::Arc<tempfile::TempDir>,
    case_root: Option<crate::process::CaseResourceHandle<CaseHttpRoot>>,
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
    server_with_context(
        root,
        cert,
        token,
        redirect,
        log,
        &packaged_call_site::native_spawn_context(),
        None,
    )
    .expect("native publication server")
}

fn server_with_context(
    root: &Path,
    cert: Option<(&Path, &Path)>,
    token: Option<&str>,
    redirect: Option<(&str, bool)>,
    log: &Path,
    context: &packaged_call_site::SpawnContext,
    retained_root: Option<std::sync::Arc<tempfile::TempDir>>,
) -> Result<(Server, String), String> {
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
    command.stdout(Stdio::piped()).stderr(Stdio::inherit());
    packaged_call_site::apply_spawn_context(&mut command, context)?;
    let (server, line) = match context.session() {
        None => {
            let mut server = OwnedProcessGroup::spawn(&mut command).unwrap();
            let mut line = String::new();
            BufReader::new(server.stdout.take().unwrap()).read_line(&mut line).unwrap();
            (Server::Native(server), line)
        }
        Some(session) => {
            use std::os::unix::process::CommandExt;
            let root = retained_root.ok_or_else(|| "case HTTP server has no retained root".to_string())?;
            command.process_group(0);
            let handle = crate::process::register_case_resource(session, || {
                let child = command.spawn().map_err(|error| format!("HTTP server spawn: {error}"))?;
                let group = i32::try_from(child.id()).unwrap_or(0);
                Ok(CaseHttpChild { context: session, role: HttpChildRole::Server, child, group, _root: root,
                    stopping: None, killed: false, status: None, error: None })
            })?;
            let stdout = crate::process::with_case_resource(handle,
                |server| server.child.stdout.take())?
                .ok_or_else(|| "HTTP server readiness pipe missing".to_string())?;
            let line = read_case_http_readiness(session, stdout, std::time::Duration::from_secs(45))?;
            (Server::Case(handle), line)
        }
    };
    Ok((server, parse_http_readiness(&line)?))
}

// The native child fixture supplies a real owned pipe to this same reader;
// production supplies the actual Python server's ChildStdout. No OS wait,
// signal or group observation is replaced by this input boundary.
fn read_case_http_readiness<R: std::io::Read + Send + 'static>(
    session: crate::process::CaseContextId,
    stdout: R,
    budget: std::time::Duration,
) -> Result<String, String> {
    let reader = spawn_http_work(session, move || {
        let mut line = String::new();
        BufReader::new(stdout).read_line(&mut line)
            .map_err(|error| format!("HTTP server readiness: {error}"))?;
        Ok::<_, String>(line)
    })?;
    let (line, _joined) = join_http_work(reader, budget)?;
    line
}

fn parse_http_readiness(line: &str) -> Result<String, String> {
    let ready: serde_json::Value = serde_json::from_str(line)
        .map_err(|error| format!("HTTP server readiness JSON: {error}"))?;
    let url = ready["url"].as_str().ok_or_else(|| "HTTP server readiness URL missing".to_string())?;
    Ok(url.to_owned())
}

impl HttpFixture {
    fn new(tls: bool, token: Option<&str>) -> Self {
        Self::open(tls, token, None, None).expect("native http fixture")
    }

    fn complete_owned_server(&mut self) -> Result<CheckedHttpServer, String> {
        let handle = match self._server.as_ref() {
            Some(Server::Case(handle)) => *handle,
            Some(Server::Native(server)) => {
                let _ = server.id();
                return Err("case completion cannot qualify a native server".into());
            }
            None => return Err("fixture server was already released".into()),
        };
        let checked = Self::check_server_completion(stop_http_child(handle)?)?;
        self._server.take();
        Ok(checked)
    }

    fn check_server_completion(proof: CheckedHttpChild) -> Result<CheckedHttpServer, String> {
        if proof.role != HttpChildRole::Server {
            return Err("HTTP server completion came from a different child role".into());
        }
        Ok(CheckedHttpServer { context: proof.context })
    }

    fn complete_case_root(&mut self) -> Result<(), String> {
        if self._server.is_some() { return Err("HTTP server still owns the case root".into()); }
        let handle = self.case_root.ok_or_else(|| "registered HTTP root is missing".to_string())?;
        crate::process::finish_case_resource(handle)?;
        self.case_root.take();
        Ok(())
    }

    fn open(
        tls: bool,
        token: Option<&str>,
        session: Option<crate::process::CaseContextId>,
        mut early: Option<
            &mut dyn FnMut(
                &Path,
                &Path,
                &str,
            ) -> Result<packaged_call_site::SpawnContext, String>,
        >,
    ) -> Result<Self, String> {
        if let Some(context) = session {
            if !crate::process::case_registration_open(context) {
                return Err("HTTP case is not admitted before fixture creation".into());
            }
        }
        let temp = std::sync::Arc::new(tempfile::tempdir().unwrap());
        let case_root = match session {
            Some(context) => {
                let root = std::sync::Arc::clone(&temp);
                Some(crate::process::register_case_resource(context,
                    || Ok(CaseHttpRoot { _root: root }))?)
            }
            None => None,
        };
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
            let args = [
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
                ];
            if let Some(context) = session {
                run_case_tool(context, &temp, "http-certificate", &args)?;
            } else { run("openssl", &args); }
            std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
            let der = temp.path().join("cert.der");
            let args = [
                    "x509",
                    "-in",
                    cert.to_str().unwrap(),
                    "-outform",
                    "DER",
                    "-out",
                    der.to_str().unwrap(),
                ];
            if let Some(context) = session {
                run_case_tool(context, &temp, "http-certificate-der", &args)?;
            } else { run("openssl", &args); }
            cert_der = Some(std::fs::read(der).unwrap());
        }
        let context = if let Some(callback) = early.as_mut() {
            callback(temp.path(), target.path(), &initial.to_string())?
        } else {
            packaged_call_site::native_spawn_context()
        };
        if context.session() != session {
            return Err("HTTP spawn context belongs to another case".into());
        }
        let (_server, url) = server_with_context(
            temp.path(),
            tls.then_some((cert.as_path(), key.as_path())),
            token,
            None,
            &temp.path().join("requests.jsonl"),
            &context,
            session.map(|_| std::sync::Arc::clone(&temp)),
        )?;
        Ok(Self {
            _server: Some(_server),
            source,
            target,
            url,
            cert_der,
            initial,
            child,
            parent,
            prepared,
            temp,
            case_root,
        })
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
    #[cfg(unix)]
    let _process_boundary = packaged_call_site::enter_process_boundary()
        .expect("serialization owner is free");
    #[cfg(unix)]
    match packaged_call_site::attest_catalogue() {
        Ok(packaged_call_site::CatalogueAttestation::PackagedImmutable { manifest }) => {
            if let Err(error) = packaged_f07_http_advertisement(manifest, &_process_boundary) {
                panic!("packaged call site failed closed: {error}");
            }
            return;
        }
        Ok(packaged_call_site::CatalogueAttestation::HistoricalNativeNotCatalogueProof) => {}
        Err(error) => panic!("missing H failed closed: {error}"),
    }
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
    #[cfg(unix)]
    let _process_boundary = packaged_call_site::enter_process_boundary()
        .expect("serialization owner is free");
    #[cfg(unix)]
    match packaged_call_site::attest_catalogue() {
        Ok(packaged_call_site::CatalogueAttestation::PackagedImmutable { manifest }) => {
            if let Err(error) = packaged_f08_https_advertisement(manifest, &_process_boundary) {
                panic!("packaged call site failed closed: {error}");
            }
            return;
        }
        Ok(packaged_call_site::CatalogueAttestation::HistoricalNativeNotCatalogueProof) => {}
        Err(error) => panic!("missing H failed closed: {error}"),
    }
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

#[cfg(unix)]
mod packaged_call_site {
    // Unapplied H6 packaged call-site runtime. Missing H fails closed when a
    // packaged marker or an explicit mode is present. Explicit native-generated-v1
    // with no packaged pin selects historical native. An absent mode is not native.
    // An unknown explicit mode fails closed even when H is well shaped. Constructors do not force native-generated-v1. Inherited packaged
    // markers are rejected before any environment clearing. REPOSYNC_EXEC_CASE_DRAW is refused and is
    // not a nonce. Core reads 16 bytes from /dev/urandom. A harness instance pin is
    // checked against that value and is not the allocator. The nonce may equal
    // case_root_id. Case root is uid 65532 and mode 0700. Repository identity is
    // uid 65532 only. A 64-character hex shape is not authentication of S.

    use std::ffi::{CString, OsStr, OsString};
    use std::io::Error;
    use std::os::fd::RawFd;
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, ExitStatus};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use sha2::Digest;

    pub const F11_CATALOGUE_GIT: &str = "/opt/reliability-exec-fixtures/setup-shim/git";
    const F11_AUTH_SHA256: &str = "78b7f16c98bcf36b6fe852850172544a69a8e471e7d67615040d48cff0d4d9cf";

    pub const PRODUCER_CASES: &[(&str, &str, &str)] = &[
        ("F01", "git-reject-v1", "crates/core/tests/pair_refresh_import_exclusion.rs"),
        ("F02", "git-reject-v1", "crates/core/tests/pair_refresh_legacy_batches.rs"),
        ("F03", "git-final-barrier-v1", "crates/core/tests/pair_refresh_legacy_batches.rs"),
        ("F04", "git-reject-v1", "crates/core/tests/pair_refresh_publication.rs"),
        ("F05", "git-ref-move-v1", "crates/core/tests/pair_refresh_publication.rs"),
        ("F06", "git-reject-v1", "crates/core/src/pair_refresh/authenticated_enrollment_tests.rs:1618"),
        ("F07", "git-ref-move-v1", "crates/core/src/pair_refresh/publication_http_tests.rs"),
        ("F08", "git-ref-move-v1", "crates/core/src/pair_refresh/publication_http_tests.rs"),
        ("F09", "svn-disconnect-barrier-v1", "crates/core/src/pair_refresh/authenticated_enrollment_tests.rs"),
        ("F10", "svn-shutdown-barrier-v1", "crates/core/src/pair_refresh/authenticated_enrollment_tests.rs"),
        ("F11", "setup-push-shim-v1", "crates/web/tests/setup_wizard_busy.rs"),
    ];

    pub fn regression_source_obligations(case_id: &str) -> Result<(), String> {
        if PRODUCER_CASES.len() != 11 {
            return Err("regression source lost an F01-F11 producer".to_string());
        }
        let mut seen = [false; 11];
        for (index, row) in PRODUCER_CASES.iter().enumerate() {
            let expected = format!("F{:02}", index + 1);
            if row.0 != expected {
                return Err(format!("regression source case order drifted at {expected}"));
            }
            seen[index] = true;
        }
        if seen.iter().any(|present| !present) {
            return Err("regression source does not cover every case".to_string());
        }
        if !PRODUCER_CASES.iter().any(|row| {
            row.0 == "F06" && row.2.contains("authenticated_enrollment_tests.rs:1618")
        }) {
            return Err("F06 protected-ref call site is missing".to_string());
        }
        if !PRODUCER_CASES.iter().any(|row| {
            row.0 == "F07"
                && row.2 == "crates/core/src/pair_refresh/publication_http_tests.rs"
        }) || !PRODUCER_CASES.iter().any(|row| {
            row.0 == "F08"
                && row.2 == "crates/core/src/pair_refresh/publication_http_tests.rs"
        }) {
            return Err("F07/F08 http advertisement producers are missing".to_string());
        }
        if !PRODUCER_CASES.iter().any(|row| row.0 == case_id) {
            return Err(format!("case {case_id} is not a connected producer"));
        }
        Ok(())
    }

    pub struct HarnessValidatedH {
        digest: String,
    }

    impl HarnessValidatedH {
        pub fn from_harness_hex(hex_digest: &str) -> Result<Self, String> {
            if hex_digest.len() != 64
                || !hex_digest
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            {
                return Err(
                    "harness-validated H must be 64 lowercase hex characters before preparation"
                        .to_string(),
                );
            }
            Ok(Self {
                digest: hex_digest.to_string(),
            })
        }

        pub fn as_str(&self) -> &str {
            &self.digest
        }
    }

    pub enum CatalogueAttestation {
        HistoricalNativeNotCatalogueProof,
        PackagedImmutable { manifest: HarnessValidatedH },
    }

    pub fn attest_catalogue() -> Result<CatalogueAttestation, String> {
        let mode = std::env::var_os("REPOSYNC_FIXTURE_EXEC_MODE");
        let mode_text = match &mode {
            None => None,
            Some(raw) => Some(
                raw.to_str()
                    .ok_or_else(|| "REPOSYNC_FIXTURE_EXEC_MODE is not UTF-8".to_string())?
                    .to_string(),
            ),
        };
        let packaged_pins = [
            "REPOSYNC_EXEC_EXPECTED_ENTRY",
            "REPOSYNC_EXEC_EXPECTED_CASE",
            "REPOSYNC_EXEC_EXPECTED_INSTANCE",
            "REPOSYNC_EXEC_EXPECTED_ROOT_ID",
            "REPOSYNC_EXEC_EXPECTED_DATA_SHA256",
            "REPOSYNC_EXEC_EXPECTED_MANIFEST_SHA256",
        ];
        let any_pin = packaged_pins
            .iter()
            .any(|key| std::env::var_os(key).is_some());
        match mode_text.as_deref() {
            Some("native-generated-v1") => {
                if any_pin {
                    return Err(
                        "native-generated-v1 refuses a packaged manifest pin".to_string(),
                    );
                }
                Ok(CatalogueAttestation::HistoricalNativeNotCatalogueProof)
            }
            Some("packaged-immutable-v1") => {
                match std::env::var_os("REPOSYNC_EXEC_EXPECTED_MANIFEST_SHA256") {
                    None => Err("absent H".to_string()),
                    Some(raw) => {
                        let text = raw.to_str().unwrap_or_else(|| {
                            panic!("packaged manifest pin failed closed: value is not UTF-8");
                        });
                        match HarnessValidatedH::from_harness_hex(text) {
                            Ok(manifest) => {
                                Ok(CatalogueAttestation::PackagedImmutable { manifest })
                            }
                            Err(error) => {
                                panic!("packaged manifest pin failed closed: {error}")
                            }
                        }
                    }
                }
            }
            Some(_) => Err("explicit mode is required before packaged preparation".to_string()),
            None => Err("explicit mode is required; absent mode is not native".to_string()),
        }
    }

    // This local record locates HTTP custody; admission itself belongs to the
    // shared process gate, not to a second independently allocated integer.
    static HTTP_CONTEXT: Mutex<Option<crate::process::CaseContextId>> = Mutex::new(None);
    static RETAINED_SESSIONS: Mutex<Vec<RetainedSession>> = Mutex::new(Vec::new());

    struct RetainedSession {
        context: crate::process::CaseContextId,
        env: EnvGuard,
        fds: Vec<RawFd>,
        publication: Option<super::CheckedHttpPublication>,
        blocking: Option<super::CheckedHttpBlocking>,
        server: Option<super::CheckedHttpServer>,
    }

    struct SerializationPermit { context: crate::process::CaseContextId }

    fn acquire_serialization() -> Result<SerializationPermit, String> {
        let context = crate::process::admit_case_context()?;
        let mut current = HTTP_CONTEXT.lock().unwrap_or_else(|poison| poison.into_inner());
        if current.is_some() {
            let _ = crate::process::release_case_context(context, false);
            return Err("prior HTTP custody still excludes this caller".into());
        }
        *current = Some(context);
        Ok(SerializationPermit { context })
    }

    impl Drop for SerializationPermit {
        fn drop(&mut self) {
            if RETAINED_SESSIONS.lock().unwrap_or_else(|poison| poison.into_inner())
                .iter().any(|session| session.context == self.context) {
                return;
            }
            // Hold local custody through the common-gate release so a newly
            // admitted HTTP caller cannot observe an obsolete local record.
            let mut current = HTTP_CONTEXT.lock().unwrap_or_else(|poison| poison.into_inner());
            if *current != Some(self.context) { return; }
            if !crate::process::case_context_held(self.context)
                || crate::process::release_case_context(self.context, false).is_ok() {
                *current = None;
            } else {
                crate::process::retain_case_context(self.context);
            }
        }
    }

    /// Observe only this already-admitted cohort. No publication, hook, retry,
    /// assertion proof, or terminal retirement is started by this observer.
    pub fn observe_retained_session(owner: u64) -> Result<(), String> {
        let context = (*HTTP_CONTEXT.lock().unwrap_or_else(|poison| poison.into_inner()))
            .filter(|context| context.serial_number() == owner)
            .ok_or_else(|| "HTTP owner has no retained context".to_string())?;
        crate::process::close_case_registration(context)?;
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let children = crate::process::case_resource_handles::<super::CaseHttpChild>(context);
        let mut errors = Vec::new();
        for handle in &children {
            match crate::process::with_case_resource(*handle, |child| child.request_stop()) {
                Ok(Ok(())) => {}
                Ok(Err(error)) | Err(error) => errors.push(error),
            }
        }
        for handle in children {
            if let Err(error) = super::stop_http_child_until(handle, deadline) {
                errors.push(error);
            }
        }
        if let Err(error) = super::observe_http_threads::<Result<String, String>>(context, deadline) {
            errors.push(error);
        }
        if let Err(error) = super::observe_http_threads::<super::HttpPublicationResult>(context, deadline) {
            errors.push(error);
        }
        if !errors.is_empty() { return Err(errors.join("; ")); }
        Err("HTTP physical observation does not supply the original publication assertions or retire a failed case; results and roots stay retained".into())
    }

    pub fn release_retained_session(owner: u64) -> Result<(), String> {
        let mut sessions = RETAINED_SESSIONS.lock().unwrap_or_else(|poison| poison.into_inner());
        let index = sessions.iter().position(|session| session.context.serial_number() == owner)
            .ok_or_else(|| "HTTP owner has no retained prepared session".to_string())?;
        let session = &mut sessions[index];
        duties_allow_release(session.context, session.publication.as_ref(),
            session.blocking.as_ref(), session.server.as_ref())?;
        if !crate::process::case_context_ready(session.context)
            || crate::process::retained_ownership_incomplete() {
            return Err("actual HTTP work or a failure remains retained".into());
        }
        session.env.restore();
        let mut seen = Vec::new();
        for fd in std::mem::take(&mut session.fds) {
            if fd >= 0 && !seen.contains(&fd) {
                unsafe { libc::close(fd) }; seen.push(fd);
            }
        }
        let context = session.context;
        let mut current = HTTP_CONTEXT.lock().unwrap_or_else(|poison| poison.into_inner());
        crate::process::release_case_context(context, true)?;
        if *current == Some(context) { *current = None; }
        sessions.remove(index);
        Ok(())
    }

    pub struct ProcessBoundary { permit: SerializationPermit }

    pub fn enter_process_boundary() -> Result<ProcessBoundary, String> {
        Ok(ProcessBoundary { permit: acquire_serialization()? })
    }

    impl ProcessBoundary {
        pub fn assert_held(&self) {
            assert_eq!(*HTTP_CONTEXT.lock().unwrap_or_else(|poison| poison.into_inner()),
                Some(self.permit.context), "HTTP boundary does not own local custody");
            assert!(crate::process::case_context_held(self.permit.context),
                "HTTP boundary does not own common case admission");
        }

        pub fn owner(&self) -> u64 { self.permit.context.serial_number() }
        pub(super) fn context(&self) -> crate::process::CaseContextId { self.permit.context }
    }

    pub struct EnvGuard {
        saved: Vec<(String, Option<OsString>)>,
    }

    impl EnvGuard {
        pub fn new() -> Self {
            Self { saved: Vec::new() }
        }

        pub fn push_os(&mut self, key: &str, value: &OsStr) {
            let previous = std::env::var_os(key);
            std::env::set_var(key, value);
            self.saved.push((key.to_string(), previous));
        }

        pub fn push_utf8(&mut self, key: &str, value: &str) {
            self.push_os(key, OsStr::new(value));
        }

        fn restore(&mut self) {
            while let Some((key, previous)) = self.saved.pop() {
                match previous {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            self.restore();
        }
    }

    struct CleanupInner {
        error: Option<String>,
        explicit_ok: bool,
    }

    #[derive(Clone)]
    pub struct CleanupState {
        inner: Arc<Mutex<CleanupInner>>,
    }

    impl CleanupState {
        pub fn new() -> Self {
            Self {
                inner: Arc::new(Mutex::new(CleanupInner {
                    error: None,
                    explicit_ok: false,
                })),
            }
        }

        pub fn record(&self, error: impl Into<String>) {
            let mut guard = self.inner.lock().unwrap_or_else(|poison| poison.into_inner());
            if guard.error.is_none() {
                guard.error = Some(error.into());
            }
        }

        pub fn current(&self) -> Option<String> {
            self.inner
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .error
                .clone()
        }

        fn mark_explicit_ok(&self) -> Result<(), String> {
            let mut guard = self.inner.lock().unwrap_or_else(|poison| poison.into_inner());
            if let Some(error) = &guard.error {
                return Err(format!("cleanup failure prevents qualification: {error}"));
            }
            guard.explicit_ok = true;
            Ok(())
        }

        fn explicit_ok(&self) -> bool {
            self.inner
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .explicit_ok
        }
    }

    pub struct JoinProof {
        _private: (),
    }

    pub struct JoinReport {
        pub proof: JoinProof,
        pub cancelled_tasks: usize,
        pub exit_codes: Vec<i32>,
    }

    pub fn qualify(proof: JoinProof, state: &CleanupState) -> Result<(), String> {
        let _proof = proof;
        state.mark_explicit_ok()?;
        if !state.explicit_ok() {
            return Err("drop cleanup cannot prove completion".to_string());
        }
        Ok(())
    }

    pub struct UnjoinedGuard {
        joined: bool,
        failure: CleanupState,
    }

    impl UnjoinedGuard {
        pub fn arm(failure: &CleanupState) -> Self {
            Self {
                joined: false,
                failure: failure.clone(),
            }
        }

        pub fn mark_joined(mut self) -> Result<JoinProof, String> {
            self.joined = true;
            if let Some(error) = self.failure.current() {
                return Err(error);
            }
            Ok(JoinProof { _private: () })
        }
    }

    impl Drop for UnjoinedGuard {
        fn drop(&mut self) {
            if !self.joined {
                self.failure
                    .record("owned work dropped without an explicit successful join");
            }
        }
    }

    pub fn join_scoped<T>(handle: std::thread::ScopedJoinHandle<'_, T>) -> Result<T, String> {
        handle.join().map_err(|_| {
            "scoped worker panicked; cleanup failure prevents qualification".to_string()
        })
    }

    struct ChildSlot {
        child: Option<Child>,
        reaped: bool,
        qualified: bool,
        failure: CleanupState,
    }

    impl ChildSlot {
        fn vacant(failure: CleanupState) -> Self {
            Self {
                child: None,
                reaped: false,
                qualified: false,
                failure,
            }
        }

        fn explicit_wait(&mut self) -> Result<i32, String> {
            let child = self
                .child
                .as_mut()
                .ok_or_else(|| "missing child".to_string())?;
            let code = wait_child(child, false)?;
            self.reaped = true;
            self.qualified = true;
            self.child.take();
            Ok(code)
        }
    }

    impl Drop for ChildSlot {
        fn drop(&mut self) {
            if self.reaped {
                return;
            }
            if let Some(mut child) = self.child.take() {
                self.reaped = true;
                self.qualified = false;
                match wait_child(&mut child, true) {
                    Ok(_) => self
                        .failure
                        .record("child reaped by drop; drop cleanup is not qualification"),
                    Err(error) => self.failure.record(error),
                }
            }
        }
    }

    fn wait_child(child: &mut Child, kill_first: bool) -> Result<i32, String> {
        if kill_first {
            let _ = child.kill();
        }
        match child.wait() {
            Ok(status) => exit_code(&status),
            Err(error) => Err(format!("wait failed: {error}")),
        }
    }

    fn exit_code(status: &ExitStatus) -> Result<i32, String> {
        status
            .code()
            .ok_or_else(|| "child ended without a status; qualification refused".to_string())
    }

    struct ThreadSlot {
        handle: Option<std::thread::JoinHandle<()>>,
        reaped: bool,
        qualified: bool,
        failure: CleanupState,
    }

    impl ThreadSlot {
        fn vacant(failure: CleanupState) -> Self {
            Self {
                handle: None,
                reaped: false,
                qualified: false,
                failure,
            }
        }

        fn explicit_join(&mut self) -> Result<(), String> {
            let handle = self
                .handle
                .take()
                .ok_or_else(|| "missing thread".to_string())?;
            match handle.join() {
                Ok(()) => {
                    self.reaped = true;
                    self.qualified = true;
                    Ok(())
                }
                Err(_) => {
                    self.reaped = true;
                    self.qualified = false;
                    let error =
                        "owned thread panicked; cleanup failure prevents qualification".to_string();
                    self.failure.record(error.clone());
                    Err(error)
                }
            }
        }
    }

    impl Drop for ThreadSlot {
        fn drop(&mut self) {
            if self.reaped {
                return;
            }
            if let Some(handle) = self.handle.take() {
                self.reaped = true;
                self.qualified = false;
                if handle.join().is_err() {
                    self.failure
                        .record("drop join observed a thread panic; completion is not proven");
                } else {
                    self.failure
                        .record("thread joined by drop; drop cleanup is not qualification");
                }
            }
        }
    }

    struct TaskSlot {
        handle: Option<tokio::task::JoinHandle<()>>,
        reaped: bool,
        qualified: bool,
        failure: CleanupState,
    }

    impl TaskSlot {
        fn vacant(failure: CleanupState) -> Self {
            Self {
                handle: None,
                reaped: false,
                qualified: false,
                failure,
            }
        }
    }

    impl Drop for TaskSlot {
        fn drop(&mut self) {
            if self.reaped {
                return;
            }
            if let Some(handle) = self.handle.take() {
                crate::process::retain_unjoined_task(handle);
                self.qualified = false;
                self.failure
                    .record("unjoined task retained; drop abort is not qualification");
            }
        }
    }

    pub struct OwnedWork {
        children: Vec<ChildSlot>,
        threads: Vec<ThreadSlot>,
        tasks: Vec<TaskSlot>,
        failure: CleanupState,
        explicit: bool,
        settled: bool,
    }

    impl OwnedWork {
        pub fn new() -> Self {
            Self {
                children: Vec::new(),
                threads: Vec::new(),
                tasks: Vec::new(),
                failure: CleanupState::new(),
                explicit: false,
                settled: false,
            }
        }

        pub fn state(&self) -> CleanupState {
            self.failure.clone()
        }

        pub fn spawn_command(&mut self, mut command: Command) -> Result<(), String> {
            self.children.push(ChildSlot::vacant(self.failure.clone()));
            let child = match command.spawn() {
                Ok(child) => child,
                Err(error) => return Err(format!("spawn: {error}")),
            };
            self.children
                .last_mut()
                .ok_or_else(|| "child slot was not registered".to_string())?
                .child = Some(child);
            Ok(())
        }

        pub fn spawn_thread<F>(&mut self, work: F) -> Result<(), String>
        where
            F: FnOnce() -> Result<(), String> + Send + 'static,
        {
            self.threads.push(ThreadSlot::vacant(self.failure.clone()));
            let failure = self.failure.clone();
            let handle = match std::thread::Builder::new().spawn(move || {
                if let Err(error) = work() {
                    failure.record(error);
                }
            }) {
                Ok(handle) => handle,
                Err(error) => return Err(format!("thread spawn: {error}")),
            };
            self.threads
                .last_mut()
                .ok_or_else(|| "thread slot was not registered".to_string())?
                .handle = Some(handle);
            Ok(())
        }

        pub fn spawn_task<F>(&mut self, future: F)
        where
            F: std::future::Future<Output = ()> + Send + 'static,
        {
            self.tasks.push(TaskSlot::vacant(self.failure.clone()));
            let handle = tokio::spawn(future);
            if let Some(slot) = self.tasks.last_mut() {
                slot.handle = Some(handle);
            }
        }

        pub fn task_finished(&self, index: usize) -> bool {
            self.tasks
                .get(index)
                .and_then(|slot| slot.handle.as_ref())
                .map(tokio::task::JoinHandle::is_finished)
                .unwrap_or(true)
        }

        pub fn abort_task(&mut self, index: usize) {
            if let Some(handle) = self.tasks.get(index).and_then(|slot| slot.handle.as_ref()) {
                handle.abort();
            }
        }

        pub fn join_sync(mut self) -> Result<JoinReport, String> {
            if !self.tasks.is_empty() {
                return Err("async tasks require join_async".to_string());
            }
            let report = match self.finish_sync() {
                Ok(report) => report,
                Err(error) => {
                    self.settled = true;
                    return Err(error);
                }
            };
            self.explicit = true;
            self.settled = true;
            Ok(report)
        }

        fn finish_sync(&mut self) -> Result<JoinReport, String> {
            let mut errors = Vec::new();
            let mut exit_codes = Vec::new();
            for slot in &mut self.children {
                match slot.explicit_wait() {
                    Ok(code) => exit_codes.push(code),
                    Err(error) => errors.push(error),
                }
            }
            for slot in &mut self.threads {
                if let Err(error) = slot.explicit_join() {
                    errors.push(error);
                }
            }
            if let Some(error) = self.failure.current() {
                errors.push(error);
            }
            if !errors.is_empty() {
                return Err(errors.join("; "));
            }
            if self
                .children
                .iter()
                .any(|slot| !slot.qualified)
                || self.threads.iter().any(|slot| !slot.qualified)
            {
                return Err("a joined slot was not qualified".to_string());
            }
            Ok(JoinReport {
                proof: JoinProof { _private: () },
                cancelled_tasks: 0,
                exit_codes,
            })
        }

        pub async fn join_async(mut self) -> Result<JoinReport, String> {
            use std::future::Future;
            let mut report = match self.finish_sync() {
                Ok(report) => report,
                Err(error) => {
                    self.settled = true;
                    return Err(error);
                }
            };
            let mut errors = Vec::new();
            for slot in &mut self.tasks {
                if slot.handle.is_none() {
                    errors.push("missing task".to_string());
                    continue;
                }
                let polled = std::future::poll_fn(|cx| {
                    let handle = slot
                        .handle
                        .as_mut()
                        .expect("task handle remains registered until poll completes");
                    std::pin::Pin::new(handle).poll(cx)
                })
                .await;
                match polled {
                    Ok(()) => {
                        slot.reaped = true;
                        slot.qualified = true;
                        slot.handle.take();
                    }
                    Err(error) if error.is_cancelled() => {
                        slot.reaped = true;
                        slot.qualified = false;
                        slot.handle.take();
                        report.cancelled_tasks += 1;
                    }
                    Err(error) => {
                        slot.reaped = true;
                        slot.qualified = false;
                        slot.handle.take();
                        errors.push(format!(
                            "task panicked or join failed; qualification refused: {error}"
                        ));
                    }
                }
            }
            if let Some(error) = self.failure.current() {
                errors.push(error);
            }
            if !errors.is_empty() {
                self.settled = true;
                return Err(errors.join("; "));
            }
            self.explicit = true;
            self.settled = true;
            Ok(report)
        }
    }

    impl Drop for OwnedWork {
        fn drop(&mut self) {
            if self.explicit || self.settled {
                return;
            }
            self.failure
                .record("owned work dropped before explicit join proved completion");
        }
    }


    pub struct ReceiverStatus {
        pub publication_rejected: bool,
        pub observed_exit: Option<i32>,
    }

    impl ReceiverStatus {
        pub fn from_publication_result(publication_rejected: bool) -> Self {
            Self {
                publication_rejected,
                observed_exit: None,
            }
        }
    }

    pub struct AuditChildExit {
        pub value: i64,
    }

    pub struct RefMoveObservation {
        pub audit_child_exit: AuditChildExit,
        pub receiver_status: ReceiverStatus,
        pub another_update_ref_permitted: bool,
    }

    fn canonical_number(raw: &str) -> Result<String, String> {
        if raw.is_empty() || raw.starts_with('+') || raw.starts_with('-') {
            return Err("number is not a canonical non-negative integer".to_string());
        }
        if raw.len() > 1 && raw.starts_with('0') {
            return Err("number spelling is not canonical".to_string());
        }
        if !raw.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err("number is not decimal".to_string());
        }
        let value: i64 = raw
            .parse()
            .map_err(|_| "number is not canonical".to_string())?;
        let canon = value.to_string();
        if canon != raw {
            return Err("number spelling is not canonical".to_string());
        }
        Ok(canon)
    }

    fn parse_flat_object(bytes: &[u8]) -> Result<std::collections::BTreeMap<String, String>, String> {
        if bytes.last() != Some(&b'\n') || bytes.iter().filter(|byte| **byte == b'\n').count() != 1 {
            return Err("witness is not one LF-terminated line".to_string());
        }
        let line = std::str::from_utf8(&bytes[..bytes.len() - 1])
            .map_err(|_| "witness is not UTF-8".to_string())?;
        if line.contains(' ') || !line.starts_with('{') || !line.ends_with('}') {
            return Err("witness is not canonical".to_string());
        }
        let mut map = std::collections::BTreeMap::new();
        let mut rest = &line[1..line.len() - 1];
        if rest.is_empty() {
            return Err("witness object is empty".to_string());
        }
        while !rest.is_empty() {
            if !rest.starts_with('"') {
                return Err("witness key is not a string".to_string());
            }
            let key_end = rest[1..]
                .find('"')
                .ok_or_else(|| "witness key is unterminated".to_string())?;
            let key = &rest[1..1 + key_end];
            rest = &rest[1 + key_end + 1..];
            if !rest.starts_with(':') {
                return Err("witness separator is missing".to_string());
            }
            rest = &rest[1..];
            let (encoded, next) = if let Some(value_rest) = rest.strip_prefix('"') {
                let value_end = value_rest
                    .find('"')
                    .ok_or_else(|| "witness string is unterminated".to_string())?;
                let value = &value_rest[..value_end];
                if value.contains('\\') || value.contains('"') {
                    return Err("witness string encoding is refused".to_string());
                }
                (format!("\"{value}\""), &value_rest[value_end + 1..])
            } else {
                let number = rest
                    .split(',')
                    .next()
                    .ok_or_else(|| "witness number is missing".to_string())?;
                let canon = canonical_number(number)?;
                (canon, &rest[number.len()..])
            };
            if map.insert(key.to_string(), encoded).is_some() {
                return Err("witness repeats a key".to_string());
            }
            if next.is_empty() {
                break;
            }
            if !next.starts_with(',') {
                return Err("witness field separator is refused".to_string());
            }
            rest = &next[1..];
        }
        let mut rebuilt = String::from("{");
        for (index, (key, value)) in map.iter().enumerate() {
            if index > 0 {
                rebuilt.push(',');
            }
            rebuilt.push('"');
            rebuilt.push_str(key);
            rebuilt.push_str("\":");
            rebuilt.push_str(value);
        }
        rebuilt.push('}');
        rebuilt.push('\n');
        if rebuilt.as_bytes() != bytes {
            return Err("witness bytes are not canonical".to_string());
        }
        Ok(map)
    }

    fn last_os_error(context: &str) -> String {
        format!("{context}: {}", Error::last_os_error())
    }

    fn c_name(name: &str) -> Result<CString, String> {
        CString::new(name).map_err(|_| format!("component contains NUL: {name}"))
    }

    fn is_component(name: &str) -> bool {
        !name.is_empty() && name != "." && name != ".." && !name.contains('/') && !name.contains('\0')
    }

    fn close_fd(fd: RawFd) {
        if fd >= 0 {
            unsafe { libc::close(fd) };
        }
    }

    fn file_type(mode: libc::mode_t) -> libc::mode_t {
        mode & libc::S_IFMT
    }

    fn perm_bits(mode: libc::mode_t) -> u32 {
        (mode & 0o777) as u32
    }

    fn fstat_fd(fd: RawFd) -> Result<libc::stat, String> {
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(fd, &mut st) } < 0 {
            return Err(last_os_error("fstat"));
        }
        Ok(st)
    }

    fn open_nofollow(path: &Path, want_dir: bool) -> Result<RawFd, String> {
        let text = path
            .to_str()
            .ok_or_else(|| "path is not UTF-8".to_string())?;
        if !text.starts_with('/') || text.contains("//") || text.ends_with('/') {
            return Err(format!("refusing non-canonical absolute path {text}"));
        }
        let root = c_name("/")?;
        let mut fd = unsafe {
            libc::open(
                root.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(last_os_error("open /"));
        }
        let parts: Vec<&str> = text.split('/').filter(|part| !part.is_empty()).collect();
        for (index, part) in parts.iter().enumerate() {
            if !is_component(part) {
                close_fd(fd);
                return Err(format!("refusing path component in {text}"));
            }
            let last = index + 1 == parts.len();
            let mut flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
            if !last || want_dir {
                flags |= libc::O_DIRECTORY;
            }
            let c = match c_name(part) {
                Ok(c) => c,
                Err(error) => {
                    close_fd(fd);
                    return Err(error);
                }
            };
            let next = unsafe { libc::openat(fd, c.as_ptr(), flags, 0) };
            close_fd(fd);
            if next < 0 {
                return Err(last_os_error(&format!("nofollow open {text}")));
            }
            fd = next;
        }
        Ok(fd)
    }

    fn require_case_root_directory(fd: RawFd) -> Result<(), String> {
        let st = fstat_fd(fd)?;
        if file_type(st.st_mode) != libc::S_IFDIR || st.st_uid != 65532 || perm_bits(st.st_mode) != 0o700
        {
            return Err(
                "case root must be uid 65532 and mode 0700; creation does not chown"
                    .to_string(),
            );
        }
        Ok(())
    }

    fn require_repository_directory(fd: RawFd) -> Result<(), String> {
        let st = fstat_fd(fd)?;
        if file_type(st.st_mode) != libc::S_IFDIR || st.st_uid != 65532 {
            return Err(
                "repository directory must be uid 65532; mode bits are not a repository rule"
                    .to_string(),
            );
        }
        Ok(())
    }

    fn fstatat_nofollow(dir_fd: RawFd, name: &str) -> Result<Option<libc::stat>, String> {
        if !is_component(name) {
            return Err(format!("refusing component {name}"));
        }
        let c = c_name(name)?;
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::fstatat(dir_fd, c.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) };
        if rc == 0 {
            return Ok(Some(st));
        }
        let error = Error::last_os_error();
        if error.raw_os_error() == Some(libc::ENOENT) {
            return Ok(None);
        }
        Err(format!("fstatat {name}: {error}"))
    }

    pub fn publish_nofollow(dir_fd: RawFd, name: &str, payload: &[u8], mode: u32) -> Result<(), String> {
        if !is_component(name) || name.starts_with('.') {
            return Err(format!("refusing publication name {name}"));
        }
        if mode != 0o644 && mode != 0o600 {
            return Err("publication mode must be 0644 or 0600".to_string());
        }
        if payload.len() > 4096 {
            return Err("publication payload exceeds 4096 bytes".to_string());
        }
        let tmp_name = format!(".rs-publish-{name}");
        let tmp = c_name(&tmp_name)?;
        let dest = c_name(name)?;
        let fd = unsafe {
            libc::openat(
                dir_fd,
                tmp.as_ptr(),
                libc::O_CREAT | libc::O_EXCL | libc::O_WRONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                mode as libc::mode_t,
            )
        };
        if fd < 0 {
            return Err(last_os_error(
                "exclusive nofollow temporary create refused; destination was not replaced",
            ));
        }
        let written = (|| {
            if unsafe { libc::fchmod(fd, mode as libc::mode_t) } < 0 {
                return Err(last_os_error("fchmod publication"));
            }
            let mut offset = 0usize;
            while offset < payload.len() {
                let wrote = unsafe {
                    libc::write(
                        fd,
                        payload[offset..].as_ptr() as *const libc::c_void,
                        payload.len() - offset,
                    )
                };
                if wrote < 0 {
                    let error = Error::last_os_error();
                    if error.raw_os_error() == Some(libc::EINTR) {
                        continue;
                    }
                    return Err(format!("write: {error}"));
                }
                if wrote == 0 {
                    return Err("short write".to_string());
                }
                offset += wrote as usize;
            }
            if unsafe { libc::fsync(fd) } < 0 {
                return Err(last_os_error("fsync publication"));
            }
            Ok(())
        })();
        close_fd(fd);
        if let Err(error) = written {
            unsafe { libc::unlinkat(dir_fd, tmp.as_ptr(), 0) };
            return Err(error);
        }
        let linked = unsafe { libc::linkat(dir_fd, tmp.as_ptr(), dir_fd, dest.as_ptr(), 0) };
        unsafe { libc::unlinkat(dir_fd, tmp.as_ptr(), 0) };
        if linked < 0 {
            return Err("linkat refused; an existing node was not replaced".to_string());
        }
        unsafe { libc::fsync(dir_fd) };
        let verify = unsafe {
            libc::openat(
                dir_fd,
                dest.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0,
            )
        };
        if verify < 0 {
            return Err(last_os_error("reopen published node"));
        }
        let verified = (|| {
            let st = fstat_fd(verify)?;
            if file_type(st.st_mode) != libc::S_IFREG
                || st.st_nlink as u64 != 1
                || st.st_uid != 65532
                || perm_bits(st.st_mode) != mode
                || st.st_size as u64 != payload.len() as u64
            {
                return Err("published node failed regular/nlink/owner/mode check".to_string());
            }
            let mut body = vec![0u8; payload.len()];
            read_exact(verify, &mut body)?;
            if body != payload {
                return Err("published node bytes changed".to_string());
            }
            Ok(())
        })();
        close_fd(verify);
        verified
    }

    fn read_nofollow(dir_fd: RawFd, name: &str) -> Result<Vec<u8>, String> {
        if !is_component(name) {
            return Err(format!("refusing component {name}"));
        }
        let c = c_name(name)?;
        let fd = unsafe {
            libc::openat(
                dir_fd,
                c.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0,
            )
        };
        if fd < 0 {
            return Err(last_os_error(&format!("nofollow read {name}")));
        }
        let read = (|| {
            let st = fstat_fd(fd)?;
            if file_type(st.st_mode) != libc::S_IFREG
                || st.st_nlink as u64 != 1
                || st.st_uid != 65532
                || (perm_bits(st.st_mode) != 0o600 && perm_bits(st.st_mode) != 0o644)
                || st.st_size < 0
                || st.st_size as u64 > 4096
            {
                return Err(format!("refusing node {name}"));
            }
            let mut body = vec![0u8; st.st_size as usize];
            read_exact(fd, &mut body)?;
            Ok(body)
        })();
        close_fd(fd);
        read
    }

    fn read_exact(fd: RawFd, body: &mut [u8]) -> Result<(), String> {
        let mut offset = 0usize;
        while offset < body.len() {
            let got = unsafe {
                libc::read(
                    fd,
                    body[offset..].as_mut_ptr() as *mut libc::c_void,
                    body.len() - offset,
                )
            };
            if got < 0 {
                let error = Error::last_os_error();
                if error.raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                return Err(format!("read: {error}"));
            }
            if got == 0 {
                return Err("short read".to_string());
            }
            offset += got as usize;
        }
        Ok(())
    }

    pub fn publish_at(path: &Path, name: &str, payload: &[u8], mode: u32) -> Result<(), String> {
        let fd = open_nofollow(path, true)?;
        let published = publish_nofollow(fd, name, payload, mode);
        close_fd(fd);
        published
    }

    pub async fn wait_marker_async(dir: &Path, name: &str, bound: Duration) -> Result<(), String> {
        let deadline = Instant::now() + bound;
        loop {
            if marker_is_regular(dir, name)? {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(format!("bounded readiness expired waiting for {name}"));
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    pub fn wait_marker(dir: &Path, name: &str, bound: Duration) -> Result<(), String> {
        let deadline = Instant::now() + bound;
        loop {
            if marker_is_regular(dir, name)? {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(format!("bounded readiness expired waiting for {name}"));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn marker_is_regular(dir: &Path, name: &str) -> Result<bool, String> {
        let fd = open_nofollow(dir, true)?;
        let found = match fstatat_nofollow(fd, name)? {
            Some(st) => file_type(st.st_mode) == libc::S_IFREG,
            None => false,
        };
        close_fd(fd);
        Ok(found)
    }

    fn canonical_json(fields: &[(&str, &str)]) -> Result<Vec<u8>, String> {
        let mut encoded: Vec<(&str, String)> = Vec::with_capacity(fields.len() + 1);
        for (key, value) in fields {
            if key.contains('"') || key.contains('\\') || value.contains('"') || value.contains('\\') {
                return Err("canonical field contains a character this encoder refuses".to_string());
            }
            encoded.push((*key, format!("\"{value}\"")));
        }
        encoded.push(("schema", "1".to_string()));
        encoded.sort_by(|left, right| left.0.cmp(right.0));
        let mut text = String::from("{");
        for (index, (key, value)) in encoded.iter().enumerate() {
            if index > 0 {
                text.push(',');
            }
            text.push('"');
            text.push_str(key);
            text.push_str("\":");
            text.push_str(value);
        }
        text.push('}');
        text.push('\n');
        Ok(text.into_bytes())
    }

    fn validate_initial_oid(value: &str) -> Result<(), String> {
        if value.len() == 40 && value.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')) {
            Ok(())
        } else {
            Err("initial_oid must be 40 lowercase hex characters".to_string())
        }
    }

    struct AdmittedHook {
        entry_id: &'static str,
        slot: &'static str,
        target: &'static str,
    }

    const ADMITTED_HOOKS: &[AdmittedHook] = &[
        AdmittedHook {
            entry_id: "git-reject-v1",
            slot: "pre-receive",
            target: "/opt/reliability-exec-fixtures/git-reject/pre-receive",
        },
        AdmittedHook {
            entry_id: "git-final-barrier-v1",
            slot: "pre-receive",
            target: "/opt/reliability-exec-fixtures/git-final-barrier/pre-receive",
        },
        AdmittedHook {
            entry_id: "git-ref-move-v1",
            slot: "pre-receive",
            target: "/opt/reliability-exec-fixtures/git-ref-move/pre-receive",
        },
        AdmittedHook {
            entry_id: "svn-disconnect-barrier-v1",
            slot: "pre-commit",
            target: "/opt/reliability-exec-fixtures/svn-disconnect/pre-commit",
        },
        AdmittedHook {
            entry_id: "svn-shutdown-barrier-v1",
            slot: "pre-commit",
            target: "/opt/reliability-exec-fixtures/svn-shutdown/pre-commit",
        },
    ];

    pub struct PreparedRepo {
        case_fd: RawFd,
        repo_fd: RawFd,
        config_fd: RawFd,
        marker_fd: RawFd,
        env: EnvGuard,
        selected_path: String,
        selected_sha256: String,
        selected_len: u64,
        selected_mode: u32,
        case_root_id: String,
        explicitly_completed: bool,
        context: crate::process::CaseContextId,
        publication: Option<super::CheckedHttpPublication>,
        blocking: Option<super::CheckedHttpBlocking>,
        server: Option<super::CheckedHttpServer>,
        proved_markers: std::sync::Mutex<Vec<String>>,
        selected_installed_relative_path: String,
        selected_installed_mode: u32,
        pub entry_id: String,
        pub case_id: String,
        pub instance_id: String,
        pub manifest_sha256: String,
        pub data_sha256: String,
        pub initial_oid: Option<String>,
        pub marker_path: PathBuf,
        hook_target: Option<String>,
        hook_dev: Option<u64>,
        hook_ino: Option<u64>,
        hook_link_dev: Option<u64>,
        hook_link_ino: Option<u64>,
    }

    impl Drop for PreparedRepo {
        fn drop(&mut self) {
            if self.explicitly_completed {
                return;
            }
            crate::process::retain_case_context(self.context);
            let mut retained = RETAINED_SESSIONS
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let mut fds = Vec::new();
            for fd in [
                &mut self.case_fd,
                &mut self.repo_fd,
                &mut self.config_fd,
                &mut self.marker_fd,
            ] {
                if *fd >= 0 && !fds.contains(fd) { fds.push(*fd); }
                *fd = -1;
            }
            retained.push(RetainedSession {
                context: self.context,
                env: std::mem::replace(&mut self.env, EnvGuard::new()),
                fds,
                publication: self.publication.take(),
                blocking: self.blocking.take(),
                server: self.server.take(),
            });
        }
    }

    fn duties_allow_release(
        context: crate::process::CaseContextId,
        publication: Option<&super::CheckedHttpPublication>,
        blocking: Option<&super::CheckedHttpBlocking>,
        server: Option<&super::CheckedHttpServer>,
    ) -> Result<(), String> {
        if !publication.is_some_and(|proof| proof.join.context == context)
            || !blocking.is_some_and(|proof| proof.context == context)
            || !server.is_some_and(|proof| proof.context == context) {
            return Err(
                "this session has not joined its publication, blocking work, and server"
                    .to_string(),
            );
        }
        Ok(())
    }

    #[cfg(test)]
    mod http_lifecycle_negative_tests {
        use super::*;
        use std::os::fd::IntoRawFd;
        use std::os::unix::net::UnixStream;

        // These are native unit probes of ownership/refusal, never packaged
        // fixture qualification. Each leaf executes once in a fresh libtest
        // process, because expected failures deliberately retain case state.
        fn isolated(leaf: &str) -> bool {
            assert!(std::env::var_os("REPOSYNC_LAUNCH_BINDING_V1").is_none(),
                "native negative unit refuses a packaged launch channel");
            assert_eq!(std::env::var("REPOSYNC_FIXTURE_EXEC_MODE").ok().as_deref(),
                Some("native-generated-v1"), "native negative unit needs explicit native mode");
            const CHILD: &str = "REPOSYNC_HTTP_NEGATIVE_UNIT_CHILD";
            let module = module_path!().split_once("::").expect("crate module prefix").1;
            let full = format!("{module}::{leaf}");
            if std::env::var(CHILD).ok().as_deref() == Some(full.as_str()) {
                return false;
            }
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command.args(["--exact", full.as_str(), "--nocapture", "--test-threads=1",
                "--format", "pretty", "--color", "never"])
                .env(CHILD, &full).env_remove("REPOSYNC_LAUNCH_BINDING_V1");
            // Existing native admission rejects packaged markers rather than
            // converting an inherited packaged expectation into native proof.
            admit_child_environment(&command, ExplicitMode::NativeGenerated)
                .expect("native negative child admission");
            command.env("REPOSYNC_FIXTURE_EXEC_MODE", "native-generated-v1");
            let scratch = tempfile::Builder::new().prefix("rs-http-negative-").tempdir().unwrap();
            command.env("TMPDIR", scratch.path()).env("TMP", scratch.path()).env("TEMP", scratch.path());
            let output = match command.output() {
                Ok(output) => output,
                Err(error) => {
                    let path = scratch.path().to_owned();
                    std::mem::forget(scratch);
                    panic!("negative child spawn failed; scratch={path:?}: {error}");
                }
            };
            let stdout = String::from_utf8_lossy(&output.stdout);
            let passed = format!("test {full} ... ok");
            if !output.status.success()
                || stdout.lines().filter(|line| *line == passed.as_str()).count() != 1
                || !stdout.contains("1 passed; 0 failed; 0 ignored;") {
                let path = scratch.path().to_owned();
                std::mem::forget(scratch);
                panic!("negative child failed or selected zero/ignored tests; scratch={path:?}; stdout={stdout}; stderr={}",
                    String::from_utf8_lossy(&output.stderr));
            }
            // Keep intentional worker-panic evidence without duplicating the
            // child's passing stdout row in the parent libtest inventory.
            if !output.stderr.is_empty() { eprint!("{}", String::from_utf8_lossy(&output.stderr)); }
            // Child has exited: its retained FDs are closed by process exit,
            // and this parent owns removal of only its scratch directory.
            true
        }

        fn retained_root(context: crate::process::CaseContextId)
            -> (crate::process::CaseResourceHandle<super::super::CaseHttpRoot>,
                std::sync::Weak<tempfile::TempDir>) {
            let root = std::sync::Arc::new(tempfile::tempdir().unwrap());
            let weak = std::sync::Arc::downgrade(&root);
            let handle = crate::process::register_case_resource(context,
                || Ok(super::super::CaseHttpRoot { _root: root })).unwrap();
            (handle, weak)
        }

        // Deliberately unadmitted metadata: only failure/Drop paths are used.
        // No H, S/P/J, executable pin, hook or positive launch is fabricated.
        fn unadmitted_cleanup_record(context: crate::process::CaseContextId, fd: RawFd) -> PreparedRepo {
            PreparedRepo {
                case_fd: fd, repo_fd: fd, config_fd: -1, marker_fd: -1,
                env: EnvGuard::new(), selected_path: String::new(),
                selected_sha256: String::new(), selected_len: 0, selected_mode: 0,
                case_root_id: String::new(), explicitly_completed: false, context,
                publication: None, blocking: None, server: None,
                proved_markers: Mutex::new(Vec::new()),
                selected_installed_relative_path: String::new(), selected_installed_mode: 0,
                entry_id: String::new(), case_id: String::new(), instance_id: String::new(),
                manifest_sha256: String::new(), data_sha256: String::new(), initial_oid: None,
                marker_path: PathBuf::new(), hook_target: None, hook_dev: None, hook_ino: None,
                hook_link_dev: None, hook_link_ino: None,
            }
        }

        #[test]
        fn http_l1_shared_admission_excludes_a_second_case() {
            if isolated("http_l1_shared_admission_excludes_a_second_case") { return; }
            let boundary = enter_process_boundary().unwrap();
            let first = boundary.context();
            boundary.assert_held();
            assert!(crate::process::admit_case_context().is_err(),
                "HTTP-local serialization must exclude the common admission path");
            drop(boundary);
            assert!(!crate::process::case_context_held(first));
            let next = crate::process::admit_case_context().unwrap();
            assert_ne!(first, next, "common case identities are not file-local counters");
            crate::process::close_case_registration(next).unwrap();
            crate::process::release_case_context(next, false).unwrap();
        }

        #[test]
        fn http_l2_lost_observer_keeps_thread_result_and_root() {
            if isolated("http_l2_lost_observer_keeps_thread_result_and_root") { return; }
            let context = crate::process::admit_case_context().unwrap();
            let (root, weak) = retained_root(context);
            let (release, wait) = std::sync::mpsc::channel();
            let (started, ready) = std::sync::mpsc::channel();
            {
                let _local_observer = super::super::spawn_http_work(context, move || {
                    started.send(()).unwrap();
                    wait.recv().unwrap();
                    37u8
                }).unwrap();
                ready.recv_timeout(Duration::from_secs(2)).unwrap();
            }
            assert!(weak.upgrade().is_some(), "case directory lost with its observer");
            assert!(crate::process::release_case_context(context, true).is_err());
            let handles = crate::process::case_resource_handles::<super::super::CaseHttpThread<u8>>(context);
            assert_eq!(handles.len(), 1, "actual thread must remain registered");
            crate::process::close_case_registration(context).unwrap();
            release.send(()).unwrap();
            let (value, proof) = super::super::join_http_work(handles[0], Duration::from_secs(2)).unwrap();
            assert_eq!(value, 37, "original thread result was lost");
            assert_eq!(proof.context, context);
            assert!(weak.upgrade().is_some(), "joined thread is not root release authority");
            crate::process::finish_case_resource(root).unwrap();
            assert!(weak.upgrade().is_none());
            crate::process::release_case_context(context, false).unwrap();
        }

        #[test]
        fn http_l3_panic_handle_stays_consumed_and_case_held() {
            if isolated("http_l3_panic_handle_stays_consumed_and_case_held") { return; }
            let context = crate::process::admit_case_context().unwrap();
            let handle = super::super::spawn_http_work(context, || -> u8 {
                panic!("HTTP-L3 expected owned worker panic");
            }).unwrap();
            let first = super::super::join_http_work(handle, Duration::from_secs(2))
                .map(|_| ()).unwrap_err();
            assert!(first.contains("panicked"));
            crate::process::with_case_resource(handle, |slot| {
                assert!(slot.handle.is_none(), "consumed panic handle must not be polled again");
                assert!(slot.ready.is_none());
                assert!(slot.error.is_some());
            }).unwrap();
            let again = super::super::join_http_work(handle, Duration::from_secs(2))
                .map(|_| ()).unwrap_err();
            assert_eq!(first, again);
            assert!(!crate::process::case_context_ready(context));
            assert!(crate::process::release_case_context(context, true).is_err());
            assert!(crate::process::admit_case_context().is_err());
        }

        #[test]
        fn http_l7_other_case_join_cannot_qualify() {
            if isolated("http_l7_other_case_join_cannot_qualify") { return; }
            let first = enter_process_boundary().unwrap();
            let context = first.context();
            let handle = super::super::spawn_http_work(context, || 5u8).unwrap();
            let (_, join) = super::super::join_http_work(handle, Duration::from_secs(2)).unwrap();
            crate::process::close_case_registration(context).unwrap();
            drop(first);
            let second = enter_process_boundary().unwrap();
            let other = second.context();
            assert_ne!(context, other);
            let (_root, _weak) = retained_root(other);
            let (stream, _peer) = UnixStream::pair().unwrap();
            let mut prepared = unadmitted_cleanup_record(other, stream.into_raw_fd());
            let error = prepared.record_publication_joined(super::super::CheckedHttpPublication { join })
                .unwrap_err();
            assert!(error.contains("changed owner"));
            assert!(prepared.publication.is_none());
            prepared.record_blocking_joined();
            assert!(prepared.blocking.is_none());
            assert!(prepared.seal().unwrap_err().contains("has not joined"));
            drop(prepared);
            assert!(crate::process::release_case_context(other, true).is_err());
        }

        #[test]
        fn http_l8_unbound_projection_never_calls_fixture_factory() {
            if isolated("http_l8_unbound_projection_never_calls_fixture_factory") { return; }
            let called = std::cell::Cell::new(false);
            let error = super::super::with_attested_http_entry(|_| {
                called.set(true);
                Ok::<_, String>(())
            }).unwrap_err();
            assert!(error.contains("derived runtime projection is unbound"));
            assert!(!called.get(), "unbound projection reached fixture creation");
        }

        #[test]
        fn http_l9_aliased_descriptors_stay_owned_until_proof() {
            if isolated("http_l9_aliased_descriptors_stay_owned_until_proof") { return; }
            let boundary = enter_process_boundary().unwrap();
            let context = boundary.context();
            let (_root, weak) = retained_root(context);
            let (stream, _peer) = UnixStream::pair().unwrap();
            let fd = stream.into_raw_fd();
            drop(unadmitted_cleanup_record(context, fd));
            {
                let sessions = RETAINED_SESSIONS.lock().unwrap_or_else(|poison| poison.into_inner());
                let retained = sessions.iter().find(|session| session.context == context).unwrap();
                assert_eq!(retained.fds.as_slice(), &[fd], "aliased descriptor was retained twice");
            }
            assert!(unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0);
            assert!(release_retained_session(context.serial_number()).is_err());
            assert!(release_retained_session(context.serial_number().checked_add(1).unwrap()).is_err());
            assert!(unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0,
                "refused or foreign release closed this case's descriptor");
            assert!(weak.upgrade().is_some());
            assert!(!crate::process::case_context_ready(context));
            assert!(crate::process::release_case_context(context, true).is_err());
        }
    }

    #[cfg(test)]
    mod http_child_failure_tests {
        use super::*;
        use std::io::{Read, Write};
        use std::os::fd::{AsRawFd, FromRawFd};
        use std::os::unix::fs::OpenOptionsExt;
        use std::os::unix::net::UnixStream;
        use std::os::unix::process::CommandExt;

        type ChildHandle = crate::process::CaseResourceHandle<super::super::CaseHttpChild>;
        const UNIT_CHILD: &str = "REPOSYNC_HTTP_CHILD_FAILURE_UNIT";
        const PROBE: &str = "REPOSYNC_HTTP_FIXED_PROBE";
        const SCENARIO: &str = "REPOSYNC_HTTP_FIXED_PROBE_SCENARIO";
        const ROLE: &str = "REPOSYNC_HTTP_FIXED_PROBE_ROLE";
        const READY_FD: &str = "REPOSYNC_HTTP_FIXED_PROBE_READY_FD";
        const RELEASE_FD: &str = "REPOSYNC_HTTP_FIXED_PROBE_RELEASE_FD";

        #[derive(Clone, Copy, PartialEq, Eq)]
        enum Scenario { LeaderExits, Nonzero, MalformedReady, MissingReady, HelperHolds }

        impl Scenario {
            fn name(self) -> &'static str {
                match self {
                    Self::LeaderExits => "leader-exits-child-holds",
                    Self::Nonzero => "nonzero-before-stop",
                    Self::MalformedReady => "malformed-ready",
                    Self::MissingReady => "missing-ready",
                    Self::HelperHolds => "helper-holds",
                }
            }

            fn from_name(name: &str) -> Self {
                match name {
                    "leader-exits-child-holds" => Self::LeaderExits,
                    "nonzero-before-stop" => Self::Nonzero,
                    "malformed-ready" => Self::MalformedReady,
                    "missing-ready" => Self::MissingReady,
                    "helper-holds" => Self::HelperHolds,
                    _ => panic!("unknown fixed native probe scenario"),
                }
            }
        }

        fn module_name() -> &'static str {
            module_path!().split_once("::").expect("crate module prefix").1
        }

        fn require_native() {
            assert_eq!(std::env::var("REPOSYNC_FIXTURE_EXEC_MODE").ok().as_deref(),
                Some("native-generated-v1"), "fixed native probe needs explicit native mode");
            assert!(std::env::var_os("REPOSYNC_LAUNCH_BINDING_V1").is_none(),
                "fixed native probe refuses a packaged channel");
            // An unreaped Child is an identity anchor only under exclusive
            // wait ownership. Refuse auto-reaping dispositions; do not change
            // this process-wide signal policy to make the fixture admissible.
            let mut disposition = std::mem::MaybeUninit::<libc::sigaction>::zeroed();
            assert_eq!(unsafe { libc::sigaction(libc::SIGCHLD, std::ptr::null(), disposition.as_mut_ptr()) }, 0);
            let disposition = unsafe { disposition.assume_init() };
            assert_eq!(disposition.sa_sigaction, libc::SIG_DFL,
                "native group lifetime needs default SIGCHLD and no other waiter");
            assert_eq!(disposition.sa_flags & libc::SA_NOCLDWAIT, 0,
                "native group lifetime refuses automatic child reaping");
            // Enforce the existing inherited-case-pin refusal in every entry,
            // including the isolated body/ignored fixture, before its effects.
            let command = Command::new(std::env::current_exe().unwrap());
            admit_child_environment(&command, ExplicitMode::NativeGenerated)
                .expect("fixed native probe refuses inherited packaged case pins");
        }

        // Expected failures retain production latches and resources. Give each
        // leaf its own native libtest process instead of clearing those latches.
        fn isolated(leaf: &str) -> bool {
            require_native();
            let full = format!("{}::{leaf}", module_name());
            if std::env::var(UNIT_CHILD).ok().as_deref() == Some(full.as_str()) { return false; }
            let mut command = Command::new(std::env::current_exe().unwrap());
            command.args(["--exact", full.as_str(), "--nocapture", "--test-threads=1",
                "--format", "pretty", "--color", "never"])
                .env(UNIT_CHILD, &full);
            admit_child_environment(&command, ExplicitMode::NativeGenerated).unwrap();
            let scratch = tempfile::Builder::new().prefix("rs-http-child-failure-").tempdir().unwrap();
            command.env("TMPDIR", scratch.path()).env("TMP", scratch.path()).env("TEMP", scratch.path());
            let output = match command.output() {
                Ok(output) => output,
                Err(error) => {
                    let path = scratch.path().to_owned(); std::mem::forget(scratch);
                    panic!("native child output observation failed (spawn/read/wait stage unknown): {error}; scratch={path:?}");
                }
            };
            let stdout = String::from_utf8_lossy(&output.stdout);
            let passed = format!("test {full} ... ok");
            if !output.status.success()
                || stdout.lines().filter(|line| *line == passed.as_str()).count() != 1
                || !stdout.contains("1 passed; 0 failed; 0 ignored;") {
                let path = scratch.path().to_owned(); std::mem::forget(scratch);
                panic!("native failure child failed or selected zero/ignored tests; scratch={path:?}; stdout={stdout}; stderr={}",
                    String::from_utf8_lossy(&output.stderr));
            }
            if !output.stderr.is_empty() { eprint!("{}", String::from_utf8_lossy(&output.stderr)); }
            true
        }

        #[derive(Clone, Copy, PartialEq, Eq)]
        enum ProbeRole { Primary, Descendant, Anchor }

        // Only the current executable, one ignored leaf and fixed IPC roles
        // are expressible. Anchor is a separate, exclusively owned group leader;
        // it is never polled/reaped until all signal-capable calls have ended.
        fn fixed_command(scenario: Scenario, role: ProbeRole, ready_fd: RawFd,
            release_fd: Option<RawFd>) -> Command {
            require_native();
            assert!(ready_fd > 2);
            assert_eq!(release_fd.is_some(), role == ProbeRole::Descendant);
            if let Some(fd) = release_fd { assert!(fd > 2 && fd != ready_fd); }
            let fixture = format!("{}::fixed_native_probe", module_name());
            let mut command = Command::new(std::env::current_exe().unwrap());
            command.args(["--exact", fixture.as_str(), "--ignored", "--nocapture",
                "--test-threads=1", "--format", "pretty", "--color", "never"])
                .env(PROBE, &fixture).env(SCENARIO, scenario.name())
                .env(ROLE, match role { ProbeRole::Primary => "primary",
                    ProbeRole::Descendant => "descendant", ProbeRole::Anchor => "anchor" })
                .env(READY_FD, ready_fd.to_string()).env_remove(RELEASE_FD);
            if let Some(fd) = release_fd { command.env(RELEASE_FD, fd.to_string()); }
            admit_child_environment(&command, ExplicitMode::NativeGenerated).unwrap();
            unsafe {
                command.pre_exec(move || {
                    for fd in [Some(ready_fd), release_fd].into_iter().flatten() {
                        let flags = libc::fcntl(fd, libc::F_GETFD);
                        if flags < 0 { return Err(std::io::Error::last_os_error()); }
                        if libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                            return Err(std::io::Error::last_os_error());
                        }
                    }
                    Ok(())
                });
            }
            command
        }

        fn read_control(stream: &mut UnixStream) -> serde_json::Value {
            let mut line = Vec::new();
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            loop {
                let remaining = deadline.checked_duration_since(std::time::Instant::now())
                    .expect("fixed probe control deadline");
                assert!(!remaining.is_zero()); stream.set_read_timeout(Some(remaining)).unwrap();
                let mut byte = [0]; stream.read_exact(&mut byte).unwrap();
                assert!(std::time::Instant::now() < deadline, "control arrived late");
                line.push(byte[0]); assert!(line.len() <= 512);
                if byte[0] == b'\n' { break; }
            }
            stream.set_read_timeout(None).unwrap(); serde_json::from_slice(&line).unwrap()
        }

        fn send_control(stream: &mut UnixStream, descendant: Option<u32>) {
            let line = serde_json::json!({"pid": std::process::id(),
                "parent_pid": unsafe { libc::getppid() },
                "group": unsafe { libc::getpgrp() }, "descendant_pid": descendant});
            writeln!(stream, "{line}").unwrap(); stream.flush().unwrap();
        }

        unsafe fn inherited_socket(fd: RawFd) -> UnixStream {
            assert!(fd > 2);
            let stream = unsafe { UnixStream::from_raw_fd(fd) };
            stream.peer_addr().unwrap();
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) }; assert!(flags >= 0);
            assert_eq!(unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) }, 0);
            stream
        }

        fn expect_byte(stream: &mut UnixStream, byte: u8) {
            stream.set_read_timeout(Some(Duration::from_secs(15))).unwrap();
            let mut got = [0]; stream.read_exact(&mut got).expect("fixed owned IPC control");
            assert_eq!(got, [byte]);
        }

        #[test]
        #[ignore = "fixed native child fixture; not an independently passing obligation"]
        fn fixed_native_probe() {
            require_native();
            assert_eq!(std::env::var(PROBE).unwrap(), format!("{}::fixed_native_probe", module_name()));
            let scenario = Scenario::from_name(&std::env::var(SCENARIO).unwrap());
            let role = std::env::var(ROLE).unwrap();
            assert!(role == "primary" || role == "descendant" || role == "anchor");
            let fd: RawFd = std::env::var(READY_FD).unwrap().parse().unwrap();
            let mut ready = unsafe { inherited_socket(fd) };
            if role == "anchor" {
                assert!(scenario == Scenario::HelperHolds);
                assert!(std::env::var_os(RELEASE_FD).is_none());
                send_control(&mut ready, None); expect_byte(&mut ready, b'Q'); return;
            }
            if role == "descendant" {
                assert!(scenario == Scenario::LeaderExits);
                let release_fd: RawFd = std::env::var(RELEASE_FD).unwrap().parse().unwrap();
                assert_ne!(release_fd, fd);
                let mut release = unsafe { inherited_socket(release_fd) };
                send_control(&mut ready, None); expect_byte(&mut release, b'Q'); return;
            }
            assert!(std::env::var_os(RELEASE_FD).is_none());
            // Pause before scenario actions while the parent installs actual
            // group-leader lifetime custody where legacy stop calls need it.
            send_control(&mut ready, None); expect_byte(&mut ready, b'S');
            if scenario == Scenario::LeaderExits {
                let (mut control, writer) = UnixStream::pair().unwrap();
                let mut command = fixed_command(scenario, ProbeRole::Descendant,
                    writer.as_raw_fd(), Some(ready.as_raw_fd()));
                command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::inherit());
                let mut child = command.spawn().unwrap(); drop(writer);
                let started = read_control(&mut control);
                assert_eq!(started["pid"].as_u64(), Some(u64::from(child.id())));
                assert_eq!(started["parent_pid"].as_i64(), Some(i64::from(std::process::id())));
                assert_eq!(started["group"].as_i64(), Some(i64::from(unsafe { libc::getpgrp() })));
                assert!(child.try_wait().unwrap().is_none());
                send_control(&mut ready, Some(child.id()));
                std::mem::forget(child); std::process::exit(0);
            }
            send_control(&mut ready, None);
            match scenario {
                Scenario::Nonzero => std::process::exit(23),
                Scenario::MalformedReady => ready.write_all(b"not-json\n").unwrap(),
                Scenario::MissingReady => ready.shutdown(std::net::Shutdown::Write).unwrap(),
                Scenario::HelperHolds => {}
                Scenario::LeaderExits => unreachable!(),
            }
            expect_byte(&mut ready, b'Q');
        }

        #[derive(Clone, Copy, PartialEq, Eq)]
        enum AnchorPhase { Unreaped, Reaping, Reaped }

        struct GroupLifetimeAnchor {
            context: crate::process::CaseContextId,
            group: i32,
            child: std::process::Child,
            release: UnixStream,
            phase: AnchorPhase,
            reaped: Option<std::process::ExitStatus>,
            authority_error: Option<String>,
            case_error: Option<String>,
        }

        impl GroupLifetimeAnchor {
            fn authorize(&self, context: crate::process::CaseContextId, group: i32) -> Result<(), String> {
                // Actual exclusive unreaped group-LEADER Child is the pin. No
                // procfs/getpgid/kill0 observation or numeric equality grants it.
                if self.context != context || self.group != group || group <= 0
                    || self.child.id() != u32::try_from(group).unwrap_or(0)
                    || self.phase != AnchorPhase::Unreaped || self.reaped.is_some()
                    || self.authority_error.is_some() {
                    return Err("unreaped group-leader anchor authority is absent or revoked".into());
                }
                Ok(())
            }

            fn poll_reap(&mut self) -> Result<Option<std::process::ExitStatus>, String> {
                if let Some(status) = self.reaped { return Ok(Some(status)); }
                // Irrevocably revoke signalling BEFORE any potentially reaping
                // operation, even if it errors, blocks or returns no status.
                self.phase = AnchorPhase::Reaping;
                match self.child.try_wait() {
                    Ok(Some(status)) => { self.reaped = Some(status); self.phase = AnchorPhase::Reaped; Ok(Some(status)) }
                    Ok(None) => Ok(None),
                    Err(error) => {
                        let error = format!("anchor wait unknown: {error}");
                        self.authority_error = Some(error.clone()); Err(error)
                    }
                }
            }
        }

        type AnchorHandle = crate::process::CaseResourceHandle<GroupLifetimeAnchor>;
        #[derive(Clone, Copy)]
        struct CheckedStopRecord { context: crate::process::CaseContextId, role: super::super::HttpChildRole }
        #[derive(Clone, Copy)]
        enum LegacyPhase { Open, CheckedStop(CheckedStopRecord), Ended }
        enum PrimaryCollection { Reaped(std::process::ExitStatus), CheckedStop(CheckedStopRecord), NotSpawned }
        struct ContainmentRecord { primary: PrimaryCollection, anchor: Option<std::process::ExitStatus> }

        struct Probe {
            handle: Option<ChildHandle>,
            context: crate::process::CaseContextId,
            group: i32, // diagnostics/observation only; never raw collector signal authority
            release: Option<UnixStream>,
            anchor: Option<AnchorHandle>,
            legacy: LegacyPhase,
            signal_capable_dispatches: usize,
            authority_loss: Option<String>,
            collection_attempted: bool,
            containment: Option<ContainmentRecord>,
        }

        impl Probe {
            fn primary(&self) -> ChildHandle { self.handle.expect("registered primary") }

            fn record_error(&mut self, error: &str) {
                crate::process::retain_case_context(self.context);
                if let Some(anchor) = self.anchor {
                    let _ = crate::process::with_case_resource(anchor, |slot| slot.case_error = Some(error.to_string()));
                }
            }

            fn require_signal_authority(&self, claimed_group: i32) -> Result<(), String> {
                if claimed_group != self.group || self.containment.is_some() || self.collection_attempted
                    || !matches!(self.legacy, LegacyPhase::Open) || self.authority_loss.is_some() {
                    return Err("physical completion or retained failure prohibits another group stop".into());
                }
                let anchor = self.anchor.ok_or_else(|| "no unreaped group-leader anchor".to_string())?;
                crate::process::with_case_resource(anchor, |slot| slot.authorize(self.context, claimed_group))?
            }

            fn checked_stop_for_claim(&mut self, claimed_group: i32,
                deadline: Option<std::time::Instant>) -> Result<super::super::CheckedHttpChild, String> {
                self.require_signal_authority(claimed_group)?;
                self.signal_capable_dispatches += 1;
                // Only this caller can reap the anchor, and collection cannot
                // start until this synchronous call returns. Anchor stays in
                // its registered, exclusive Unreaped state through ALL legacy
                // waits/TERM/KILL, including a primary reaped by request_stop.
                let result = match deadline {
                    Some(deadline) => super::super::stop_http_child_until(self.primary(), deadline),
                    None => super::super::stop_http_child(self.primary()),
                };
                match &result {
                    Ok(proof) => {
                        assert_eq!(proof.context, self.context);
                        self.legacy = LegacyPhase::CheckedStop(CheckedStopRecord { context: proof.context, role: proof.role });
                    }
                    Err(error) => { self.legacy = LegacyPhase::Ended; self.record_error(error); }
                }
                result
            }

            fn observe_certificate_until(&mut self, deadline: std::time::Instant) -> Result<(), String> {
                self.require_signal_authority(self.group)?;
                self.signal_capable_dispatches += 1;
                let result = super::super::observe_case_tool_until(self.primary(), deadline);
                self.legacy = LegacyPhase::Ended;
                if let Err(error) = &result { self.record_error(error); }
                result
            }

            fn wait_for_exit(&mut self) -> std::process::ExitStatus {
                let deadline = std::time::Instant::now() + Duration::from_secs(3);
                loop {
                    assert!(std::time::Instant::now() < deadline);
                    let status = crate::process::with_case_resource(self.primary(), |child| child.child.try_wait())
                        .unwrap().unwrap();
                    assert!(std::time::Instant::now() < deadline, "primary wait arrived late");
                    if let Some(status) = status { return status; }
                    std::thread::sleep(Duration::from_millis(5));
                }
            }

            fn reap_anchor(&mut self) -> Result<Option<std::process::ExitStatus>, String> {
                let Some(anchor) = self.anchor else { return Ok(None); };
                // A socket release is bound to the original endpoint, not an
                // integer process identity. No group signal occurs here.
                let _ = crate::process::with_case_resource(anchor, |slot| slot.release.write_all(b"Q"))?;
                let deadline = std::time::Instant::now() + Duration::from_secs(3);
                loop {
                    if std::time::Instant::now() >= deadline { return Err("anchor collection unknown".into()); }
                    let status = crate::process::with_case_resource(anchor, |slot| slot.poll_reap())??;
                    if std::time::Instant::now() >= deadline { return Err("anchor collection arrived late".into()); }
                    if let Some(status) = status { return Ok(Some(status)); }
                    std::thread::sleep(Duration::from_millis(5));
                }
            }

            // This collector has NO group/direct kill or legacy stop path.
            // Cooperative original IPC + actual Child waits + group observation
            // may establish physical containment; no qualification/error/root
            // token is released. If cooperation/observation fails, retain
            // UNKNOWN and leave further containment to admitted outer owners.
            fn contain_without_group_signals(&mut self) -> Result<(), String> {
                if self.containment.is_some() { return Ok(()); }
                if self.collection_attempted { return Err("prior physical containment remains unknown".into()); }
                self.collection_attempted = true;
                if let Some(release) = self.release.as_mut() { let _ = release.write_all(b"Q"); }
                let primary = if let LegacyPhase::CheckedStop(record) = self.legacy {
                    PrimaryCollection::CheckedStop(record)
                } else if self.handle.is_some() {
                    let deadline = std::time::Instant::now() + Duration::from_secs(3);
                    loop {
                        if std::time::Instant::now() >= deadline { return Err("primary containment unknown".into()); }
                        let status = crate::process::with_case_resource(self.primary(), |slot| slot.child.try_wait())?
                            .map_err(|error| format!("primary wait unknown: {error}"))?;
                        if std::time::Instant::now() >= deadline { return Err("primary containment arrived late".into()); }
                        if let Some(status) = status { break PrimaryCollection::Reaped(status); }
                        std::thread::sleep(Duration::from_millis(5));
                    }
                } else { PrimaryCollection::NotSpawned };
                let anchor = self.reap_anchor()?;
                if self.group <= 0 { return Err("group identity unavailable; no unsafe signal attempted".into()); }
                let deadline = std::time::Instant::now() + Duration::from_secs(3);
                loop {
                    if std::time::Instant::now() >= deadline { return Err("group containment unknown".into()); }
                    let quiet = crate::process::owned_group_quiescent(self.group)
                        .map_err(|error| format!("group observation unknown: {error}"))?;
                    if std::time::Instant::now() >= deadline { return Err("group containment arrived late".into()); }
                    if quiet { self.containment = Some(ContainmentRecord { primary, anchor }); return Ok(()); }
                    std::thread::sleep(Duration::from_millis(5));
                }
            }

            fn cleanup(&mut self) -> Result<(), String> {
                if let Some(error) = &self.authority_loss { return Err(error.clone()); }
                let result = self.contain_without_group_signals();
                if let Err(error) = &result { self.record_error(error); }
                result
            }

            fn revoke_anchor_for_test(&mut self, error: &str) {
                self.authority_loss = Some(error.to_string()); self.record_error(error);
                let anchor = self.anchor.expect("real anchor");
                crate::process::with_case_resource(anchor, |slot| slot.authority_error = Some(error.to_string())).unwrap();
            }

            fn retire_anchor_for_test(&mut self) {
                assert!(self.reap_anchor().unwrap().is_some(), "actual anchor must be reaped");
                // Reaped OS lifetime itself must refuse, independently of the
                // error latch installed below. A copied numeric ID is no pin.
                crate::process::with_case_resource(self.anchor.unwrap(), |slot| {
                    assert!(slot.phase == AnchorPhase::Reaped && slot.reaped.is_some());
                    assert!(slot.authority_error.is_none());
                    assert!(slot.authorize(self.context, self.group).is_err());
                }).unwrap();
                self.revoke_anchor_for_test("actual group-leader anchor was retired; numeric claim is not authority");
            }

            fn assert_contained(&self) {
                let record = self.containment.as_ref().expect("actual physical observations");
                match &record.primary {
                    PrimaryCollection::CheckedStop(stopped) => {
                        assert_eq!(stopped.context, self.context);
                        assert!(stopped.role == super::super::HttpChildRole::CertificateTool
                            || stopped.role == super::super::HttpChildRole::Server);
                    }
                    PrimaryCollection::Reaped(status) => { let _ = status.code(); }
                    PrimaryCollection::NotSpawned => panic!("no primary in completed test"),
                }
                if self.anchor.is_some() { assert!(record.anchor.is_some()); }
            }
        }

        impl Drop for Probe {
            fn drop(&mut self) {
                if self.containment.is_none() && !self.collection_attempted {
                    if let Err(error) = self.contain_without_group_signals() {
                        self.record_error(&error);
                        eprintln!("UNKNOWN native fixture containment: {error}; group={} (no signal authorized)", self.group);
                        if !std::thread::panicking() { panic!("{error}"); }
                    }
                }
            }
        }

        fn spawn_probe(context: crate::process::CaseContextId, role: super::super::HttpChildRole,
            scenario: Scenario) -> (Probe, UnixStream, serde_json::Value, std::sync::Weak<tempfile::TempDir>) {
            require_native();
            let root = std::sync::Arc::new(tempfile::tempdir().unwrap()); let weak = std::sync::Arc::downgrade(&root);
            let retained = std::sync::Arc::clone(&root);
            crate::process::register_case_resource(context, || Ok(super::super::CaseHttpRoot { _root: retained })).unwrap();
            let log = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600)
                .custom_flags(libc::O_NOFOLLOW).open(root.path().join("fixed-probe.log")).unwrap();
            let mut probe = Probe { handle: None, context, group: 0, release: None, anchor: None,
                legacy: LegacyPhase::Open, signal_capable_dispatches: 0, authority_loss: None,
                collection_attempted: false, containment: None };
            if scenario != Scenario::LeaderExits {
                let (mut control, writer) = UnixStream::pair().unwrap();
                let release = control.try_clone().unwrap(); release.set_write_timeout(Some(Duration::from_millis(50))).unwrap();
                let mut command = fixed_command(Scenario::HelperHolds, ProbeRole::Anchor, writer.as_raw_fd(), None);
                command.process_group(0).stdin(Stdio::null()).stdout(log.try_clone().unwrap()).stderr(log.try_clone().unwrap());
                let mut group = 0;
                let anchor = crate::process::register_case_resource(context, || {
                    let child = command.spawn().map_err(|error| format!("anchor spawn: {error}"))?;
                    group = i32::try_from(child.id()).unwrap_or(0);
                    Ok(GroupLifetimeAnchor { context, group, child, release, phase: AnchorPhase::Unreaped,
                        reaped: None, authority_error: None, case_error: None })
                }).unwrap();
                probe.anchor = Some(anchor); probe.group = group; drop(writer);
                assert!(group > 0); let hello = read_control(&mut control);
                assert_eq!(hello["pid"].as_u64(), Some(u64::try_from(group).unwrap()));
                assert_eq!(hello["group"].as_i64(), Some(i64::from(group)));
            }
            let (mut control, writer) = UnixStream::pair().unwrap();
            let release = control.try_clone().unwrap(); release.set_write_timeout(Some(Duration::from_millis(50))).unwrap();
            let mut command = fixed_command(scenario, ProbeRole::Primary, writer.as_raw_fd(), None);
            command.process_group(probe.group).stdin(Stdio::null()).stdout(log.try_clone().unwrap()).stderr(log);
            let retained = std::sync::Arc::clone(&root); let mut pid = 0; let mut group = probe.group;
            let handle = crate::process::register_case_resource(context, || {
                let child = command.spawn().map_err(|error| format!("primary spawn: {error}"))?; pid = child.id();
                if group == 0 { group = i32::try_from(pid).unwrap_or(0); }
                Ok(super::super::CaseHttpChild { context, role, child, group, _root: retained,
                    stopping: None, killed: false, status: None, error: None })
            }).unwrap();
            probe.handle = Some(handle); probe.group = group; probe.release = Some(release);
            drop(writer); drop(root); assert!(group > 0);
            let hello = read_control(&mut control);
            assert_eq!(hello["pid"].as_u64(), Some(u64::from(pid)));
            assert_eq!(hello["group"].as_i64(), Some(i64::from(group)));
            if let Some(anchor) = probe.anchor {
                // Only release execution, NEVER reap. Even an exited zombie
                // group-leader Child pins its PID until our last stop returns.
                crate::process::with_case_resource(anchor, |slot| slot.release.write_all(b"Q")).unwrap().unwrap();
            }
            probe.release.as_mut().unwrap().write_all(b"S").unwrap();
            let hello = read_control(&mut control);
            assert_eq!(hello["pid"].as_u64(), Some(u64::from(pid)));
            assert_eq!(hello["group"].as_i64(), Some(i64::from(group)));
            (probe, control, hello, weak)
        }

        fn retained(context: crate::process::CaseContextId, weak: &std::sync::Weak<tempfile::TempDir>) {
            assert!(weak.upgrade().is_some(), "case root lost while child/observation is unresolved");
            assert!(!crate::process::case_context_ready(context));
            assert!(crate::process::release_case_context(context, true).is_err());
            assert!(crate::process::admit_case_context().is_err());
        }

        #[test]
        fn http_l4_reaped_leader_with_live_descendant_refuses_completion() {
            if isolated("http_l4_reaped_leader_with_live_descendant_refuses_completion") { return; }
            let boundary = enter_process_boundary().unwrap(); let context = boundary.context();
            let (mut probe, _ready, hello, weak) = spawn_probe(context,
                super::super::HttpChildRole::Server, Scenario::LeaderExits);
            let descendant = i32::try_from(hello["descendant_pid"].as_u64().unwrap()).unwrap();
            assert_eq!(unsafe { libc::getpgid(descendant) }, probe.group, "descendant escaped its owned group");
            assert!(probe.wait_for_exit().success(), "leader must really be reaped");
            assert!(!crate::process::owned_group_quiescent(probe.group).unwrap());
            let observed = crate::process::with_case_resource(probe.primary(), |child| child.observe_exit())
                .unwrap().unwrap();
            assert!(observed.is_none(), "reaped leader falsely qualified a live descendant");
            retained(context, &weak); probe.cleanup().unwrap(); retained(context, &weak);
        }

        #[test]
        fn http_l4_expired_cleanup_deadline_retains_child_and_root() {
            if isolated("http_l4_expired_cleanup_deadline_retains_child_and_root") { return; }
            let boundary = enter_process_boundary().unwrap(); let context = boundary.context();
            let (mut probe, _ready, _hello, weak) = spawn_probe(context,
                super::super::HttpChildRole::Server, Scenario::HelperHolds);
            let error = probe.checked_stop_for_claim(probe.group, Some(std::time::Instant::now()))
                .map(|_| ()).unwrap_err();
            assert!(error.contains("budget expired"));
            assert_eq!(crate::process::case_resource_handles::<super::super::CaseHttpChild>(context).len(), 1);
            retained(context, &weak); probe.cleanup().unwrap(); retained(context, &weak);
        }

        #[test]
        fn http_l5_nonzero_before_stop_preserves_actual_failure() {
            if isolated("http_l5_nonzero_before_stop_preserves_actual_failure") { return; }
            let boundary = enter_process_boundary().unwrap(); let context = boundary.context();
            let (mut probe, _ready, _hello, weak) = spawn_probe(context,
                super::super::HttpChildRole::Server, Scenario::Nonzero);
            assert_eq!(probe.wait_for_exit().code(), Some(23));
            let error = probe.checked_stop_for_claim(probe.group, None).map(|_| ()).unwrap_err();
            assert!(error.contains("exited before stop"));
            crate::process::with_case_resource(probe.primary(), |child| {
                assert_eq!(child.status.and_then(|status| status.code()), Some(23));
                assert!(child.error.is_some());
            }).unwrap();
            retained(context, &weak); probe.cleanup().unwrap();
        }

        fn refused_readiness(scenario: Scenario) {
            let boundary = enter_process_boundary().unwrap(); let context = boundary.context();
            let (mut probe, ready, _hello, weak) = spawn_probe(context,
                super::super::HttpChildRole::Server, scenario);
            let line = super::super::read_case_http_readiness(context, ready, Duration::from_secs(2)).unwrap();
            if scenario == Scenario::MissingReady { assert!(line.is_empty()); }
            else { assert_eq!(line, "not-json\n"); }
            assert!(super::super::parse_http_readiness(&line).unwrap_err().contains("readiness JSON"));
            assert_eq!(crate::process::case_resource_handles::<super::super::CaseHttpChild>(context).len(), 1);
            retained(context, &weak); probe.cleanup().unwrap(); retained(context, &weak);
        }

        #[test]
        fn http_l6_malformed_readiness_retains_server_and_root() {
            if isolated("http_l6_malformed_readiness_retains_server_and_root") { return; }
            refused_readiness(Scenario::MalformedReady);
        }

        #[test]
        fn http_l6_missing_readiness_retains_server_and_root() {
            if isolated("http_l6_missing_readiness_retains_server_and_root") { return; }
            refused_readiness(Scenario::MissingReady);
        }

        #[test]
        fn http_l6_readiness_timeout_keeps_actual_reader_and_child() {
            if isolated("http_l6_readiness_timeout_keeps_actual_reader_and_child") { return; }
            let boundary = enter_process_boundary().unwrap(); let context = boundary.context();
            let (mut probe, ready, _hello, weak) = spawn_probe(context,
                super::super::HttpChildRole::Server, Scenario::HelperHolds);
            let error = super::super::read_case_http_readiness(context, ready, Duration::from_millis(50))
                .unwrap_err();
            assert!(error.contains("timed out"));
            let readers = crate::process::case_resource_handles::<super::super::CaseHttpThread<Result<String, String>>>(context);
            assert_eq!(readers.len(), 1, "timed-out actual reader must remain registered");
            crate::process::with_case_resource(readers[0], |slot| {
                assert!(slot.handle.is_some()); assert!(slot.ready.is_none());
            }).unwrap();
            retained(context, &weak); probe.cleanup().unwrap();
            // Group cleanup unblocks the pipe but cannot reconstruct the
            // abandoned readiness assertion or release its retained result.
            super::super::observe_http_threads::<Result<String, String>>(context,
                std::time::Instant::now() + Duration::from_secs(2)).unwrap();
            assert_eq!(crate::process::case_resource_handles::<super::super::CaseHttpThread<Result<String, String>>>(context).len(), 1);
            crate::process::with_case_resource(readers[0], |slot| {
                assert!(slot.handle.is_none(), "physically joined reader handle was not consumed");
                assert!(matches!(slot.ready.as_ref(), Some(Ok(line)) if line.is_empty()),
                    "late EOF result was lost or converted into a readiness assertion");
            }).unwrap();
            retained(context, &weak);
        }

        #[test]
        fn http_l6_certificate_role_nonzero_keeps_failure_and_root() {
            if isolated("http_l6_certificate_role_nonzero_keeps_failure_and_root") { return; }
            let boundary = enter_process_boundary().unwrap(); let context = boundary.context();
            let (mut probe, _ready, _hello, weak) = spawn_probe(context,
                super::super::HttpChildRole::CertificateTool, Scenario::Nonzero);
            assert_eq!(probe.wait_for_exit().code(), Some(23));
            let error = probe.observe_certificate_until(std::time::Instant::now() + Duration::from_secs(2)).unwrap_err();
            assert!(error.contains("HTTP certificate tool failed"));
            assert_eq!(crate::process::case_resource_handles::<super::super::CaseHttpChild>(context).len(), 1);
            retained(context, &weak); probe.cleanup().unwrap();
        }

        #[test]
        fn http_l7_certificate_child_cannot_supply_server_completion() {
            if isolated("http_l7_certificate_child_cannot_supply_server_completion") { return; }
            let boundary = enter_process_boundary().unwrap(); let context = boundary.context();
            let (mut probe, _ready, _hello, weak) = spawn_probe(context,
                super::super::HttpChildRole::CertificateTool, Scenario::HelperHolds);
            let proof = probe.checked_stop_for_claim(probe.group, None).unwrap();
            assert_eq!(proof.context, context);
            let error = super::super::HttpFixture::check_server_completion(proof).map(|_| ()).unwrap_err();
            assert!(error.contains("different child role")); probe.record_error(&error);
            let dispatches = probe.signal_capable_dispatches;
            assert!(probe.require_signal_authority(probe.group).is_err(), "checked stop already ended signal authority");
            retained(context, &weak); probe.cleanup().unwrap(); probe.assert_contained();
            assert_eq!(probe.signal_capable_dispatches, dispatches, "checked stop must not be dispatched again");
            assert!(probe.checked_stop_for_claim(probe.group, None).is_err());
            assert_eq!(probe.signal_capable_dispatches, dispatches, "physical completion cannot reauthorize a stop");
            retained(context, &weak);
        }

        #[test]
        fn http_l4_reaped_anchor_number_cannot_authorize_replacement_claim() {
            if isolated("http_l4_reaped_anchor_number_cannot_authorize_replacement_claim") { return; }
            let boundary = enter_process_boundary().unwrap(); let context = boundary.context();
            let (mut probe, _ready, _hello, weak) = spawn_probe(context,
                super::super::HttpChildRole::Server, Scenario::Nonzero);
            assert_eq!(probe.wait_for_exit().code(), Some(23));
            let old_numeric_group = probe.group;
            probe.retire_anchor_for_test(); // actual exit/reap, no ID-reuse process probe
            let dispatches = probe.signal_capable_dispatches;
            assert!(probe.checked_stop_for_claim(old_numeric_group, None).is_err());
            let replacement_numeric_claim = old_numeric_group;
            assert!(probe.checked_stop_for_claim(replacement_numeric_claim, None).is_err());
            assert_eq!(probe.signal_capable_dispatches, dispatches);
            assert!(probe.cleanup().is_err()); retained(context, &weak);
            probe.contain_without_group_signals().unwrap(); probe.assert_contained();
            assert!(probe.cleanup().is_err(), "retired authority must not turn into cleanup success");
            assert_eq!(probe.signal_capable_dispatches, dispatches); retained(context, &weak);
        }

        #[test]
        fn http_l4_revoked_anchor_retains_error_root_and_context() {
            if isolated("http_l4_revoked_anchor_retains_error_root_and_context") { return; }
            let boundary = enter_process_boundary().unwrap(); let context = boundary.context();
            let (mut probe, _ready, _hello, weak) = spawn_probe(context,
                super::super::HttpChildRole::Server, Scenario::HelperHolds);
            probe.revoke_anchor_for_test("controlled loss of validated lifetime authority");
            let anchor = probe.anchor.unwrap();
            let dispatches = probe.signal_capable_dispatches;
            assert!(probe.checked_stop_for_claim(probe.group, None).is_err());
            assert!(probe.cleanup().unwrap_err().contains("controlled loss"));
            retained(context, &weak);
            // Original IPC/Child owners can collect the fixed actors without
            // a numeric group signal. That does not clear the failed case.
            probe.contain_without_group_signals().unwrap(); probe.assert_contained();
            crate::process::with_case_resource(anchor, |slot| {
                assert!(slot.reaped.is_some()); assert!(slot.authority_error.is_some()); assert!(slot.case_error.is_some());
            }).unwrap();
            assert!(probe.cleanup().is_err());
            assert_eq!(probe.signal_capable_dispatches, dispatches); retained(context, &weak);
        }
    }

    impl PreparedRepo {
        pub fn seal(&mut self) -> Result<(), String> {
            if self.explicitly_completed { return Err("HTTP session was already sealed".into()); }
            duties_allow_release(
                self.context,
                self.publication.as_ref(),
                self.blocking.as_ref(),
                self.server.as_ref(),
            )?;
            crate::process::close_case_registration(self.context)?;
            if !crate::process::case_context_ready(self.context)
                || crate::process::retained_ownership_incomplete() {
                return Err(
                    "unconfirmed completion retains context and blocks qualification".to_string(),
                );
            }
            self.release_owned_resources();
            self.explicitly_completed = true;
            Ok(())
        }

        fn release_owned_resources(&mut self) {
            self.env.restore();
            let mut seen = [-1i32; 4];
            let mut count = 0usize;
            for fd in [
                &mut self.case_fd,
                &mut self.repo_fd,
                &mut self.config_fd,
                &mut self.marker_fd,
            ] {
                if *fd >= 0 && !seen[..count].contains(fd) {
                    unsafe { libc::close(*fd) };
                    seen[count] = *fd;
                    count += 1;
                }
                *fd = -1;
            }
        }

        pub(super) fn record_publication_joined(
            &mut self,
            proof: super::CheckedHttpPublication,
        ) -> Result<(), String> {
            if proof.join.context != self.context || self.publication.is_some() {
                return Err("HTTP publication join changed owner or was already recorded".into());
            }
            self.publication = Some(proof);
            Ok(())
        }

        pub fn record_blocking_joined(&mut self) {
            // This HTTP path performs its synchronous libgit work inside the
            // same actually joined publication thread. This is not proof for
            // execution.rs work: live/unresolved case slots still block seal.
            self.blocking = self.publication.as_ref()
                .filter(|proof| proof.join.context == self.context)
                .map(|_| super::CheckedHttpBlocking { context: self.context });
        }

        pub(super) fn record_server_completed(
            &mut self,
            proof: super::CheckedHttpServer,
        ) -> Result<(), String> {
            if proof.context != self.context || self.server.is_some() {
                return Err("HTTP server completion changed owner or was already recorded".into());
            }
            self.server = Some(proof);
            Ok(())
        }

        pub(super) fn close_registration(&self) -> Result<(), String> {
            crate::process::close_case_registration(self.context)
        }

        pub fn install_admitted_hook(&mut self) -> Result<(), String> {
            let admitted = ADMITTED_HOOKS
                .iter()
                .find(|row| row.entry_id == self.entry_id)
                .ok_or_else(|| "this entry has no admitted hook slot".to_string())?;
            let hooks = open_hook_parent(self.repo_fd)?;
            let installed = (|| {
                if fstatat_nofollow(hooks, admitted.slot)?.is_some() {
                    return Err("hook slot already exists; stale slot was not overwritten".to_string());
                }
                if !archive_relative_source(&self.selected_path) {
                    return Err(
                        "source_path is archive-relative, not an installed absolute path".to_string(),
                    );
                }
                if self.selected_installed_relative_path == admitted.slot
                    || !self.selected_installed_relative_path
                        .ends_with(&format!("/{}", admitted.slot))
                {
                    return Err(
                        "installed_relative_path is the catalogue path, not the hook slot".to_string(),
                    );
                }
                let target_fd = open_nofollow(Path::new(admitted.target), false)?;
                let target_st = fstat_fd(target_fd)?;
                close_fd(target_fd);
                if file_type(target_st.st_mode) != libc::S_IFREG
                    || target_st.st_nlink as u64 != 1
                    || target_st.st_uid != 0
                    || perm_bits(target_st.st_mode) != self.selected_installed_mode
                {
                    return Err(
                        "catalogue target is not a root-owned mode 0755 regular nlink-1 file"
                            .to_string(),
                    );
                }
                let (sha, length, mode, dev, ino) = catalogue_identity(admitted.target)?;
                if sha != self.selected_sha256
                    || length != self.selected_len
                    || mode != self.selected_installed_mode
                    || dev != target_st.st_dev as u64
                    || ino != target_st.st_ino as u64
                {
                    return Err(
                        "hook target bytes do not match the selected catalogue row".to_string(),
                    );
                }
                let target_c = c_name(admitted.target)?;
                let slot_c = c_name(admitted.slot)?;
                if unsafe { libc::symlinkat(target_c.as_ptr(), hooks, slot_c.as_ptr()) } < 0 {
                    return Err(last_os_error("symlinkat"));
                }
                let linked = readlink_at(hooks, admitted.slot)?;
                if linked != admitted.target {
                    return Err("hook symlink target does not match the admitted entry".to_string());
                }
                let identity_fd = open_nofollow(Path::new(admitted.target), false)?;
                let identity = fstat_fd(identity_fd)?;
                close_fd(identity_fd);
                if file_type(identity.st_mode) != libc::S_IFREG
                    || identity.st_nlink as u64 != 1
                    || identity.st_uid != 0
                    || perm_bits(identity.st_mode) != self.selected_installed_mode
                {
                    return Err(
                        "catalogue target identity is not a root-owned mode 0755 regular nlink-1 file"
                            .to_string(),
                    );
                }
                if identity.st_dev != target_st.st_dev || identity.st_ino != target_st.st_ino {
                    return Err(
                        "catalogue target changed between validation and link; link text is not object identity"
                            .to_string(),
                    );
                }
                let (sha_after, len_after, mode_after, dev_after, ino_after) =
                    catalogue_identity(admitted.target)?;
                if sha_after != sha
                    || len_after != length
                    || mode_after != mode
                    || dev_after != dev
                    || ino_after != ino
                {
                    return Err(
                        "catalogue target bytes changed between the anchored checks".to_string(),
                    );
                }
                let link_node = fstatat_nofollow(hooks, admitted.slot)?
                    .ok_or_else(|| "hook symlink node is missing after link".to_string())?;
                if file_type(link_node.st_mode) != libc::S_IFLNK {
                    return Err(
                        "hook slot text was installed but the node is not a symlink".to_string(),
                    );
                }
                Ok((
                    identity.st_dev as u64,
                    identity.st_ino as u64,
                    link_node.st_dev as u64,
                    link_node.st_ino as u64,
                ))
            })();
            close_fd(hooks);
            let (dev, ino, link_dev, link_ino) = installed?;
            self.hook_target = Some(admitted.target.to_string());
            self.hook_dev = Some(dev);
            self.hook_ino = Some(ino);
            self.hook_link_dev = Some(link_dev);
            self.hook_link_ino = Some(link_ino);
            Ok(())
        }

        pub fn recheck_hook(&self) -> Result<(), String> {
            let target = self
                .hook_target
                .as_deref()
                .ok_or_else(|| "hook was not installed".to_string())?;
            let expected_dev = self
                .hook_dev
                .ok_or_else(|| "hook object identity was not recorded".to_string())?;
            let expected_ino = self
                .hook_ino
                .ok_or_else(|| "hook object identity was not recorded".to_string())?;
            let admitted = ADMITTED_HOOKS
                .iter()
                .find(|row| row.entry_id == self.entry_id)
                .ok_or_else(|| "this entry has no admitted hook slot".to_string())?;
            let hooks = open_hook_parent_existing(self.repo_fd)?;
            let checked = (|| {
                let linked = readlink_at(hooks, admitted.slot)?;
                if linked != target || linked != admitted.target {
                    return Err("hook slot text does not name the admitted target".to_string());
                }
                let fd = open_nofollow(Path::new(admitted.target), false)?;
                let st = fstat_fd(fd)?;
                close_fd(fd);
                if file_type(st.st_mode) != libc::S_IFREG
                    || st.st_nlink as u64 != 1
                    || st.st_uid != 0
                    || perm_bits(st.st_mode) != self.selected_installed_mode
                {
                    return Err("hook target is not the admitted regular object".to_string());
                }
                let (sha, length, mode, _dev, _ino) = catalogue_identity(admitted.target)?;
                if sha != self.selected_sha256
                    || length != self.selected_len
                    || mode != self.selected_installed_mode
                {
                    return Err(
                        "hook target bytes do not match the selected catalogue row".to_string(),
                    );
                }
                if st.st_dev as u64 != expected_dev || st.st_ino as u64 != expected_ino {
                    return Err(
                        "hook link text matches but the object identity does not".to_string(),
                    );
                }
                let link_node = fstatat_nofollow(hooks, admitted.slot)?
                    .ok_or_else(|| "hook symlink node disappeared".to_string())?;
                if file_type(link_node.st_mode) != libc::S_IFLNK
                    || self.hook_link_dev != Some(link_node.st_dev as u64)
                    || self.hook_link_ino != Some(link_node.st_ino as u64)
                {
                    return Err("hook symlink node identity does not match the install".to_string());
                }
                Ok(())
            })();
            close_fd(hooks);
            checked
        }

        pub fn publish_marker(&self, name: &str, payload: &[u8], mode: u32) -> Result<(), String> {
            publish_nofollow(self.marker_fd, name, payload, mode)
        }

        pub fn release_token(&self) -> Result<Vec<u8>, String> {
            canonical_json(&[
                ("case_id", self.case_id.as_str()),
                ("entry_id", self.entry_id.as_str()),
                ("instance_id", self.instance_id.as_str()),
                ("manifest_sha256", self.manifest_sha256.as_str()),
            ])
        }

        pub fn entered_witness(&self) -> Result<Vec<u8>, String> {
            canonical_json(&[
                ("case_id", self.case_id.as_str()),
                ("data_sha256", self.data_sha256.as_str()),
                ("entry_id", self.entry_id.as_str()),
                ("instance_id", self.instance_id.as_str()),
                ("manifest_sha256", self.manifest_sha256.as_str()),
            ])
        }

        pub fn require_entered_witness(&self, marker: &str) -> Result<(), String> {
            let expected = self.entered_witness()?;
            self.require_marker(marker, &expected)?;
            let case_bytes = read_nofollow(self.config_fd, "execution-case-v1.json")?;
            let hashed = hex::encode(sha2::Sha256::digest(&case_bytes));
            if hashed != self.data_sha256 {
                return Err("case file bytes do not match the bound data_sha256".to_string());
            }
            let text = String::from_utf8(expected)
                .map_err(|_| "entered witness is not UTF-8".to_string())?;
            if !text.contains(&format!("\"instance_id\":\"{}\"", self.instance_id))
                || !text.contains(&format!("\"manifest_sha256\":\"{}\"", self.manifest_sha256))
                || !text.contains(&format!("\"data_sha256\":\"{}\"", self.data_sha256))
            {
                return Err(
                    "entered witness does not bind the nonce, H, and data bytes".to_string(),
                );
            }
            Ok(())
        }

        pub fn require_marker(&self, name: &str, expected: &[u8]) -> Result<(), String> {
            let body = read_nofollow(self.marker_fd, name)?;
            if body != expected {
                return Err(format!("marker {name} does not match the retained case"));
            }
            self.proved_markers
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .push(name.to_string());
            Ok(())
        }

        pub fn read_marker(&self, name: &str) -> Result<Vec<u8>, String> {
            read_nofollow(self.marker_fd, name)
        }

        pub fn push_f11_identity(&mut self) {
            self.env
                .push_utf8("REPOSYNC_EXEC_F11_IDENTITY_PROFILE", "reposync-test-v1");
            self.env
                .push_utf8("REPOSYNC_EXEC_F11_AUTH_KIND", "http-extraheader-v1");
            self.env
                .push_utf8("REPOSYNC_EXEC_F11_AUTH_SHA256", F11_AUTH_SHA256);
            self.env.push_utf8("GIT_AUTHOR_NAME", "RepoSync Test");
            self.env
                .push_utf8("GIT_AUTHOR_EMAIL", "reposync@localhost");
            self.env.push_utf8("GIT_COMMITTER_NAME", "RepoSync Test");
            self.env
                .push_utf8("GIT_COMMITTER_EMAIL", "reposync@localhost");
            let mut path = OsString::from("/opt/reliability-exec-fixtures/setup-shim");
            if let Some(previous) = std::env::var_os("PATH") {
                path.push(":");
                path.push(previous);
            }
            self.env.push_os("PATH", &path);
        }
    }

    fn open_hook_parent(repo_fd: RawFd) -> Result<RawFd, String> {
        match fstatat_nofollow(repo_fd, "hooks")? {
            None => Err("hooks directory is absent".to_string()),
            Some(st) => {
                if file_type(st.st_mode) != libc::S_IFDIR {
                    return Err("hooks node is not a directory".to_string());
                }
                let c = c_name("hooks")?;
                let fd = unsafe {
                    libc::openat(
                        repo_fd,
                        c.as_ptr(),
                        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                        0,
                    )
                };
                if fd < 0 {
                    return Err(last_os_error("open hooks"));
                }
                Ok(fd)
            }
        }
    }

    fn open_hook_parent_existing(repo_fd: RawFd) -> Result<RawFd, String> {
        open_hook_parent(repo_fd)
    }

    fn readlink_at(dir_fd: RawFd, name: &str) -> Result<String, String> {
        let c = c_name(name)?;
        let mut buffer = vec![0u8; 4096];
        let got = unsafe {
            libc::readlinkat(
                dir_fd,
                c.as_ptr(),
                buffer.as_mut_ptr() as *mut libc::c_char,
                buffer.len(),
            )
        };
        if got < 0 {
            return Err(last_os_error("readlinkat"));
        }
        if got as usize == buffer.len() {
            return Err("readlinkat result was truncated".to_string());
        }
        buffer.truncate(got as usize);
        String::from_utf8(buffer).map_err(|_| "symlink target is not UTF-8".to_string())
    }

    fn actual_case_root_id(case_root: &Path) -> Result<String, String> {
        let text = case_root
            .to_str()
            .ok_or_else(|| "case root is not UTF-8".to_string())?;
        if !text.starts_with('/') || text.contains("//") || text.ends_with('/') || text.contains("/./") || text.contains("/../") {
            return Err(format!("case root {text} is not a canonical absolute path"));
        }
        const PARENT: &str = "/fixture/tmp/";
        if !text.starts_with(PARENT) {
            return Err(format!(
                "case root {text} is not an immediate child of /fixture/tmp"
            ));
        }
        let id = &text[PARENT.len()..];
        if id.is_empty()
            || id.contains('/')
            || id == "."
            || id == ".."
            || id.len() > 96
            || !id.is_ascii()
            || !is_component(id)
        {
            return Err(
                "case_root_id must be one ASCII basename under /fixture/tmp, at most 96 bytes"
                    .to_string(),
            );
        }
        if let Some(pinned) = std::env::var_os("REPOSYNC_EXEC_EXPECTED_ROOT_ID") {
            let pinned = pinned
                .to_str()
                .ok_or_else(|| "REPOSYNC_EXEC_EXPECTED_ROOT_ID is not UTF-8".to_string())?;
            if pinned != id {
                return Err(
                    "REPOSYNC_EXEC_EXPECTED_ROOT_ID does not match the actual case directory basename"
                        .to_string(),
                );
            }
        }
        Ok(id.to_string())
    }

    fn pinned_instance_nonce(case_root_id: &str) -> Result<String, String> {
        if std::env::var_os("REPOSYNC_EXEC_CASE_DRAW").is_some() {
            return Err(
                "REPOSYNC_EXEC_CASE_DRAW is not adopted as a nonce or case root".to_string(),
            );
        }
        let _allows_equality_with_case_root = case_root_id;
        let mut bytes = [0u8; 16];
        let mut source = std::fs::File::open("/dev/urandom")
            .map_err(|error| format!("core nonce source is unavailable: {error}"))?;
        std::io::Read::read_exact(&mut source, &mut bytes)
            .map_err(|error| format!("core nonce read failed: {error}"))?;
        let nonce: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        if let Some(raw) = std::env::var_os("REPOSYNC_EXEC_EXPECTED_INSTANCE") {
            let text = raw
                .to_str()
                .ok_or_else(|| "instance nonce pin is not UTF-8".to_string())?;
            if text != nonce {
                return Err(
                    "REPOSYNC_EXEC_EXPECTED_INSTANCE does not match the core-owned nonce"
                        .to_string(),
                );
            }
        }
        Ok(nonce)
    }

    fn repository_stays_in_case(case_root: &Path, repository: &Path) -> Result<(), String> {
        let case_text = case_root
            .to_str()
            .ok_or_else(|| "case root is not UTF-8".to_string())?;
        let repo_text = repository
            .to_str()
            .ok_or_else(|| "repository path is not UTF-8".to_string())?;
        if repo_text == case_text {
            return Ok(());
        }
        let prefix = format!("{case_text}/");
        if !repo_text.starts_with(&prefix) {
            return Err("repository is outside the case root".to_string());
        }
        let rest = &repo_text[prefix.len()..];
        if !is_component(rest) {
            return Err("repository must be the case root or one basename inside it".to_string());
        }
        Ok(())
    }

    fn catalogue_identity(path: &str) -> Result<(String, u64, u32, u64, u64), String> {
        let fd = open_nofollow(Path::new(path), false)?;
        let read = (|| {
            let st = fstat_fd(fd)?;
            if file_type(st.st_mode) != libc::S_IFREG || st.st_size < 0 {
                return Err("catalogue target is not a regular file".to_string());
            }
            let mut body = vec![0u8; st.st_size as usize];
            read_exact(fd, &mut body)?;
            Ok((
                hex::encode(sha2::Sha256::digest(&body)),
                body.len() as u64,
                perm_bits(st.st_mode),
                st.st_dev as u64,
                st.st_ino as u64,
            ))
        })();
        close_fd(fd);
        read
    }

    pub struct SelectedEntry {
        pub entry_id: String,
        pub source_path: String,
        pub sha256: String,
        pub bytes: u64,
        pub source_mode: u32,
        pub installed_relative_path: String,
        pub installed_mode: u32,
    }

    fn archive_relative_source(path: &str) -> bool {
        let parts: Vec<&str> = path.split('/').collect();
        !path.is_empty()
            && !path.starts_with('/')
            && parts.len() >= 2
            && parts
                .iter()
                .all(|part| !part.is_empty() && *part != "." && *part != "..")
    }

    pub fn require_selected_entry(entry: Option<&SelectedEntry>) -> Result<(), String> {
        let Some(row) = entry else {
            return Err(
                "validated entry is required; validated catalogue metadata is missing; an explicit mode is required"
                    .to_string(),
            );
        };
        let mut problems = Vec::new();
        if row.entry_id.is_empty() {
            problems.push("validated entry is required");
        }
        let digest = row.sha256.len() == 64
            && row
                .sha256
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
        if !digest || row.bytes == 0 {
            problems.push("validated catalogue metadata is missing");
        }
        if !archive_relative_source(&row.source_path) {
            problems.push("source_path is archive-relative");
        }
        if !archive_relative_source(&row.installed_relative_path) {
            problems.push("installed_relative_path is not the hook slot");
        }
        if row.source_mode == 0 || row.installed_mode == 0 {
            problems.push("an explicit mode is required");
        } else {
            if row.source_mode != 0o100755 {
                problems.push("source_mode is git 100755");
            }
            if row.installed_mode != 0o755 {
                problems.push("installed_mode is 0755");
            }
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems.join("; "))
        }
    }

    pub struct CatalogueAttributes<'a> {
        pub source_path: &'a str,
        pub installed_relative_path: &'a str,
        pub sha256: &'a str,
        pub bytes: u64,
        pub source_mode: u32,
        pub installed_mode: u32,
    }

    pub fn select_attested_entry(
        entry_id: &str,
        attributes: Option<CatalogueAttributes<'_>>,
    ) -> Result<SelectedEntry, String> {
        let Some(row) = attributes else {
            return Err("validated catalogue metadata is missing".to_string());
        };
        let selected = SelectedEntry {
            entry_id: entry_id.to_string(),
            source_path: row.source_path.to_string(),
            sha256: row.sha256.to_string(),
            bytes: row.bytes,
            source_mode: row.source_mode,
            installed_relative_path: row.installed_relative_path.to_string(),
            installed_mode: row.installed_mode,
        };
        require_selected_entry(Some(&selected))?;
        Ok(selected)
    }

    pub fn attested_entry_from_validated_metadata(entry_id: &str) -> Result<SelectedEntry, String> {
        let _ = entry_id;
        Err(format!(
            "derived runtime projection is unbound for {entry_id}; harness owner 01a1434e-b4d8-7583-969f-75ad62ef10ce catalogue reposync-immutable-exec-v2.1 reconciled v2.3.2 has no acknowledgment; a process-wide entry id is not provenance"
        ))
    }

    pub fn prepare_existing_repository(
        boundary: &ProcessBoundary,
        manifest: &HarnessValidatedH,
        selected: &SelectedEntry,
        entry_id: &str,
        case_id: &str,
        case_root: &Path,
        repository: Option<&Path>,
        markers_on_case: bool,
        initial_oid: Option<&str>,
    ) -> Result<PreparedRepo, String> {
        boundary.assert_held();
        if crate::process::retained_ownership_incomplete() {
            return Err("retained process ownership is unresolved".to_string());
        }
        require_selected_entry(Some(selected))?;
        if selected.entry_id != entry_id {
            return Err("selected entry does not match the requested catalogue row".to_string());
        }
        if !archive_relative_source(&selected.source_path) {
            return Err(
                "source_path is archive-relative, not an installed absolute path".to_string(),
            );
        }
        if let Some(admitted) = ADMITTED_HOOKS.iter().find(|row| row.entry_id == entry_id) {
            if selected.installed_relative_path == admitted.slot
                || !selected.installed_relative_path.ends_with(&format!("/{}", admitted.slot))
            {
                return Err(
                    "installed_relative_path is the catalogue path, not the hook slot".to_string(),
                );
            }
            let (anchored_sha, anchored_len, anchored_mode, _dev, _ino) =
                catalogue_identity(admitted.target)?;
            if anchored_sha != selected.sha256
                || anchored_len != selected.bytes
                || anchored_mode != selected.installed_mode
            {
                return Err(
                    "anchored installed identity does not match the selected row".to_string(),
                );
            }
        } else if entry_id == "setup-push-shim-v1" {
            return Err(
                "derived runtime projection is unbound for setup-push-shim-v1; harness owner 01a1434e-b4d8-7583-969f-75ad62ef10ce has not acknowledged an H-row binding"
                    .to_string(),
            );
        } else {
            return Err("this entry has no admitted installed target".to_string());
        }
        regression_source_obligations(case_id)?;
        let case_root_id = actual_case_root_id(case_root)?;
        let instance = pinned_instance_nonce(&case_root_id)?;
        if let Some(repository) = repository {
            repository_stays_in_case(case_root, repository)?;
        }
        let retained_oid = if entry_id == "git-ref-move-v1" {
            let oid = initial_oid.ok_or_else(|| "git-ref-move-v1 requires initial_oid".to_string())?;
            validate_initial_oid(oid)?;
            Some(oid.to_string())
        } else if initial_oid.is_some() {
            return Err("this entry schema has no initial_oid".to_string());
        } else {
            None
        };
        let mut fields = vec![
            ("case_id", case_id),
            ("case_root_id", case_root_id.as_str()),
            ("entry_id", entry_id),
            ("instance_id", instance.as_str()),
        ];
        if let Some(oid) = retained_oid.as_deref() {
            fields.push(("initial_oid", oid));
        }
        let case_bytes = canonical_json(&fields)?;
        let data_sha256 = hex::encode(sha2::Sha256::digest(&case_bytes));
        let case_fd = open_nofollow(case_root, true)?;
        if let Err(error) = require_case_root_directory(case_fd) {
            close_fd(case_fd);
            return Err(error);
        }
        let repo_fd = if let Some(repository) = repository {
            match open_nofollow(repository, true) {
                Ok(fd) => fd,
                Err(error) => {
                    close_fd(case_fd);
                    return Err(error);
                }
            }
        } else {
            case_fd
        };
        if repo_fd != case_fd {
            if let Err(error) = require_repository_directory(repo_fd) {
                close_fd(repo_fd);
                close_fd(case_fd);
                return Err(error);
            }
        }
        let config_fd = repo_fd;
        if let Err(error) = publish_nofollow(config_fd, "execution-case-v1.json", &case_bytes, 0o600) {
            if repo_fd != case_fd {
                close_fd(repo_fd);
            }
            close_fd(case_fd);
            return Err(error);
        }
        let marker_fd = if markers_on_case { case_fd } else { repo_fd };
        let marker_path = if markers_on_case {
            case_root.to_path_buf()
        } else {
            repository
                .unwrap_or(case_root)
                .to_path_buf()
        };
        let mut prepared = PreparedRepo {
            case_fd,
            repo_fd,
            config_fd,
            marker_fd,
            env: EnvGuard::new(),
            selected_path: selected.source_path.clone(),
            selected_sha256: selected.sha256.clone(),
            selected_len: selected.bytes,
            selected_mode: selected.source_mode,
            case_root_id: case_root_id.clone(),
            explicitly_completed: false,
            context: boundary.context(),
            publication: None,
            blocking: None,
            server: None,
            proved_markers: std::sync::Mutex::new(Vec::new()),
            selected_installed_relative_path: selected.installed_relative_path.clone(),
            selected_installed_mode: selected.installed_mode,
            entry_id: entry_id.to_string(),
            case_id: case_id.to_string(),
            instance_id: instance.clone(),
            manifest_sha256: manifest.as_str().to_string(),
            data_sha256,
            initial_oid: retained_oid,
            marker_path,
            hook_target: None,
            hook_dev: None,
            hook_ino: None,
            hook_link_dev: None,
            hook_link_ino: None,
        };
        prepared.env.push_utf8("REPOSYNC_FIXTURE_EXEC_MODE", "packaged-immutable-v1");
        prepared.env.push_utf8("REPOSYNC_EXEC_EXPECTED_ENTRY", entry_id);
        prepared.env.push_utf8("REPOSYNC_EXEC_EXPECTED_CASE", case_id);
        prepared
            .env
            .push_utf8("REPOSYNC_EXEC_EXPECTED_INSTANCE", &instance);
        prepared
            .env
            .push_utf8("REPOSYNC_EXEC_EXPECTED_ROOT_ID", &case_root_id);
        prepared
            .env
            .push_utf8("REPOSYNC_EXEC_EXPECTED_DATA_SHA256", &prepared.data_sha256);
        prepared
            .env
            .push_utf8("REPOSYNC_EXEC_EXPECTED_MANIFEST_SHA256", manifest.as_str());
        Ok(prepared)
    }

    pub fn observe_ref_move_after_join(
        prepared: &PreparedRepo,
        receiver_status: ReceiverStatus,
    ) -> Result<RefMoveObservation, String> {
        let audit = match prepared.read_marker("action-audit-witness") {
            Ok(bytes) => bytes,
            Err(error) => {
                return Err(format!(
                    "action-audit-witness absent after join; publication_rejected={} observed_exit={:?} is not qualification and another update-ref is forbidden: {error}",
                    receiver_status.publication_rejected, receiver_status.observed_exit
                ));
            }
        };
        let parsed = parse_flat_object(&audit).map_err(|error| {
            format!("action-audit-witness refused; another update-ref is forbidden: {error}")
        })?;
        let required = [
            "action",
            "after_oid",
            "before_oid",
            "case_id",
            "child_exit",
            "data_sha256",
            "entry_id",
            "initial_oid",
            "instance_id",
            "manifest_sha256",
            "schema",
        ];
        if parsed.len() != required.len() || required.iter().any(|key| !parsed.contains_key(*key)) {
            return Err("action-audit-witness keys are not the sealed set".to_string());
        }
        let string_field = |key: &str| -> Result<String, String> {
            let encoded = parsed.get(key).ok_or_else(|| format!("missing {key}"))?;
            encoded
                .strip_prefix('"')
                .and_then(|value| value.strip_suffix('"'))
                .map(str::to_string)
                .ok_or_else(|| format!("{key} is not a string"))
        };
        if string_field("action")? != "update-ref" {
            return Err("audit action is not update-ref".to_string());
        }
        let intended = prepared
            .initial_oid
            .clone()
            .ok_or_else(|| "retained case has no initial_oid".to_string())?;
        if string_field("initial_oid")? != intended {
            return Err("audit initial_oid does not equal the retained value".to_string());
        }
        if string_field("case_id")? != prepared.case_id
            || string_field("entry_id")? != prepared.entry_id
            || string_field("instance_id")? != prepared.instance_id
            || string_field("manifest_sha256")? != prepared.manifest_sha256
            || string_field("data_sha256")? != prepared.data_sha256
        {
            return Err("audit identity does not match the retained case".to_string());
        }
        if parsed.get("schema").map(String::as_str) != Some("1") {
            return Err("audit schema is not canonical integer 1".to_string());
        }
        let child_exit = parsed
            .get("child_exit")
            .ok_or_else(|| "child_exit missing".to_string())?
            .parse::<i64>()
            .map_err(|_| "child_exit is not an integer".to_string())?;
        if !(0..=255).contains(&child_exit) {
            return Err("child_exit is outside the sealed range".to_string());
        }
        Ok(RefMoveObservation {
            audit_child_exit: AuditChildExit { value: child_exit },
            receiver_status,
            another_update_ref_permitted: false,
        })
    }


    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum ExplicitMode {
        NativeGenerated,
        PackagedImmutable,
    }

    pub fn allocate_case_nonce() -> Result<String, String> {
        let mut bytes = [0u8; 16];
        let mut source = std::fs::File::open("/dev/urandom")
            .map_err(|error| format!("core nonce source is unavailable: {error}"))?;
        std::io::Read::read_exact(&mut source, &mut bytes)
            .map_err(|error| format!("core nonce read failed: {error}"))?;
        Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
    }

    pub fn admit_child_environment(
        command: &std::process::Command,
        mode: ExplicitMode,
    ) -> Result<(), String> {
        let value = |key: &str| -> Option<std::ffi::OsString> {
            match command.get_envs().find(|(name, _)| *name == key) {
                Some((_, item)) => item.map(std::ffi::OsString::from),
                None => std::env::var_os(key),
            }
        };
        let malformed = |raw: &std::ffi::OsStr| -> bool {
            let Some(text) = raw.to_str() else {
                return true;
            };
            text.len() != 64 || !text.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        };
        let packaged_pins = [
            "REPOSYNC_EXEC_EXPECTED_ENTRY",
            "REPOSYNC_EXEC_EXPECTED_CASE",
            "REPOSYNC_EXEC_EXPECTED_INSTANCE",
            "REPOSYNC_EXEC_EXPECTED_ROOT_ID",
            "REPOSYNC_EXEC_EXPECTED_DATA_SHA256",
        ];
        match mode {
            ExplicitMode::NativeGenerated => {
                if packaged_pins.iter().any(|key| value(key).is_some())
                    || value("REPOSYNC_EXEC_EXPECTED_MANIFEST_SHA256").is_some()
                {
                    return Err(
                        "native admission rejected a packaged marker before env cleanup".to_string(),
                    );
                }
                if let Some(raw) = value("REPOSYNC_FIXTURE_EXEC_MODE") {
                    if raw.to_str() != Some("native-generated-v1") {
                        return Err(
                            "native admission rejected a packaged marker before env cleanup"
                                .to_string(),
                        );
                    }
                }
                Ok(())
            }
            ExplicitMode::PackagedImmutable => match value("REPOSYNC_EXEC_EXPECTED_MANIFEST_SHA256") {
                None => Err("absent H".to_string()),
                Some(raw) if malformed(&raw) => Err("malformed H".to_string()),
                Some(_) => Ok(()),
            },
        }
    }

    pub struct SpawnContext {
        session: Option<crate::process::CaseContextId>,
        packaged: bool,
        entry_id: String,
        case_id: String,
        instance_id: String,
        case_root_id: String,
        data_sha256: String,
        manifest_sha256: String,
    }

    impl SpawnContext {
        pub(super) fn session(&self) -> Option<crate::process::CaseContextId> { self.session }
    }

    pub fn native_spawn_context() -> SpawnContext {
        SpawnContext {
            session: None,
            packaged: false,
            entry_id: String::new(),
            case_id: String::new(),
            instance_id: String::new(),
            case_root_id: String::new(),
            data_sha256: String::new(),
            manifest_sha256: String::new(),
        }
    }

    pub fn packaged_spawn_context(prepared: &PreparedRepo) -> SpawnContext {
        SpawnContext {
            session: Some(prepared.context),
            packaged: true,
            entry_id: prepared.entry_id.clone(),
            case_id: prepared.case_id.clone(),
            instance_id: prepared.instance_id.clone(),
            case_root_id: prepared.case_root_id.clone(),
            data_sha256: prepared.data_sha256.clone(),
            manifest_sha256: prepared.manifest_sha256.clone(),
        }
    }

    pub fn apply_spawn_context(
        command: &mut std::process::Command,
        context: &SpawnContext,
    ) -> Result<(), String> {
        if !context.packaged {
            if context.session.is_some() { return Err("native HTTP context cannot carry a case".into()); }
            return admit_child_environment(command, ExplicitMode::NativeGenerated);
        }
        let session = context.session.ok_or_else(|| "packaged HTTP context has no admitted case".to_string())?;
        if !crate::process::case_registration_open(session) {
            return Err("packaged HTTP case registration is closed".into());
        }
        command.env("REPOSYNC_FIXTURE_EXEC_MODE", "packaged-immutable-v1");
        command.env("REPOSYNC_EXEC_EXPECTED_ENTRY", &context.entry_id);
        command.env("REPOSYNC_EXEC_EXPECTED_CASE", &context.case_id);
        command.env("REPOSYNC_EXEC_EXPECTED_INSTANCE", &context.instance_id);
        command.env("REPOSYNC_EXEC_EXPECTED_ROOT_ID", &context.case_root_id);
        command.env("REPOSYNC_EXEC_EXPECTED_DATA_SHA256", &context.data_sha256);
        command.env(
            "REPOSYNC_EXEC_EXPECTED_MANIFEST_SHA256",
            &context.manifest_sha256,
        );
        admit_child_environment(command, ExplicitMode::PackagedImmutable)
    }

#[cfg(test)]
mod child_context_admission {
    use super::{admit_child_environment, allocate_case_nonce, ExplicitMode};
    use std::ffi::OsString;
    use std::process::Command;

    fn env_of(command: &Command, key: &str) -> Option<OsString> {
        command
            .get_envs()
            .find(|(name, _)| *name == key)
            .and_then(|(_, value)| value.map(OsString::from))
    }

    #[test]
    fn native_admission_rejects_packaged_markers_before_clearing_them() {
        // Synchronous environment inspection. This is not an async spawn.
        let mut command = Command::new("python3");
        command.env("PATH", "/usr/bin");
        command.env("REPOSYNC_FIXTURE_EXEC_MODE", "packaged-immutable-v1");
        command.env(
            "REPOSYNC_EXEC_EXPECTED_MANIFEST_SHA256",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        );
        let admitted = admit_child_environment(&command, ExplicitMode::NativeGenerated);
        assert!(admitted.is_err());
        assert_eq!(
            env_of(&command, "REPOSYNC_FIXTURE_EXEC_MODE").as_deref(),
            Some(std::ffi::OsStr::new("packaged-immutable-v1"))
        );
        assert!(env_of(&command, "REPOSYNC_EXEC_EXPECTED_MANIFEST_SHA256").is_some());
        assert_eq!(env_of(&command, "PATH").as_deref(), Some(std::ffi::OsStr::new("/usr/bin")));
    }

    #[test]
    fn absent_h_and_malformed_h_are_different_failures() {
        let mut missing = Command::new("python3");
        assert!(admit_child_environment(&missing, ExplicitMode::PackagedImmutable).is_err());
        missing.env("REPOSYNC_EXEC_EXPECTED_MANIFEST_SHA256", "not-a-manifest");
        let malformed = admit_child_environment(&missing, ExplicitMode::PackagedImmutable);
        assert!(malformed.is_err());
        assert_ne!(
            admit_child_environment(&Command::new("python3"), ExplicitMode::PackagedImmutable)
                .unwrap_err(),
            malformed.unwrap_err()
        );
    }

    #[test]
    fn core_allocates_a_fresh_nonce_per_begin() {
        let first = allocate_case_nonce().unwrap();
        let second = allocate_case_nonce().unwrap();
        assert_eq!(first.len(), 32);
        assert!(first.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')));
        assert_ne!(first, second);
    }
}

}

#[cfg(unix)]
fn with_attested_http_entry<T>(
    create_fixture: impl FnOnce(packaged_call_site::SelectedEntry) -> Result<T, String>,
) -> Result<T, String> {
    let selected = packaged_call_site::attested_entry_from_validated_metadata("git-ref-move-v1")?;
    create_fixture(selected)
}

#[cfg(unix)]
fn packaged_http_advertisement(
    manifest: packaged_call_site::HarnessValidatedH,
    boundary: &packaged_call_site::ProcessBoundary,
    case_id: &str,
    tls: bool,
) -> Result<(), String> {
    boundary.assert_held();
    // A selected, externally validated projection is still required. Keep the
    // frozen selector refusal ahead of directories, certificates and servers.
    let session = boundary.context();
    let (fixture, mut prepared) = with_attested_http_entry(|selected| {
        let mut prepared_slot: Option<packaged_call_site::PreparedRepo> = None;
        let fixture = HttpFixture::open(tls, None, Some(session), Some(&mut |case_root, repository, initial_oid| {
            let mut prepared = packaged_call_site::prepare_existing_repository(
                boundary,
                &manifest,
                &selected,
                "git-ref-move-v1",
                case_id,
                case_root,
                Some(repository),
                false,
                Some(initial_oid),
            )?;
            prepared.install_admitted_hook()?;
            prepared.recheck_hook()?;
            let context = packaged_call_site::packaged_spawn_context(&prepared);
            prepared_slot = Some(prepared);
            Ok(context)
        }))?;
        let prepared = prepared_slot.ok_or_else(|| "prepared session missing before launch".to_string())?;
        Ok((fixture, prepared))
    })?;
    let before = fixture.tip();
    // Move the repositories into their owned thread; do not require a shared
    // reference to git2 repositories to be Sync. Registration precedes spawn.
    let handle = spawn_http_work(session, move || {
        let published = fixture.publish(None);
        (fixture, published)
    })?;
    let ((mut fixture, published), join) =
        join_http_work(handle, std::time::Duration::from_secs(45))?;
    prepared.record_publication_joined(CheckedHttpPublication { join })?;
    prepared.record_blocking_joined();
    let after = fixture.tip();
    assert!(
        published.is_err(),
        "publish={published:?} before={before} after={after} {} stderr={}",
        exec_witness("hook", &fixture.target.path().join("hooks/pre-receive")),
        fixture.backend_stderr()
    );
    assert_eq!(fixture.tip(), fixture.initial);
    let receiver = packaged_call_site::ReceiverStatus::from_publication_result(published.is_err());
    if receiver.observed_exit.is_some() {
        return Err(
            "http advertisement result has no observed process exit; no code was synthesized"
                .to_string(),
        );
    }
    let observation = packaged_call_site::observe_ref_move_after_join(&prepared, receiver)?;
    if observation.another_update_ref_permitted {
        return Err("another update-ref is forbidden after the joined receiver status".to_string());
    }
    if observation.receiver_status.observed_exit.is_some() {
        return Err("receiver publication result is not an observed process exit".to_string());
    }
    prepared.require_entered_witness("entered-witness")?;
    prepared.close_registration()?;
    let server = fixture.complete_owned_server()?;
    prepared.record_server_completed(server)?;
    fixture.complete_case_root()?;
    prepared.seal()?;
    Ok(())
}

#[cfg(unix)]
fn packaged_f07_http_advertisement(
    manifest: packaged_call_site::HarnessValidatedH,
    boundary: &packaged_call_site::ProcessBoundary,
) -> Result<(), String> {
    packaged_http_advertisement(manifest, boundary, "F07", false)
}

#[cfg(unix)]
fn packaged_f08_https_advertisement(
    manifest: packaged_call_site::HarnessValidatedH,
    boundary: &packaged_call_site::ProcessBoundary,
) -> Result<(), String> {
    packaged_http_advertisement(manifest, boundary, "F08", true)
}
