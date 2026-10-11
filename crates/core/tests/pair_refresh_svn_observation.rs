//! Recovery observations use only disposable local SVN and Git repositories.
//! The fixture commits normal prepared merges; production observation is read-only.

use std::path::PathBuf;
use std::process::Command;

use reposync_core::pair_refresh::lineage::{
    inspect_svn_refresh_lineage, SvnLineagePins, SvnRefreshLineagePins,
};
use reposync_core::pair_refresh::svn_observation::{
    observe_prepared_child_svn, refresh_svn_commit_message, SvnPublicationObservation,
};
use reposync_core::pair_refresh::svn_preparation::{
    prepare_clean_child_svn, CleanChildSvnPins, PreparedCleanChildSvn,
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

impl Fixture {
    async fn prepared(&self) -> PreparedCleanChildSvn {
        prepare_clean_child_svn(
            &self.client("/branches/child"),
            &self.client("/trunk"),
            &self.git,
            &self.operations(),
            Uuid::new_v4(),
            &self.pins(4).await,
        )
        .await
        .unwrap()
    }

    async fn observe(
        &self,
        prepared: &PreparedCleanChildSvn,
    ) -> Result<SvnPublicationObservation, reposync_core::errors::SvnError> {
        observe_prepared_child_svn(
            &self.client("/branches/child"),
            &self.client("/trunk"),
            &self.git,
            self.temp.path(),
            prepared,
        )
        .await
    }

    async fn publish(&self, prepared: &PreparedCleanChildSvn) -> i64 {
        self.client("/branches/child")
            .commit(
                &prepared.working_copy,
                &refresh_svn_commit_message(prepared.operation_id).unwrap(),
                "",
            )
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn absent_marker_is_only_not_observed_and_creates_no_revision() {
    let fixture = Fixture::new();
    let prepared = fixture.prepared().await;
    assert_eq!(
        fixture.observe(&prepared).await.unwrap(),
        SvnPublicationObservation::NotObserved
    );
    fixture.assert_unpublished(4).await;
}

#[tokio::test]
async fn lost_success_response_is_recovered_from_history_without_second_write() {
    let fixture = Fixture::new();
    let prepared = fixture.prepared().await;
    // The disposable child receives the real SVN commit, then the wrapper
    // discards its successful receipt and returns an explicit transport error.
    let output = Command::new("sh")
        .args([
            "-c",
            "svn commit \"$1\" -m \"$2\" >/dev/null; result=$?; [ \"$result\" -eq 0 ] || exit \"$result\"; printf 'simulated lost commit response\\n' >&2; exit 75",
            "lost-svn-receipt",
            prepared.working_copy.to_str().unwrap(),
            &refresh_svn_commit_message(prepared.operation_id).unwrap(),
        ])
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(75));
    assert!(String::from_utf8_lossy(&output.stderr).contains("simulated lost commit response"));
    let first = fixture.observe(&prepared).await.unwrap();
    let SvnPublicationObservation::Observed(receipt) = &first else {
        panic!("lost response did not yield proven publication: {first:?}");
    };
    assert_eq!(receipt.revision, 5);
    assert_eq!(fixture.observe(&prepared).await.unwrap(), first);
    let history = fixture
        .client("/branches/child")
        .copy_history_at_rev(5)
        .await
        .unwrap();
    assert_eq!(history.iter().filter(|entry| entry.revision > 3).count(), 1);
    assert_eq!(
        fixture
            .client("/branches/child")
            .info()
            .await
            .unwrap()
            .latest_rev,
        5
    );
}

#[tokio::test]
async fn marker_identifies_exact_revision_despite_unrelated_repository_head() {
    let fixture = Fixture::new();
    let prepared = fixture.prepared().await;
    command(
        "svn",
        &[
            "mkdir",
            &format!("{}/other", fixture.root),
            "-m",
            "unrelated before",
        ],
    );
    let revision = fixture.publish(&prepared).await;
    assert_eq!(revision, 6);
    command(
        "svn",
        &[
            "mkdir",
            &format!("{}/other/nested", fixture.root),
            "-m",
            "unrelated after",
        ],
    );
    let receipt = match fixture.observe(&prepared).await.unwrap() {
        SvnPublicationObservation::Observed(receipt) => receipt,
        other => panic!("missing publication receipt: {other:?}"),
    };
    assert_eq!(receipt.revision, revision);
    assert_ne!(
        receipt.revision,
        prepared.pins.lineage.pins.child.child_revision + 1
    );
    assert_ne!(
        receipt.revision,
        fixture
            .client("/branches/child")
            .info()
            .await
            .unwrap()
            .latest_rev
    );
    assert_eq!(receipt.operation_id, prepared.operation_id);
    assert_eq!(receipt.git_tree, prepared.result_git_tree);
    assert_eq!(receipt.properties, prepared.result_properties);
    assert_eq!(receipt.mergeinfo, "/trunk:3-4");
    assert_eq!(receipt.child_copy.created_revision, 3);
    assert_eq!(receipt.child_copy.copy_from_revision, 2);
    assert_eq!(receipt.child_copy.inspected_revision, revision);
    assert_eq!(
        fixture.observe(&prepared).await.unwrap(),
        SvnPublicationObservation::Observed(receipt)
    );
    for (path, revision) in [("/trunk", 2), ("/branches/child", 3), ("/trunk", 4)] {
        assert_eq!(
            fixture
                .client(path)
                .info_at_rev(revision)
                .await
                .unwrap()
                .latest_rev,
            revision
        );
    }
    assert!(git2::Repository::open(&fixture.git)
        .unwrap()
        .is_empty()
        .unwrap());
}

#[tokio::test]
async fn quoted_marker_with_wrong_message_is_not_a_receipt() {
    let fixture = Fixture::new();
    let prepared = fixture.prepared().await;
    let message = format!(
        "{}\nquoted from another operation",
        refresh_svn_commit_message(prepared.operation_id).unwrap()
    );
    fixture
        .client("/branches/child")
        .commit(&prepared.working_copy, &message, "")
        .await
        .unwrap();
    let error = fixture.observe(&prepared).await.unwrap_err();
    assert!(error.to_string().contains("marker is ambiguous"), "{error}");
}

#[tokio::test]
async fn duplicate_operation_marker_refuses_recovery() {
    let fixture = Fixture::new();
    let prepared = fixture.prepared().await;
    fixture.publish(&prepared).await;
    fixture
        .client("/branches/child")
        .run_svn_in_dir_public(
            &prepared.working_copy,
            &["propset", "svn:mergeinfo", "/trunk:3-5", "."],
        )
        .await
        .unwrap();
    fixture.publish(&prepared).await;
    let error = fixture.observe(&prepared).await.unwrap_err();
    assert!(error.to_string().contains("marker is ambiguous"), "{error}");
}

#[tokio::test]
async fn marker_transaction_that_also_changes_parent_refuses() {
    let fixture = Fixture::new();
    let prepared = fixture.prepared().await;
    std::fs::write(
        fixture.parent_wc.join("seed.txt"),
        b"unapproved second parent change\n",
    )
    .unwrap();
    command(
        "svn",
        &[
            "commit",
            prepared.working_copy.to_str().unwrap(),
            fixture.parent_wc.to_str().unwrap(),
            "-m",
            &refresh_svn_commit_message(prepared.operation_id).unwrap(),
        ],
    );
    let error = fixture.observe(&prepared).await.unwrap_err();
    assert!(error.to_string().contains("unapproved path"), "{error}");
}

#[tokio::test]
async fn later_child_file_change_refuses_recovery_of_earlier_marker() {
    let fixture = Fixture::new();
    let prepared = fixture.prepared().await;
    let published = fixture.publish(&prepared).await;
    std::fs::write(
        prepared.working_copy.join("seed.txt"),
        b"later child work\n",
    )
    .unwrap();
    let later = fixture
        .client("/branches/child")
        .commit(&prepared.working_copy, "later child work", "")
        .await
        .unwrap();
    assert_eq!(later, published + 1);
    assert_eq!(
        fixture
            .client("/branches/child")
            .last_changed_revision()
            .await
            .unwrap(),
        later
    );
    let error = fixture.observe(&prepared).await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("outside the one approved publication"),
        "{error}"
    );
}

#[tokio::test]
async fn another_operations_publication_is_drift_instead_of_not_observed() {
    let fixture = Fixture::new();
    let prepared = fixture.prepared().await;
    let mut other = prepared.clone();
    other.operation_id = Uuid::new_v4();
    fixture.publish(&other).await;
    let error = fixture.observe(&prepared).await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("outside the one approved publication"),
        "{error}"
    );
}

#[tokio::test]
async fn matching_tree_copied_from_another_branch_refuses_history_substitution() {
    let fixture = Fixture::new();
    let prepared = fixture.prepared().await;
    command(
        "svn",
        &[
            "copy",
            &format!("{}/trunk@4", fixture.root),
            &format!("{}/other", fixture.root),
            "-m",
            "other copy",
        ],
    );
    let path = prepared.working_copy.join("new.sh");
    command("svn", &["delete", "--force", path.to_str().unwrap()]);
    command(
        "svn",
        &[
            "copy",
            &format!("{}/other/new.sh@5", fixture.root),
            path.to_str().unwrap(),
        ],
    );
    fixture.publish(&prepared).await;
    let error = fixture.observe(&prepared).await.unwrap_err();
    assert!(error.to_string().contains("copy source differs"), "{error}");
}

#[tokio::test]
async fn matching_added_bytes_without_parent_copy_history_refuses() {
    let fixture = Fixture::new();
    let prepared = fixture.prepared().await;
    let path = prepared.working_copy.join("new.sh");
    command("svn", &["delete", "--force", path.to_str().unwrap()]);
    std::fs::write(&path, b"echo parent\n").unwrap();
    command("svn", &["add", path.to_str().unwrap()]);
    command(
        "svn",
        &["propset", "svn:executable", "*", path.to_str().unwrap()],
    );
    fixture.publish(&prepared).await;
    let error = fixture.observe(&prepared).await.unwrap_err();
    assert!(error.to_string().contains("copy source differs"), "{error}");
}

#[tokio::test]
async fn multi_revision_parent_advance_preserves_nested_normal_merge_copy_sources() {
    let mut fixture = Fixture::new();
    let directory = fixture.parent_wc.join("nested");
    std::fs::create_dir(&directory).unwrap();
    std::fs::write(directory.join("added.txt"), b"next parent revision\n").unwrap();
    command("svn", &["add", directory.to_str().unwrap()]);
    command(
        "svn",
        &[
            "commit",
            fixture.parent_wc.to_str().unwrap(),
            "-m",
            "nested parent advance",
        ],
    );
    let repo = git2::Repository::open(&fixture.git).unwrap();
    let parent = repo
        .find_tree(git2::Oid::from_str(&fixture.parent_tree).unwrap())
        .unwrap();
    let mut nested = repo.treebuilder(None).unwrap();
    nested
        .insert(
            "added.txt",
            repo.blob(b"next parent revision\n").unwrap(),
            0o100644,
        )
        .unwrap();
    let mut result = repo.treebuilder(Some(&parent)).unwrap();
    result
        .insert("nested", nested.write().unwrap(), 0o040000)
        .unwrap();
    fixture.parent_tree = result.write().unwrap().to_string();
    let prepared = prepare_clean_child_svn(
        &fixture.client("/branches/child"),
        &fixture.client("/trunk"),
        &fixture.git,
        &fixture.operations(),
        Uuid::new_v4(),
        &fixture.pins(5).await,
    )
    .await
    .unwrap();
    let published = fixture.publish(&prepared).await;
    let SvnPublicationObservation::Observed(receipt) = fixture.observe(&prepared).await.unwrap()
    else {
        panic!("nested normal merge was not observed");
    };
    assert_eq!(receipt.revision, published);
    assert_eq!(receipt.mergeinfo, "/trunk:3-5");
    assert_eq!(receipt.git_tree, fixture.parent_tree);
    let changes = fixture
        .client("/branches/child")
        .copy_history_at_rev(published)
        .await
        .unwrap();
    let added = changes[0]
        .changed_paths
        .iter()
        .find(|change| change.path == "/branches/child/nested")
        .unwrap();
    assert_eq!(added.copy_from_path.as_deref(), Some("/trunk/nested"));
    assert_eq!(added.copy_from_rev, Some(5));
}

#[tokio::test]
async fn next_parent_advance_recovers_with_prior_mergeinfo_and_old_marker_intact() {
    let fixture = Fixture::new();
    let first = fixture.prepared().await;
    let first_revision = fixture.publish(&first).await;
    let SvnPublicationObservation::Observed(first_receipt) = fixture.observe(&first).await.unwrap()
    else {
        panic!("first merge was not observed");
    };
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
    let repo = git2::Repository::open(&fixture.git).unwrap();
    let previous_tree = repo
        .find_tree(git2::Oid::from_str(&fixture.parent_tree).unwrap())
        .unwrap();
    let mut next_tree = repo.treebuilder(Some(&previous_tree)).unwrap();
    next_tree
        .insert(
            "seed.txt",
            repo.blob(b"second parent advance\n").unwrap(),
            0o100644,
        )
        .unwrap();
    let next_tree = next_tree.write().unwrap().to_string();
    let mut next_lineage = first.pins.lineage.pins.clone();
    next_lineage.child.child_revision = first_revision;
    next_lineage.child.child_mapping_revision = first_revision;
    next_lineage.child.parent_baseline_revision = 4;
    next_lineage.parent_revision = 6;
    let pins = CleanChildSvnPins {
        lineage: inspect_svn_refresh_lineage(
            &fixture.client("/branches/child"),
            &fixture.client("/trunk"),
            &next_lineage,
        )
        .await
        .unwrap(),
        baseline_git_tree: fixture.parent_tree.clone(),
        parent_git_tree: next_tree.clone(),
        previous_mergeinfo: Some(first_receipt.mergeinfo.clone()),
    };
    let second = prepare_clean_child_svn(
        &fixture.client("/branches/child"),
        &fixture.client("/trunk"),
        &fixture.git,
        &fixture.temp.path().join("operations"),
        Uuid::new_v4(),
        &pins,
    )
    .await
    .unwrap();
    let second_revision = fixture.publish(&second).await;
    let SvnPublicationObservation::Observed(second_receipt) =
        fixture.observe(&second).await.unwrap()
    else {
        panic!("second merge was not observed");
    };
    assert_eq!(second_revision, 7);
    assert_eq!(second_receipt.revision, second_revision);
    assert_eq!(second_receipt.mergeinfo, "/trunk:3-6");
    assert_eq!(second_receipt.git_tree, next_tree);
    assert_ne!(first_receipt.operation_id, second_receipt.operation_id);
    assert_eq!(
        first_receipt.child_copy.created_revision,
        second_receipt.child_copy.created_revision
    );
    let history = fixture
        .client("/branches/child")
        .copy_history_at_rev(second_revision)
        .await
        .unwrap();
    assert_eq!(history.iter().filter(|entry| entry.message == refresh_svn_commit_message(first.operation_id).unwrap()).count(), 1);
    assert_eq!(
        history
            .iter()
            .filter(
                |entry| entry.message == refresh_svn_commit_message(second.operation_id).unwrap()
            )
            .count(),
        1
    );
    assert_eq!(history.last().unwrap().revision, 3);
    assert!(fixture.observe(&first).await.is_err());
    assert_eq!(
        fixture.observe(&second).await.unwrap(),
        SvnPublicationObservation::Observed(second_receipt)
    );
}

#[tokio::test]
async fn marker_without_root_mergeinfo_change_refuses_receipt() {
    let fixture = Fixture::new();
    let prepared = fixture.prepared().await;
    fixture
        .client("/branches/child")
        .run_svn_in_dir_public(&prepared.working_copy, &["propdel", "svn:mergeinfo", "."])
        .await
        .unwrap();
    fixture.publish(&prepared).await;
    let error = fixture.observe(&prepared).await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("omitted the root mergeinfo change"),
        "{error}"
    );
}

#[tokio::test]
async fn marker_and_mergeinfo_do_not_substitute_for_full_content_proof() {
    let fixture = Fixture::new();
    let prepared = fixture.prepared().await;
    std::fs::write(
        prepared.working_copy.join("seed.txt"),
        b"wrong publication bytes\n",
    )
    .unwrap();
    fixture.publish(&prepared).await;
    let error = fixture.observe(&prepared).await.unwrap_err();
    assert!(error.to_string().contains("content differs"), "{error}");
}

#[tokio::test]
async fn marker_and_tree_do_not_substitute_for_full_property_proof() {
    let fixture = Fixture::new();
    let prepared = fixture.prepared().await;
    fixture
        .client("/branches/child")
        .run_svn_in_dir_public(
            &prepared.working_copy,
            &["propset", "svn:mime-type", "text/plain", "seed.txt"],
        )
        .await
        .unwrap();
    fixture.publish(&prepared).await;
    let error = fixture.observe(&prepared).await.unwrap_err();
    assert!(
        error.to_string().contains("published properties differ"),
        "{error}"
    );
}

#[tokio::test]
async fn child_replacement_after_publication_refuses_the_old_operation() {
    let fixture = Fixture::new();
    let prepared = fixture.prepared().await;
    fixture.publish(&prepared).await;
    command(
        "svn",
        &[
            "delete",
            &format!("{}/branches/child", fixture.root),
            "-m",
            "remove fixture child",
        ],
    );
    command(
        "svn",
        &[
            "copy",
            &format!("{}/trunk@4", fixture.root),
            &format!("{}/branches/child", fixture.root),
            "-m",
            "replacement fixture child",
        ],
    );
    assert!(fixture.observe(&prepared).await.is_err());
}

#[tokio::test]
async fn corrupted_preparation_receipt_refuses_before_any_observation_checkout() {
    let fixture = Fixture::new();
    let prepared = fixture.prepared().await;
    fixture.publish(&prepared).await;
    let before = std::fs::read_dir(fixture.temp.path()).unwrap().count();
    let mut missing_identity = prepared.clone();
    missing_identity.operation_id = Uuid::nil();
    assert!(fixture.observe(&missing_identity).await.is_err());
    let mut wrong_tree = prepared.clone();
    wrong_tree.result_git_tree = fixture.baseline_tree.clone();
    assert!(fixture.observe(&wrong_tree).await.is_err());
    let mut wrong_properties = prepared.clone();
    wrong_properties
        .result_properties
        .get_mut("")
        .unwrap()
        .insert("svn:mergeinfo".into(), "/trunk:3-99".into());
    assert!(fixture.observe(&wrong_properties).await.is_err());
    assert_eq!(
        std::fs::read_dir(fixture.temp.path()).unwrap().count(),
        before
    );
}

#[tokio::test]
async fn root_mergeinfo_change_rejects_second_prepared_commit_at_the_old_base() {
    let fixture = Fixture::new();
    let first = fixture.prepared().await;
    let second = prepare_clean_child_svn(
        &fixture.client("/branches/child"),
        &fixture.client("/trunk"),
        &fixture.git,
        &fixture.temp.path().join("operations"),
        Uuid::new_v4(),
        &first.pins,
    )
    .await
    .unwrap();
    let revision = fixture.publish(&first).await;
    let error = fixture
        .client("/branches/child")
        .commit(
            &second.working_copy,
            &refresh_svn_commit_message(second.operation_id).unwrap(),
            "",
        )
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("E160028") || error.to_string().contains("E155011"),
        "{error}"
    );
    assert_eq!(
        fixture
            .client("/branches/child")
            .info()
            .await
            .unwrap()
            .latest_rev,
        revision
    );
    assert!(matches!(
        fixture.observe(&first).await.unwrap(),
        SvnPublicationObservation::Observed(_)
    ));
    assert!(fixture.observe(&second).await.is_err());
}
