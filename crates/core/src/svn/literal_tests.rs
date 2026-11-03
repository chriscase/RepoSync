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
