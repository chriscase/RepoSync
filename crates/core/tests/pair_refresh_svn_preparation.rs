//! Preparation controls use disposable real SVN and Git repositories. No daemon,
//! production repository or remote publication is involved.

use std::path::PathBuf;
use std::process::Command;

use reposync_core::pair_refresh::lineage::{
    inspect_svn_refresh_lineage, SvnLineagePins, SvnRefreshLineagePins,
};
use reposync_core::pair_refresh::svn_preparation::{
    expected_refresh_mergeinfo, parse_refresh_properties, prepare_clean_child_svn,
    require_refresh_merge_status, CleanChildSvnPins,
};
use reposync_core::svn::SvnClient;
use uuid::Uuid;

fn command(binary: &str, args: &[&str]) {
    let output = Command::new(binary)
        .args(args)
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{binary}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

struct Fixture {
    temp: tempfile::TempDir,
    root: String,
    git: PathBuf,
    parent_wc: PathBuf,
    baseline_tree: String,
    parent_tree: String,
}

fn tree(repo: &git2::Repository, files: &[(&str, &[u8], i32)]) -> String {
    let mut builder = repo.treebuilder(None).unwrap();
    for (name, bytes, mode) in files {
        builder
            .insert(name, repo.blob(bytes).unwrap(), *mode)
            .unwrap();
    }
    builder.write().unwrap().to_string()
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let svn = temp.path().join("svn");
        command("svnadmin", &["create", svn.to_str().unwrap()]);
        let root = reqwest::Url::from_directory_path(&svn)
            .unwrap()
            .to_string()
            .trim_end_matches('/')
            .to_string();
        command(
            "svn",
            &[
                "mkdir",
                &format!("{root}/trunk"),
                &format!("{root}/branches"),
                "-m",
                "layout",
            ],
        );
        let parent_wc = temp.path().join("parent");
        command(
            "svn",
            &[
                "checkout",
                &format!("{root}/trunk"),
                parent_wc.to_str().unwrap(),
            ],
        );
        std::fs::write(parent_wc.join("seed.txt"), b"baseline\n").unwrap();
        std::fs::write(parent_wc.join("old.sh"), b"echo baseline\n").unwrap();
        command(
            "svn",
            &[
                "add",
                parent_wc.join("seed.txt").to_str().unwrap(),
                parent_wc.join("old.sh").to_str().unwrap(),
            ],
        );
        command(
            "svn",
            &[
                "propset",
                "svn:executable",
                "*",
                parent_wc.join("old.sh").to_str().unwrap(),
            ],
        );
        command(
            "svn",
            &["commit", parent_wc.to_str().unwrap(), "-m", "baseline"],
        );
        command(
            "svn",
            &[
                "copy",
                &format!("{root}/trunk@2"),
                &format!("{root}/branches/child"),
                "-m",
                "child copy",
            ],
        );
        std::fs::write(parent_wc.join("seed.txt"), b"parent advance\n").unwrap();
        command(
            "svn",
            &["delete", parent_wc.join("old.sh").to_str().unwrap()],
        );
        std::fs::write(parent_wc.join("new.sh"), b"echo parent\n").unwrap();
        std::fs::write(parent_wc.join("binary.dat"), [0, 1, 128, 255]).unwrap();
        command(
            "svn",
            &[
                "add",
                parent_wc.join("new.sh").to_str().unwrap(),
                parent_wc.join("binary.dat").to_str().unwrap(),
            ],
        );
        command(
            "svn",
            &[
                "propset",
                "svn:executable",
                "*",
                parent_wc.join("new.sh").to_str().unwrap(),
            ],
        );
        command(
            "svn",
            &[
                "commit",
                parent_wc.to_str().unwrap(),
                "-m",
                "parent advance",
            ],
        );
        let git = temp.path().join("git");
        let repo = git2::Repository::init(&git).unwrap();
        let baseline_tree = tree(
            &repo,
            &[
                ("seed.txt", b"baseline\n", 0o100644),
                ("old.sh", b"echo baseline\n", 0o100755),
            ],
        );
        let parent_tree = tree(
            &repo,
            &[
                ("seed.txt", b"parent advance\n", 0o100644),
                ("new.sh", b"echo parent\n", 0o100755),
                ("binary.dat", &[0, 1, 128, 255], 0o100644),
            ],
        );
        Self {
            temp,
            root,
            git,
            parent_wc,
            baseline_tree,
            parent_tree,
        }
    }

    fn client(&self, path: &str) -> SvnClient {
        SvnClient::new(format!("{}{path}", self.root), "", "")
    }

    async fn pins(&self, parent_revision: i64) -> CleanChildSvnPins {
        let lineage = inspect_svn_refresh_lineage(
            &self.client("/branches/child"),
            &self.client("/trunk"),
            &SvnRefreshLineagePins {
                child: SvnLineagePins {
                    repository_uuid: self.client("/trunk").info().await.unwrap().uuid,
                    child_path: "/branches/child".into(),
                    parent_path: "/trunk".into(),
                    child_revision: 3,
                    child_mapping_revision: 3,
                    parent_baseline_revision: 2,
                },
                repository_root_url: self.root.clone(),
                child_url: format!("{}/branches/child", self.root),
                parent_url: format!("{}/trunk", self.root),
                parent_revision,
            },
        )
        .await
        .unwrap();
        CleanChildSvnPins {
            lineage,
            baseline_git_tree: self.baseline_tree.clone(),
            parent_git_tree: self.parent_tree.clone(),
            previous_mergeinfo: None,
        }
    }

    fn operations(&self) -> PathBuf {
        let path = self.temp.path().join("operations");
        std::fs::create_dir(&path).unwrap();
        path
    }

    async fn assert_unpublished(&self, revision: i64) {
        assert_eq!(
            self.client("/trunk").info().await.unwrap().latest_rev,
            revision
        );
        assert_eq!(
            self.client("/branches/child")
                .last_changed_revision()
                .await
                .unwrap(),
            3
        );
        assert!(self
            .client("/branches/child")
            .tree_properties_at_rev(3)
            .await
            .unwrap()
            .find("svn:mergeinfo")
            .is_none());
        assert!(git2::Repository::open(&self.git)
            .unwrap()
            .is_empty()
            .unwrap());
    }
}

#[test]
fn strict_properties_retain_supported_values_and_comments() {
    let properties = parse_refresh_properties("<properties><target path='url'><property name='svn:mergeinfo'>/trunk:3<!-- retain -->-4</property></target><target path='url/a.sh'><property name='svn:executable'>*</property></target></properties>", "url").unwrap();
    assert_eq!(properties[""]["svn:mergeinfo"], "/trunk:3-4");
    assert_eq!(properties["a.sh"]["svn:executable"], "*");
}

#[test]
fn property_namespace_attribute_aliases_refuse() {
    for xml in [
        "<properties><target xmlns:x='x' x:path='evil' path='url'/></properties>",
        "<properties><target path='url'><property xmlns:x='x' x:name='evil' name='svn:executable'>*</property></target></properties>",
        "<properties xmlns:x='x'><x:target path='url'/></properties>",
        "<properties><target path='url'><property name='svn:executable' encoding='base64'>Kg==</property></target></properties>",
    ] { assert!(parse_refresh_properties(xml,"url").is_err(),"{xml}"); }
}

#[test]
fn properties_refuse_missing_duplicate_nested_and_inherited_evidence() {
    for xml in [
        "<properties/>", "<properties><target path='url'/><target path='url'/></properties>",
        "<properties><target path='url'><property name='p'>a</property><property name='p'>b</property></target></properties>",
        "<properties><target path='url'><property name='p'><nested/></property></target></properties>",
        "<properties><target path='url'><inherited_property name='svn:externals'>outside</inherited_property></target></properties>",
        "<!DOCTYPE properties [<!ENTITY value 'x'>]><properties><target path='url'><property name='p'>&value;</property></target></properties>",
    ] { assert!(parse_refresh_properties(xml,"url").is_err(),"{xml}"); }
}

#[test]
fn property_targets_refuse_ambiguous_and_foreign_paths() {
    for path in [
        "url2/a",
        "url//a",
        "url/a/../b",
        "url/a/",
        "url/.svn/a",
        "url/a%2Fb",
        "url/a b",
    ] {
        let xml = format!("<properties><target path='url'/><target path='{path}'/></properties>");
        assert!(parse_refresh_properties(&xml, "url").is_err(), "{path}");
    }
}

#[test]
fn expected_merge_range_preserves_prior_approved_ranges() {
    assert_eq!(
        expected_refresh_mergeinfo("/trunk", 2, 4, None).unwrap(),
        "/trunk:3-4"
    );
    assert_eq!(
        expected_refresh_mergeinfo("/trunk", 4, 8, Some("/trunk:3-4")).unwrap(),
        "/trunk:3-8"
    );
    assert_eq!(
        expected_refresh_mergeinfo("/trunk", 10, 11, Some("/trunk:3-4,7-8")).unwrap(),
        "/trunk:3-4,7-8,11"
    );
}

#[test]
fn mergeinfo_refuses_other_sources_noninheritance_duplicates_and_future_ranges() {
    for prior in [
        "/other:3-4",
        "/trunk:3-4\n/other:3",
        "/trunk:3*",
        "/trunk:3,3",
        "/trunk:4-3",
        "/trunk:03",
        "/trunk:5",
        "/trunk:1-9223372036854775808",
    ] {
        assert!(
            expected_refresh_mergeinfo("/trunk", 4, 8, Some(prior)).is_err(),
            "{prior}"
        );
    }
    for (source, base, parent) in [
        ("trunk", 2, 4),
        ("/trunk/../other", 2, 4),
        ("/trunk", 0, 4),
        ("/trunk", 4, 4),
    ] {
        assert!(expected_refresh_mergeinfo(source, base, parent, None).is_err());
    }
}

#[test]
fn merge_status_refuses_conflicts_namespaces_and_external_metadata() {
    let status = |item: &str, extra: &str| {
        format!("<status><target path='.'><entry path='a'><wc-status item='{item}' props='none' {extra}/></entry></target></status>")
    };
    require_refresh_merge_status(&status("added", "copied='true'")).unwrap();
    for item in [
        "conflicted",
        "missing",
        "unversioned",
        "obstructed",
        "external",
        "incomplete",
    ] {
        assert!(require_refresh_merge_status(&status(item, "")).is_err());
    }
    for extra in [
        "props2='normal'",
        "xmlns:x='x' x:item='conflicted'",
        "tree-conflicted='true'",
        "switched='true'",
        "file-external='true'",
        "wc-locked='true'",
    ] {
        assert!(require_refresh_merge_status(&status("normal", extra)).is_err());
    }
    for xml in ["<status/>","<status><target path='foreign'/></status>","<status><target path='.'><entry path='a'><wc-status item='normal' props='none'><tree-conflict/></wc-status></entry></target></status>","<status><target path='.'><entry path='a'/></target></status>"] {
        assert!(require_refresh_merge_status(xml).is_err());
    }
}

#[tokio::test]
async fn normal_merge_preserves_bytes_modes_and_mergeinfo_without_publication() {
    let fixture = Fixture::new();
    let pins = fixture.pins(4).await;
    let root = fixture.operations();
    let id = Uuid::new_v4();
    let prepared = prepare_clean_child_svn(
        &fixture.client("/branches/child"),
        &fixture.client("/trunk"),
        &fixture.git,
        &root,
        id,
        &pins,
    )
    .await
    .unwrap();
    assert_eq!(prepared.operation_id, id);
    assert_eq!(prepared.pins, pins);
    assert_eq!(prepared.mergeinfo, "/trunk:3-4");
    assert_eq!(prepared.result_git_tree, fixture.parent_tree);
    assert_eq!(
        prepared.result_properties["binary.dat"]["svn:mime-type"],
        "application/octet-stream"
    );
    assert_eq!(prepared.result_properties["new.sh"]["svn:executable"], "*");
    assert_eq!(
        std::fs::read(prepared.working_copy.join("seed.txt")).unwrap(),
        b"parent advance\n"
    );
    assert_eq!(
        std::fs::read(prepared.working_copy.join("binary.dat")).unwrap(),
        [0, 1, 128, 255]
    );
    assert!(!prepared.working_copy.join("old.sh").exists());
    assert_eq!(
        std::fs::read(prepared.working_copy.join("new.sh")).unwrap(),
        b"echo parent\n"
    );
    fixture.assert_unpublished(4).await;
    // Existing operation state cannot be overwritten by a duplicate preparation.
    assert!(prepare_clean_child_svn(
        &fixture.client("/branches/child"),
        &fixture.client("/trunk"),
        &fixture.git,
        &root,
        id,
        &pins
    )
    .await
    .is_err());
    assert_eq!(
        std::fs::read(prepared.working_copy.join("seed.txt")).unwrap(),
        b"parent advance\n"
    );
}

#[tokio::test]
async fn mismatched_parent_git_content_refuses_before_merge() {
    let fixture = Fixture::new();
    let mut pins = fixture.pins(4).await;
    let root = fixture.operations();
    pins.parent_git_tree = fixture.baseline_tree.clone();
    assert!(prepare_clean_child_svn(
        &fixture.client("/branches/child"),
        &fixture.client("/trunk"),
        &fixture.git,
        &root,
        Uuid::new_v4(),
        &pins
    )
    .await
    .is_err());
    fixture.assert_unpublished(4).await;
}

#[tokio::test]
async fn unsupported_parent_property_refuses_without_remote_change() {
    let fixture = Fixture::new();
    command(
        "svn",
        &[
            "propset",
            "svn:eol-style",
            "LF",
            fixture.parent_wc.join("seed.txt").to_str().unwrap(),
        ],
    );
    command(
        "svn",
        &[
            "commit",
            fixture.parent_wc.to_str().unwrap(),
            "-m",
            "unsupported property",
        ],
    );
    let pins = fixture.pins(5).await;
    let root = fixture.operations();
    assert!(prepare_clean_child_svn(
        &fixture.client("/branches/child"),
        &fixture.client("/trunk"),
        &fixture.git,
        &root,
        Uuid::new_v4(),
        &pins
    )
    .await
    .is_err());
    fixture.assert_unpublished(5).await;
}

#[tokio::test]
async fn executable_property_mismatch_refuses_even_with_identical_bytes() {
    let fixture = Fixture::new();
    let mut pins = fixture.pins(4).await;
    let root = fixture.operations();
    let repo = git2::Repository::open(&fixture.git).unwrap();
    pins.parent_git_tree = tree(
        &repo,
        &[
            ("seed.txt", b"parent advance\n", 0o100644),
            ("new.sh", b"echo parent\n", 0o100644),
            ("binary.dat", &[0, 1, 128, 255], 0o100644),
        ],
    );
    drop(repo);
    assert!(prepare_clean_child_svn(
        &fixture.client("/branches/child"),
        &fixture.client("/trunk"),
        &fixture.git,
        &root,
        Uuid::new_v4(),
        &pins
    )
    .await
    .is_err());
    fixture.assert_unpublished(4).await;
}

#[tokio::test]
async fn stale_client_or_nil_operation_refuses_before_creating_directory() {
    let fixture = Fixture::new();
    let pins = fixture.pins(4).await;
    let root = fixture.operations();
    assert!(prepare_clean_child_svn(
        &fixture.client("/trunk"),
        &fixture.client("/trunk"),
        &fixture.git,
        &root,
        Uuid::new_v4(),
        &pins
    )
    .await
    .is_err());
    assert!(prepare_clean_child_svn(
        &fixture.client("/branches/child"),
        &fixture.client("/trunk"),
        &fixture.git,
        &root,
        Uuid::nil(),
        &pins
    )
    .await
    .is_err());
    assert_eq!(std::fs::read_dir(root).unwrap().count(), 0);
    fixture.assert_unpublished(4).await;
}

#[tokio::test]
async fn svn_only_empty_directory_is_not_silently_discarded() {
    let fixture = Fixture::new();
    command(
        "svn",
        &["mkdir", fixture.parent_wc.join("empty").to_str().unwrap()],
    );
    command(
        "svn",
        &[
            "commit",
            fixture.parent_wc.to_str().unwrap(),
            "-m",
            "empty directory",
        ],
    );
    let pins = fixture.pins(5).await;
    let root = fixture.operations();
    assert!(prepare_clean_child_svn(
        &fixture.client("/branches/child"),
        &fixture.client("/trunk"),
        &fixture.git,
        &root,
        Uuid::new_v4(),
        &pins
    )
    .await
    .is_err());
    fixture.assert_unpublished(5).await;
}

#[tokio::test]
async fn inherited_external_configuration_refuses_before_checkout() {
    let fixture = Fixture::new();
    let root_wc = fixture.temp.path().join("root-wc");
    command(
        "svn",
        &[
            "checkout",
            "--depth",
            "empty",
            &fixture.root,
            root_wc.to_str().unwrap(),
        ],
    );
    command(
        "svn",
        &[
            "propset",
            "svn:externals",
            "^/missing must-not-fetch",
            root_wc.to_str().unwrap(),
        ],
    );
    command(
        "svn",
        &[
            "commit",
            root_wc.to_str().unwrap(),
            "-m",
            "inherited external",
        ],
    );
    let pins = fixture.pins(5).await;
    let root = fixture.operations();
    let id = Uuid::new_v4();
    assert!(prepare_clean_child_svn(
        &fixture.client("/branches/child"),
        &fixture.client("/trunk"),
        &fixture.git,
        &root,
        id,
        &pins
    )
    .await
    .is_err());
    // No checkout exists for the offending parent pin. Its inherited evidence
    // is rejected before SVN can create even an ignored external directory.
    assert!(!root.join(id.to_string()).join("parent").exists());
    fixture.assert_unpublished(5).await;
}

#[tokio::test]
async fn prior_completed_mergeinfo_is_preserved_on_the_next_parent_advance() {
    let fixture = Fixture::new();
    let root = fixture.operations();
    let pins = fixture.pins(4).await;
    let prepared = prepare_clean_child_svn(
        &fixture.client("/branches/child"),
        &fixture.client("/trunk"),
        &fixture.git,
        &root,
        Uuid::new_v4(),
        &pins,
    )
    .await
    .unwrap();
    // Publish only to this disposable fixture to create prior completed evidence.
    let child_revision = fixture
        .client("/branches/child")
        .commit(
            &prepared.working_copy,
            "fixture prior completed refresh",
            "fixture",
        )
        .await
        .unwrap();
    assert_eq!(child_revision, 5);
    std::fs::write(
        fixture.parent_wc.join("seed.txt"),
        b"second parent advance\n",
    )
    .unwrap();
    command(
        "svn",
        &[
            "commit",
            fixture.parent_wc.to_str().unwrap(),
            "-m",
            "second parent advance",
        ],
    );
    let new_tree = tree(
        &git2::Repository::open(&fixture.git).unwrap(),
        &[
            ("seed.txt", b"second parent advance\n", 0o100644),
            ("new.sh", b"echo parent\n", 0o100755),
            ("binary.dat", &[0, 1, 128, 255], 0o100644),
        ],
    );
    let mut lineage_pins = pins.lineage.pins.clone();
    lineage_pins.child.child_revision = 5;
    lineage_pins.child.child_mapping_revision = 5;
    lineage_pins.child.parent_baseline_revision = 4;
    lineage_pins.parent_revision = 6;
    let lineage = inspect_svn_refresh_lineage(
        &fixture.client("/branches/child"),
        &fixture.client("/trunk"),
        &lineage_pins,
    )
    .await
    .unwrap();
    let next_pins = CleanChildSvnPins {
        lineage,
        baseline_git_tree: fixture.parent_tree.clone(),
        parent_git_tree: new_tree.clone(),
        previous_mergeinfo: Some(prepared.mergeinfo),
    };
    let next = prepare_clean_child_svn(
        &fixture.client("/branches/child"),
        &fixture.client("/trunk"),
        &fixture.git,
        &root,
        Uuid::new_v4(),
        &next_pins,
    )
    .await
    .unwrap();
    assert_eq!(next.mergeinfo, "/trunk:3-6");
    assert_eq!(next.result_git_tree, new_tree);
    assert_eq!(
        std::fs::read(next.working_copy.join("seed.txt")).unwrap(),
        b"second parent advance\n"
    );
    assert_eq!(fixture.client("/trunk").info().await.unwrap().latest_rev, 6);
    assert_eq!(
        fixture
            .client("/branches/child")
            .last_changed_revision()
            .await
            .unwrap(),
        5
    );
}
