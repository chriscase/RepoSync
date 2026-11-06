//! Read-only copy-lineage controls against real svnadmin repositories.

use std::path::Path;
use std::process::Command;

use reposync_core::pair_refresh::lineage::{inspect_svn_copy_origin, SvnLineagePins};
use reposync_core::svn::SvnClient;

struct Fixture {
    temp: tempfile::TempDir,
    root: String,
}

fn command(binary: &str, args: &[&str]) -> String {
    let output = Command::new(binary)
        .args(args)
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{binary} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

impl Fixture {
    fn new(copy: bool, source: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("svn-repo");
        command("svnadmin", &["create", repo.to_str().unwrap()]);
        let root = reqwest::Url::from_directory_path(&repo)
            .unwrap()
            .to_string()
            .trim_end_matches('/')
            .to_owned();
        command(
            "svn",
            &[
                "mkdir",
                &format!("{root}/trunk"),
                &format!("{root}/other"),
                &format!("{root}/branches"),
                "-m",
                "fixture layout",
            ],
        );
        if copy {
            command(
                "svn",
                &[
                    "copy",
                    &format!("{root}{source}@1"),
                    &format!("{root}/branches/child"),
                    "-m",
                    "fixture originating copy",
                ],
            );
        } else {
            command(
                "svn",
                &[
                    "mkdir",
                    &format!("{root}/branches/child"),
                    "-m",
                    "fixture plain directory",
                ],
            );
        }
        Self { temp, root }
    }

    fn client(&self, path: &str) -> SvnClient {
        SvnClient::new(format!("{}{path}", self.root), "", "")
    }

    async fn pins(&self) -> SvnLineagePins {
        SvnLineagePins {
            repository_uuid: self.client("/trunk").info().await.unwrap().uuid,
            child_path: "/branches/child".into(),
            parent_path: "/trunk".into(),
            child_revision: 2,
            child_mapping_revision: 2,
            parent_baseline_revision: 1,
        }
    }

    fn checkout(&self, path: &str, destination: &Path) {
        command(
            "svn",
            &[
                "checkout",
                &format!("{}{path}", self.root),
                destination.to_str().unwrap(),
            ],
        );
    }
}

#[tokio::test]
async fn direct_copy_origin_is_pinned_and_complete_beyond_old_window() {
    let fixture = Fixture::new(true, "/trunk");
    let wc = fixture.temp.path().join("child-wc");
    fixture.checkout("/branches/child", &wc);
    for number in 0..12 {
        let file = wc.join(format!("file-{number}.txt"));
        std::fs::write(&file, format!("child history {number}\n")).unwrap();
        command("svn", &["add", file.to_str().unwrap()]);
        command(
            "svn",
            &["commit", "-m", "fixture child change", wc.to_str().unwrap()],
        );
    }
    let client = fixture.client("/branches/child");
    let mut pins = fixture.pins().await;
    pins.child_revision = 14;
    let proof = inspect_svn_copy_origin(&client, &pins).await.unwrap();
    assert_eq!(proof.created_revision, 2);
    assert_eq!(proof.copy_from_revision, 1);
    assert_eq!(proof.inspected_revision, 14);
    assert_eq!(proof.parent_path, "/trunk");
    assert_eq!(client.last_changed_revision().await.unwrap(), 14);
}

#[tokio::test]
async fn plain_directory_is_not_copy_lineage() {
    let fixture = Fixture::new(false, "/trunk");
    assert!(
        inspect_svn_copy_origin(&fixture.client("/branches/child"), &fixture.pins().await,)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn identical_tree_copied_from_another_source_is_refused() {
    let fixture = Fixture::new(true, "/other");
    assert!(
        inspect_svn_copy_origin(&fixture.client("/branches/child"), &fixture.pins().await,)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn deleted_and_recopied_child_does_not_inherit_old_mapping() {
    let fixture = Fixture::new(true, "/trunk");
    command(
        "svn",
        &[
            "delete",
            &format!("{}/branches/child", fixture.root),
            "-m",
            "fixture deletion",
        ],
    );
    command(
        "svn",
        &[
            "copy",
            &format!("{}/trunk@1", fixture.root),
            &format!("{}/branches/child", fixture.root),
            "-m",
            "fixture new incarnation",
        ],
    );
    let mut pins = fixture.pins().await;
    pins.child_revision = 4;
    assert!(
        inspect_svn_copy_origin(&fixture.client("/branches/child"), &pins)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn same_commit_replacement_is_refused_even_with_new_mapping() {
    let fixture = Fixture::new(true, "/trunk");
    let wc = fixture.temp.path().join("root-wc");
    fixture.checkout("", &wc);
    let child = wc.join("branches/child");
    command("svn", &["delete", child.to_str().unwrap()]);
    command(
        "svn",
        &[
            "copy",
            &format!("{}/trunk@1", fixture.root),
            child.to_str().unwrap(),
        ],
    );
    command(
        "svn",
        &["commit", "-m", "fixture replacement", wc.to_str().unwrap()],
    );
    let mut pins = fixture.pins().await;
    pins.child_revision = 3;
    pins.child_mapping_revision = 3;
    assert!(
        inspect_svn_copy_origin(&fixture.client("/branches/child"), &pins)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn renamed_child_is_not_a_direct_parent_copy() {
    let fixture = Fixture::new(true, "/trunk");
    command(
        "svn",
        &[
            "move",
            &format!("{}/branches/child", fixture.root),
            &format!("{}/branches/moved", fixture.root),
            "-m",
            "fixture rename",
        ],
    );
    let mut pins = fixture.pins().await;
    pins.child_path = "/branches/moved".into();
    pins.child_revision = 3;
    pins.child_mapping_revision = 3;
    assert!(
        inspect_svn_copy_origin(&fixture.client("/branches/moved"), &pins)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn old_peg_remains_observable_after_current_path_deletion() {
    let fixture = Fixture::new(true, "/trunk");
    let pins = fixture.pins().await;
    command(
        "svn",
        &[
            "delete",
            &format!("{}/branches/child", fixture.root),
            "-m",
            "fixture later deletion",
        ],
    );
    let proof = inspect_svn_copy_origin(&fixture.client("/branches/child"), &pins)
        .await
        .unwrap();
    assert_eq!(proof.inspected_revision, 2);
    assert_eq!(proof.created_revision, 2);
    assert!(fixture.client("/branches/child").info().await.is_err());
}

#[tokio::test]
async fn wrong_uuid_and_pegged_path_are_refused() {
    let fixture = Fixture::new(true, "/trunk");
    let mut pins = fixture.pins().await;
    pins.repository_uuid = "another-repository".into();
    assert!(
        inspect_svn_copy_origin(&fixture.client("/branches/child"), &pins)
            .await
            .is_err()
    );
    pins = fixture.pins().await;
    pins.child_path = "/branches/other".into();
    assert!(
        inspect_svn_copy_origin(&fixture.client("/branches/child"), &pins)
            .await
            .is_err()
    );
}
