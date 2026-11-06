use super::*;
use crate::svn::working_copy::ConfinedWorkingCopy;

fn fixture() -> (tempfile::TempDir, SvnClient) {
    let root = tempfile::tempdir().unwrap();
    let repository = root.path().join("repository@literal");
    let output = std::process::Command::new("svnadmin")
        .arg("create")
        .arg(&repository)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "svnadmin is required: {:?}",
        output
    );
    let url = reqwest::Url::from_directory_path(&repository)
        .unwrap()
        .to_string();
    let client = SvnClient::new_isolated_refresh(url.trim_end_matches('/'), "", None).unwrap();
    (root, client)
}

#[tokio::test]
async fn rs113_real_literal_names_keep_exact_bytes_through_add_cat_revert_rename_delete() {
    let (root, client) = fixture();
    client
        .export("", 0, &root.path().join("empty-export"))
        .await
        .unwrap();
    let wc = root.path().join("working@copy");
    client.checkout_head(&wc).await.unwrap();
    let confined = ConfinedWorkingCopy::open(&wc).unwrap();
    let names = [
        "ordinary.txt",
        "-dash",
        "--username",
        "--config-dir",
        "--diff-cmd=not-a-helper",
        "x@HEAD",
        "snow 雪",
        " leading trailing ",
        "xml&<name>",
        "space name",
        "nested/深@1",
    ];
    for (index, name) in names.iter().enumerate() {
        confined
            .write_file(name, format!("exact payload {index}\n").as_bytes())
            .unwrap();
    }
    client.add(&wc, &names).await.unwrap();
    let statuses = client.status_entries(&wc).await.unwrap();
    for name in names {
        assert!(
            statuses.iter().any(|e| e.path == name && e.item == "added"),
            "{name:?}: {statuses:?}"
        );
    }
    let revision = client
        .commit(&wc, "literal paths\nUnicode 雪\n[marker]", "")
        .await
        .unwrap();
    assert_eq!(revision, 1);
    for (index, name) in names.iter().enumerate() {
        assert_eq!(
            client.cat(name, revision).await.unwrap(),
            format!("exact payload {index}\n"),
            "{name:?}"
        );
    }
    let log = client.log(1, 1).await.unwrap();
    for name in names {
        assert!(
            log[0]
                .changed_paths
                .iter()
                .any(|p| p.path == format!("/{name}")),
            "{name:?}: {:?}",
            log[0].changed_paths
        );
    }
    assert!(client
        .list_branches("")
        .await
        .unwrap()
        .contains(&"space name".to_string()));
    confined
        .write_file("space name", b"temporary edit")
        .unwrap();
    client.revert_files(&wc, &["space name"]).await.unwrap();
    assert_eq!(
        std::fs::read(wc.join("space name")).unwrap(),
        b"exact payload 9\n"
    );
    // Rename is the explicit deletion/addition pair used by projection.
    client.rm(&wc, &["xml&<name>", "-dash"]).await.unwrap();
    confined
        .write_file("renamed/雪@9", b"exact payload 8\n")
        .unwrap();
    client.add_with_retry(&wc, &["renamed/雪@9"]).await.unwrap();
    assert_eq!(client.commit(&wc, "rename/delete", "").await.unwrap(), 2);
    assert!(client.cat("xml&<name>", 2).await.is_err());
    assert_eq!(
        client.cat("xml&<name>", 1).await.unwrap(),
        "exact payload 8\n"
    );
    assert_eq!(
        client.cat("renamed/雪@9", 2).await.unwrap(),
        "exact payload 8\n"
    );
    assert!(client.status_entries(&wc).await.unwrap().is_empty());
}

#[tokio::test]
async fn rs113_unsupported_nodes_and_paths_refuse_before_staging_or_outside_access() {
    let (root, client) = fixture();
    let wc = root.path().join("wc");
    client.checkout_head(&wc).await.unwrap();
    std::fs::write(wc.join("ordinary"), b"control").unwrap();
    let outside = root.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("canary"), b"unchanged").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, wc.join("link")).unwrap();
    let before = std::fs::read(wc.join(".svn/wc.db")).unwrap();
    for bad in [
        "../outside/canary",
        ".svn/wc.db",
        "C:escape",
        "a\\b",
        "link/canary",
        "tab\tname",
        "line\nname",
    ] {
        assert!(client.add(&wc, &["ordinary", bad]).await.is_err(), "{bad}");
        assert_eq!(std::fs::read(wc.join(".svn/wc.db")).unwrap(), before);
        assert_eq!(std::fs::read(outside.join("canary")).unwrap(), b"unchanged");
    }
    let foreign = reqwest::Url::from_file_path(outside.join("canary"))
        .unwrap()
        .to_string();
    assert!(client.cat(&foreign, 1).await.is_err());
    assert_eq!(client.info().await.unwrap().latest_rev, 0);
    assert_eq!(std::fs::read(wc.join("ordinary")).unwrap(), b"control");
}

#[tokio::test]
async fn rs113_branch_url_components_and_empty_pegs_are_literal() {
    let (_root, client) = fixture();
    let trunk = literal_url(&repository_url(client.url(), "trunk").unwrap(), None).unwrap();
    client
        .run_svn(&["mkdir", "-m", "seed", "--", &trunk])
        .await
        .unwrap();
    let revision = client.info().await.unwrap().latest_rev;
    assert_eq!(revision, 1);
    client
        .create_branch("雪@HEAD", "trunk", "copies", revision)
        .await
        .unwrap();
    assert!(client
        .list_branches("copies")
        .await
        .unwrap()
        .contains(&"雪@HEAD".into()));
    client.delete_branch("copies/雪@HEAD").await.unwrap();
    assert!(client.list_branches("copies").await.unwrap().is_empty());
}

#[test]
fn rs113_xml_names_and_command_roles_refuse_ambiguous_or_unsafe_inputs() {
    use crate::svn::parser::parse_svn_status_xml;
    let xml = r#"<status><target path="."><entry path=" space &amp; 雪@1 "><wc-status item="modified" props="none"/></entry></target></status>"#;
    assert_eq!(parse_svn_status_xml(xml).unwrap()[0].path, " space & 雪@1 ");
    let info = r#"<info><entry revision="1"><url>file:///repo/&amp;lt;&#x96EA;</url><repository><root>file:///repo</root><uuid>fixture</uuid></repository></entry></info>"#;
    assert_eq!(
        crate::svn::parser::parse_svn_info(info).unwrap().url,
        "file:///repo/&lt;雪"
    );
    for xml in [
        r#"<status/>"#,
        r#"<status><target path="elsewhere"/></status>"#,
        r#"<status><target path="."><unknown/></target></status>"#,
        r#"<status><target path="."><entry path="tab&#9;name"><wc-status item="added" props="none"/></entry></target></status>"#,
        r#"<status><target path="."><entry path="x"><wc-status item="external" props="none"/></entry></target></status>"#,
    ] {
        assert!(parse_svn_status_xml(xml).is_err(), "{xml}");
    }
    let root = Path::new("/disposable/wc");
    for flag in [
        "--username",
        "--password",
        "--config-dir",
        "--config-option",
        "--editor-cmd",
        "--diff-cmd",
        "--targets",
    ] {
        assert!(
            super::super::operand::working_copy_arguments(root, &["info", flag, "untrusted"])
                .is_err(),
            "{flag}"
        );
    }
    let args = super::super::operand::working_copy_arguments(
        root,
        &[
            "propset",
            "--",
            "svn:custom",
            "--config-option=literal-value",
            "--username",
        ],
    )
    .unwrap();
    assert_eq!(
        args,
        [
            "propset",
            "--",
            "svn:custom",
            "--config-option=literal-value",
            "--username@"
        ]
    );
    for url in [
        "https://example.invalid/a/../b",
        "https://example.invalid/a/%2e%2e/b",
        "https://example.invalid/a%2fb",
        "https://example.invalid/a%09b",
        "https://example.invalid/a\nb",
        "https://user:password@example.invalid/repo",
        "https://example.invalid/repo?other=path",
    ] {
        assert!(
            super::super::operand::literal_url(url, None).is_err(),
            "{url}"
        );
    }
}

#[tokio::test]
async fn rs113_external_properties_refuse_without_materializing_or_publishing_omitted_work() {
    let (root, client) = fixture();
    let wc = root.path().join("wc");
    client.checkout_head(&wc).await.unwrap();
    std::fs::write(wc.join("ordinary"), b"before").unwrap();
    client.add(&wc, &["ordinary"]).await.unwrap();
    client.commit(&wc, "seed", "").await.unwrap();
    let outside_repo = root.path().join("outside-repository");
    let output = std::process::Command::new("svnadmin")
        .arg("create")
        .arg(&outside_repo)
        .output()
        .unwrap();
    assert!(output.status.success());
    let external = format!(
        "{} dependency",
        reqwest::Url::from_directory_path(&outside_repo).unwrap()
    );
    client
        .run_svn_in_dir_public(&wc, &["propset", "svn:externals", &external, "."])
        .await
        .unwrap();
    assert!(client.require_supported_working_copy(&wc).await.is_err());
    // The normal commit API also refuses the unsupported property before an effect.
    assert!(client.commit(&wc, "must not publish", "").await.is_err());
    assert_eq!(client.info().await.unwrap().latest_rev, 1);
    assert_eq!(std::fs::read(wc.join("ordinary")).unwrap(), b"before");
    assert!(!wc.join("dependency").exists());
    client
        .run_svn_in_dir_public(&wc, &["propdel", "svn:externals", "."])
        .await
        .unwrap();
    client.require_supported_working_copy(&wc).await.unwrap();
}

#[tokio::test]
async fn rs113_merge_destination_is_a_plain_wcpath_not_a_peg_target() {
    let (root, client) = fixture();
    let seed = root.path().join("seed");
    client.checkout_head(&seed).await.unwrap();
    let target = root.path().join("destination@literal");
    client.checkout_head(&target).await.unwrap();
    std::fs::write(seed.join("new.txt"), b"merged exact bytes\n").unwrap();
    client.add(&seed, &["new.txt"]).await.unwrap();
    let revision = client.commit(&seed, "source change", "").await.unwrap();
    let source = literal_url(client.url(), Some(revision)).unwrap();
    client
        .run_svn_in_dir_public(&target, &["merge", "-r", "0:1", &source, "."])
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(target.join("new.txt")).unwrap(),
        b"merged exact bytes\n"
    );
    assert!(!target.join("@").exists());
    assert_eq!(
        client.info().await.unwrap().latest_rev,
        1,
        "merge does not publish"
    );
    let args = crate::svn::operand::working_copy_arguments(
        &target,
        &["merge", "-r", "0:1", &source, target.to_str().unwrap()],
    )
    .unwrap();
    assert_eq!(args.last().unwrap(), target.to_str().unwrap());
}

#[test]
fn rs113_info_log_uri_components_decode_once_after_containment() {
    use crate::path_projection::{checked_svn_path_identity, svn_log_path_to_branch_relative};
    use crate::svn::parser::{parse_svn_info, parse_svn_log};
    for (encoded, literal, decoy) in [
        ("a%20b", "a b", "a%20b"),
        ("%E9%9B%AA", "雪", "%E9%9B%AA"),
        ("a%2540b", "a%40b", "a@b"),
        ("a@b", "a@b", "a%40b"),
        ("a%23b%3Fc", "a#b?c", "a"),
    ] {
        let info = parse_svn_info(&format!("<info><entry revision=\"7\"><url>file:///repo/branches/{encoded}</url><repository><root>file:///repo</root><uuid>fixture</uuid></repository></entry></info>")).unwrap();
        let identity = checked_svn_path_identity(&info.root_url, &info.url).unwrap();
        assert_eq!(identity.branch_path, format!("branches/{literal}"));
        let log = parse_svn_log(&format!("<log><logentry revision=\"7\"><author>fixture</author><date>2026-11-03T00:00:00Z</date><paths><path action=\"A\" copyfrom-path=\"/branches/{literal}/old.txt\" copyfrom-rev=\"6\">/branches/{literal}/new.txt</path></paths><msg>literal</msg></logentry></log>")).unwrap();
        let path = &log[0].changed_paths[0];
        assert_eq!(
            svn_log_path_to_branch_relative(&path.path, &identity).as_deref(),
            Some("new.txt")
        );
        assert_eq!(
            svn_log_path_to_branch_relative(path.copy_from_path.as_deref().unwrap(), &identity)
                .as_deref(),
            Some("old.txt")
        );
        for wrong in [
            format!("/branches/{decoy}/new.txt"),
            format!("/branches/{literal}-sibling/new.txt"),
        ] {
            assert_eq!(svn_log_path_to_branch_relative(&wrong, &identity), None);
        }
    }
    for target in [
        "file:///repo-other/branches/a",
        "https://foreign.invalid/repo/branches/a",
        "file:///repo/branches/a%2fb",
        "file:///repo/branches/%2e%2e/a",
        "file:///repo/branches/a%5Cb",
    ] {
        assert!(
            checked_svn_path_identity("file:///repo", target).is_none(),
            "{target}"
        );
    }
    let malformed = r#"<log><logentry revision="7"><paths><path action="M">/branches/a\b/file.txt</path></paths></logentry></log>"#;
    assert!(
        parse_svn_log(malformed).is_err(),
        "unrepresentable log names must refuse, not disappear as empty continuation"
    );
    assert_eq!(
        checked_svn_path_identity("file:///repo", "file:///repo")
            .unwrap()
            .branch_path,
        ""
    );
}

#[tokio::test]
async fn rs113_url_userinfo_refuses_without_secret_in_trace_debug_or_error() {
    use std::sync::{Arc, Mutex};
    use tracing::instrument::WithSubscriber;
    #[derive(Clone)]
    struct Capture(Arc<Mutex<String>>);
    struct Fields<'a>(&'a mut String);
    impl tracing::field::Visit for Fields<'_> {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            use std::fmt::Write;
            write!(self.0, "{}={value:?};", field.name()).unwrap();
        }
    }
    impl tracing::Subscriber for Capture {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, attrs: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            attrs.record(&mut Fields(&mut self.0.lock().unwrap()));
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::span::Id, record: &tracing::span::Record<'_>) {
            record.record(&mut Fields(&mut self.0.lock().unwrap()));
        }
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            event.record(&mut Fields(&mut self.0.lock().unwrap()));
        }
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }
    for url in [
        "http://alice:synthetic-secret@127.0.0.1:1/repo",
        "http://alice%3Asynthetic%2Dsecret@127.0.0.1:1/repo",
        "HTTP://alice:synthetic-secret@127.0.0.1:1/repo",
        "file://alice:synthetic-secret@localhost/repo",
    ] {
        let capture = Capture(Arc::new(Mutex::new(String::new())));
        let client =
            tracing::subscriber::with_default(capture.clone(), || SvnClient::new(url, "", ""));
        let error = client
            .info()
            .with_subscriber(capture.clone())
            .await
            .unwrap_err();
        let trace = capture.0.lock().unwrap().clone();
        assert!(
            trace.contains("created SvnClient"),
            "constructor trace must actually be captured: {trace}"
        );
        assert!(trace.contains("REDACTED"), "{trace}");
        let observed = format!("{client:?}\n{error}\n{trace}");
        assert!(!observed.contains("synthetic-secret"), "{observed}");
        assert!(!observed.contains("synthetic%2Dsecret"), "{observed}");
        assert!(observed.contains("REDACTED"), "{observed}");
        assert!(
            matches!(error, crate::errors::SvnError::WorkingCopyError { .. }),
            "URL must refuse at pre-launch validation: {error}"
        );
        assert_eq!(
            client.command_stderr(b"failure with separate-secret"),
            "failure with separate-secret"
        );
    }
    let client = SvnClient::new("file:///not-present-rs113", "fixture", "separate-secret");
    assert_eq!(
        client.command_stderr(b"failed separate-secret http://u:embedded-secret@127.0.0.1:1/"),
        "failed [REDACTED] http://[REDACTED]@127.0.0.1:1/"
    );
}

#[tokio::test]
async fn rs113_configured_branch_builders_select_literal_nodes_and_root_controls() {
    use crate::svn::operand::configured_branch_url;
    let (root, client) = fixture();
    let seed = root.path().join("seed");
    client.checkout_head(&seed).await.unwrap();
    let names = [
        "branches/a%40b",
        "branches/a@b",
        "branches/hash#query? 雪",
        "trunk",
    ];
    for (index, name) in names.iter().enumerate() {
        let file = format!("{name}/identity.txt");
        ConfinedWorkingCopy::open(&seed)
            .unwrap()
            .write_file(&file, format!("node-{index}\n").as_bytes())
            .unwrap();
        client.add_with_retry(&seed, &[&file]).await.unwrap();
    }
    let revision = client
        .commit(&seed, "distinct literal branch nodes", "")
        .await
        .unwrap();
    for (index, name) in names.iter().enumerate() {
        let ordinary = configured_branch_url(client.url(), name).unwrap();
        assert_eq!(
            crate::pair_refresh::branch_svn_url(client.url(), name).unwrap(),
            ordinary
        );
        assert_eq!(
            configured_branch_url(client.url(), &format!("/{name}")).unwrap(),
            ordinary
        );
        let probe = SvnClient::new(&ordinary, "", "");
        let info = probe.info().await.unwrap();
        assert_eq!(info.url, ordinary);
        assert_eq!(
            probe.cat("identity.txt", revision).await.unwrap(),
            format!("node-{index}\n")
        );
    }
    for branch in ["", "/"] {
        assert_eq!(
            configured_branch_url(client.url(), branch).unwrap(),
            client.url()
        );
    }
    for branch in [
        "//trunk",
        "branches/../trunk",
        "branches//trunk",
        "bad\nname",
    ] {
        assert!(configured_branch_url(client.url(), branch).is_err());
    }
    assert_eq!(client.info().await.unwrap().latest_rev, revision);
    assert_eq!(
        client
            .cat("branches/a@b/identity.txt", revision)
            .await
            .unwrap(),
        "node-1\n"
    );
}
