//! Parent incarnation controls against disposable real SVN repositories.

use std::path::{Path, PathBuf};
use std::process::Command;

use reposync_core::pair_refresh::lineage::{
    inspect_svn_refresh_lineage, SvnLineagePins, SvnRefreshLineagePins,
};
use reposync_core::svn::SvnClient;

struct Fixture {
    temp: tempfile::TempDir,
    root: String,
    parent_path: String,
}

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

impl Fixture {
    fn new(parent_path: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let repository = temp.path().join("repository");
        command("svnadmin", &["create", repository.to_str().unwrap()]);
        let root = reqwest::Url::from_directory_path(repository)
            .unwrap()
            .to_string()
            .trim_end_matches('/')
            .to_owned();
        command(
            "svn",
            &[
                "mkdir",
                "--parents",
                &format!("{root}{parent_path}"),
                &format!("{root}/branches"),
                &format!("{root}/other"),
                "-m",
                "fixture layout",
            ],
        );
        command(
            "svn",
            &[
                "copy",
                &format!("{root}{parent_path}@1"),
                &format!("{root}/branches/child"),
                "-m",
                "fixture child origin",
            ],
        );
        Self {
            temp,
            root,
            parent_path: parent_path.into(),
        }
    }

    fn client(&self, path: &str) -> SvnClient {
        SvnClient::new(format!("{}{path}", self.root), "", "")
    }

    async fn pins(&self, revision: i64) -> SvnRefreshLineagePins {
        SvnRefreshLineagePins {
            child: SvnLineagePins {
                repository_uuid: self.client(&self.parent_path).info().await.unwrap().uuid,
                child_path: "/branches/child".into(),
                parent_path: self.parent_path.clone(),
                child_revision: revision,
                child_mapping_revision: 2,
                parent_baseline_revision: 1,
            },
            repository_root_url: self.root.clone(),
            child_url: format!("{}/branches/child", self.root),
            parent_url: format!("{}{}", self.root, self.parent_path),
            parent_revision: revision,
        }
    }

    fn checkout(&self, path: &str, name: &str) -> PathBuf {
        let destination = self.temp.path().join(name);
        command(
            "svn",
            &[
                "checkout",
                &format!("{}{path}", self.root),
                destination.to_str().unwrap(),
            ],
        );
        destination
    }

    fn commit(&self, directory: &Path) {
        command(
            "svn",
            &[
                "commit",
                "-m",
                "fixture parent change",
                directory.to_str().unwrap(),
            ],
        );
    }
}

#[tokio::test]
async fn proof_echoes_all_location_and_revision_pins() {
    let fixture = Fixture::new("/trunk");
    let pins = fixture.pins(2).await;
    let proof = inspect_svn_refresh_lineage(
        &fixture.client("/branches/child"),
        &fixture.client("/trunk"),
        &pins,
    )
    .await
    .unwrap();
    assert_eq!(proof.pins, pins);
    assert_eq!(proof.child_copy.copy_from_revision, 1);
    assert_eq!(proof.parent_originating_revision, 1);
}

#[tokio::test]
async fn advanced_parent_proves_origin_beyond_short_history_window() {
    let fixture = Fixture::new("/trunk");
    let wc = fixture.checkout("/trunk", "parent-wc");
    let file = wc.join("file.txt");
    for number in 0..12 {
        std::fs::write(&file, format!("parent revision {number}\n")).unwrap();
        if number == 0 {
            command("svn", &["add", file.to_str().unwrap()]);
        }
        fixture.commit(&wc);
    }
    let mut pins = fixture.pins(14).await;
    pins.child.parent_baseline_revision = 8;
    let proof = inspect_svn_refresh_lineage(
        &fixture.client("/branches/child"),
        &fixture.client("/trunk"),
        &pins,
    )
    .await
    .unwrap();
    assert_eq!(proof.parent_originating_revision, 1);
    assert_eq!(proof.pins.parent_revision, 14);
    assert_eq!(proof.pins.child.parent_baseline_revision, 8);
}

#[tokio::test]
async fn descendant_addition_and_deletion_keep_parent_incarnation() {
    let fixture = Fixture::new("/trunk");
    let wc = fixture.checkout("/trunk", "parent-wc");
    let file = wc.join("temporary.txt");
    std::fs::write(&file, "temporary parent content\n").unwrap();
    command("svn", &["add", file.to_str().unwrap()]);
    fixture.commit(&wc);
    command("svn", &["delete", file.to_str().unwrap()]);
    fixture.commit(&wc);
    let proof = inspect_svn_refresh_lineage(
        &fixture.client("/branches/child"),
        &fixture.client("/trunk"),
        &fixture.pins(4).await,
    )
    .await
    .unwrap();
    assert_eq!(proof.parent_originating_revision, 1);
}

#[tokio::test]
async fn same_commit_parent_replacement_refuses_identical_tree() {
    let fixture = Fixture::new("/trunk");
    let wc = fixture.checkout("", "root-wc");
    let parent = wc.join("trunk");
    command("svn", &["delete", parent.to_str().unwrap()]);
    command(
        "svn",
        &[
            "copy",
            &format!("{}/other@1", fixture.root),
            parent.to_str().unwrap(),
        ],
    );
    fixture.commit(&wc);
    assert!(inspect_svn_refresh_lineage(
        &fixture.client("/branches/child"),
        &fixture.client("/trunk"),
        &fixture.pins(3).await,
    )
    .await
    .is_err());
}

#[tokio::test]
async fn deleted_and_recreated_parent_refuses_old_copy_source() {
    let fixture = Fixture::new("/trunk");
    command(
        "svn",
        &[
            "delete",
            &format!("{}/trunk", fixture.root),
            "-m",
            "fixture deletion",
        ],
    );
    command(
        "svn",
        &[
            "mkdir",
            &format!("{}/trunk", fixture.root),
            "-m",
            "fixture new parent",
        ],
    );
    assert!(inspect_svn_refresh_lineage(
        &fixture.client("/branches/child"),
        &fixture.client("/trunk"),
        &fixture.pins(4).await,
    )
    .await
    .is_err());
}

#[tokio::test]
async fn parent_rename_out_and_back_refuses_old_incarnation() {
    let fixture = Fixture::new("/trunk");
    for (source, target) in [("/trunk", "/moved"), ("/moved", "/trunk")] {
        command(
            "svn",
            &[
                "move",
                &format!("{}{source}", fixture.root),
                &format!("{}{target}", fixture.root),
                "-m",
                "fixture parent move",
            ],
        );
    }
    assert!(inspect_svn_refresh_lineage(
        &fixture.client("/branches/child"),
        &fixture.client("/trunk"),
        &fixture.pins(4).await,
    )
    .await
    .is_err());
}

#[tokio::test]
async fn enclosing_directory_replacement_refuses_parent() {
    let fixture = Fixture::new("/source/trunk");
    let wc = fixture.checkout("", "root-wc");
    let enclosing = wc.join("source");
    command("svn", &["delete", enclosing.to_str().unwrap()]);
    command(
        "svn",
        &[
            "copy",
            &format!("{}/source@1", fixture.root),
            enclosing.to_str().unwrap(),
        ],
    );
    fixture.commit(&wc);
    assert!(inspect_svn_refresh_lineage(
        &fixture.client("/branches/child"),
        &fixture.client("/source/trunk"),
        &fixture.pins(3).await,
    )
    .await
    .is_err());
}

#[tokio::test]
async fn mismatched_client_root_uuid_and_range_refuse() {
    let fixture = Fixture::new("/trunk");
    let child = fixture.client("/branches/child");
    let parent = fixture.client("/trunk");
    let original = fixture.pins(2).await;
    assert!(
        inspect_svn_refresh_lineage(&child, &fixture.client("/other"), &original)
            .await
            .is_err()
    );
    for alteration in 0..5 {
        let mut pins = original.clone();
        match alteration {
            0 => pins.repository_root_url.push_str("/another-root"),
            1 => pins.child.repository_uuid = "another-uuid".into(),
            2 => pins.parent_url.push_str("/another-path"),
            3 => pins.child.parent_baseline_revision = 3,
            4 => pins.child_url.push_str("/another-path"),
            _ => unreachable!(),
        }
        assert!(inspect_svn_refresh_lineage(&child, &parent, &pins)
            .await
            .is_err());
    }
}

#[tokio::test]
async fn historical_parent_peg_does_not_claim_current_freshness() {
    let fixture = Fixture::new("/trunk");
    let pins = fixture.pins(2).await;
    command(
        "svn",
        &[
            "delete",
            &format!("{}/trunk", fixture.root),
            "-m",
            "fixture later deletion",
        ],
    );
    let proof = inspect_svn_refresh_lineage(
        &fixture.client("/branches/child"),
        &fixture.client("/trunk"),
        &pins,
    )
    .await
    .unwrap();
    assert_eq!(proof.pins.parent_revision, 2);
    assert!(fixture.client("/trunk").info().await.is_err());
}
