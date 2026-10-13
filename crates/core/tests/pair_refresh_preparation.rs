//! Object-level proof for bounded, history-preserving Git merge preparation.

use git2::{Oid, Repository, Signature};
use reposync_core::pair_refresh::preparation::{prepare_clean_child_git, CleanChildGitPins};
use uuid::Uuid;

const PREPARED_AT: i64 = 1_791_712_815;

struct Fixture {
    _temp: tempfile::TempDir,
    repo: Repository,
    baseline: Oid,
    child: Oid,
    parent: Oid,
}

fn tree(repo: &Repository, contents: &[u8]) -> Oid {
    let blob = repo.blob(contents).unwrap();
    let mut tree = repo.treebuilder(None).unwrap();
    tree.insert("file.txt", blob, 0o100644).unwrap();
    tree.write().unwrap()
}

fn commit(repo: &Repository, tree: Oid, parents: &[Oid], message: &str) -> Oid {
    let tree = repo.find_tree(tree).unwrap();
    let parents: Vec<_> = parents
        .iter()
        .map(|id| repo.find_commit(*id).unwrap())
        .collect();
    let parents: Vec<_> = parents.iter().collect();
    let signature = Signature::now("Preparation fixture", "fixture@example.invalid").unwrap();
    repo.commit(None, &signature, &signature, message, &tree, &parents)
        .unwrap()
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let repo = Repository::init_bare(temp.path()).unwrap();
        let baseline_tree = tree(&repo, b"baseline\n");
        let baseline = commit(&repo, baseline_tree, &[], "baseline");
        let child = commit(&repo, baseline_tree, &[baseline], "mapped child origin");
        let parent = commit(
            &repo,
            tree(&repo, b"advanced parent\n"),
            &[baseline],
            "parent advance",
        );
        repo.reference("refs/heads/child", child, false, "fixture child")
            .unwrap();
        repo.reference("refs/heads/parent", parent, false, "fixture parent")
            .unwrap();
        Self {
            _temp: temp,
            repo,
            baseline,
            child,
            parent,
        }
    }

    fn pins(&self) -> CleanChildGitPins {
        CleanChildGitPins {
            child_tip: self.child.to_string(),
            parent_tip: self.parent.to_string(),
            parent_baseline_tip: self.baseline.to_string(),
        }
    }

    fn assert_refs_unchanged(&self) {
        assert_eq!(
            self.repo.refname_to_id("refs/heads/child").unwrap(),
            self.child
        );
        assert_eq!(
            self.repo.refname_to_id("refs/heads/parent").unwrap(),
            self.parent
        );
        assert_eq!(self.repo.references().unwrap().count(), 2);
    }

    fn prepare(
        &self,
        pins: &CleanChildGitPins,
    ) -> Result<
        reposync_core::pair_refresh::preparation::PreparedCleanChildGit,
        reposync_core::errors::GitError,
    > {
        prepare_clean_child_git(&self.repo, pins, Uuid::from_u128(42), PREPARED_AT)
    }
}

#[test]
fn prepared_commit_preserves_both_histories_and_exact_parent_tree() {
    let fixture = Fixture::new();
    let head = std::fs::read(fixture.repo.path().join("HEAD")).unwrap();
    let prepared = fixture.prepare(&fixture.pins()).unwrap();
    let result = fixture
        .repo
        .find_commit(Oid::from_str(&prepared.prepared_sha).unwrap())
        .unwrap();
    assert_eq!(result.parent_count(), 2);
    assert_eq!(result.parent_id(0).unwrap(), fixture.child);
    assert_eq!(result.parent_id(1).unwrap(), fixture.parent);
    assert_eq!(
        result.tree_id(),
        fixture.repo.find_commit(fixture.parent).unwrap().tree_id()
    );
    assert_eq!(result.tree_id().to_string(), prepared.tree_sha);
    assert_eq!(result.author().when().seconds(), PREPARED_AT);
    assert!(result
        .message()
        .unwrap()
        .contains(&Uuid::from_u128(42).to_string()));
    assert_eq!(prepared.parent_advance_commits, 1);
    assert!(fixture
        .repo
        .graph_descendant_of(result.id(), fixture.child)
        .unwrap());
    assert!(fixture
        .repo
        .graph_descendant_of(result.id(), fixture.parent)
        .unwrap());
    assert_eq!(
        std::fs::read(fixture.repo.path().join("HEAD")).unwrap(),
        head
    );
    fixture.assert_refs_unchanged();
}

#[test]
fn preparation_is_reproducible_for_the_same_operation_inputs() {
    let fixture = Fixture::new();
    let first = fixture.prepare(&fixture.pins()).unwrap();
    let repeated = fixture.prepare(&fixture.pins()).unwrap();
    assert_eq!(first, repeated);
    let distinct = prepare_clean_child_git(
        &fixture.repo,
        &fixture.pins(),
        Uuid::from_u128(43),
        PREPARED_AT,
    )
    .unwrap();
    assert_ne!(first.prepared_sha, distinct.prepared_sha);
    fixture.assert_refs_unchanged();
}

#[test]
fn changed_child_content_refuses() {
    let fixture = Fixture::new();
    let changed = commit(
        &fixture.repo,
        tree(&fixture.repo, b"independent child change\n"),
        &[fixture.child],
        "child work",
    );
    let mut pins = fixture.pins();
    pins.child_tip = changed.to_string();
    assert!(fixture.prepare(&pins).is_err());
    fixture.assert_refs_unchanged();
}

#[test]
fn no_parent_advance_refuses_without_creating_a_result() {
    let fixture = Fixture::new();
    let mut pins = fixture.pins();
    pins.parent_tip = fixture.baseline.to_string();
    assert!(fixture.prepare(&pins).is_err());
    fixture.assert_refs_unchanged();
}

#[test]
fn rewritten_parent_and_unrelated_child_refuse_despite_matching_trees() {
    let fixture = Fixture::new();
    let unrelated = commit(
        &fixture.repo,
        fixture
            .repo
            .find_commit(fixture.baseline)
            .unwrap()
            .tree_id(),
        &[],
        "foreign origin",
    );
    for child in [false, true] {
        let mut pins = fixture.pins();
        if child {
            pins.child_tip = unrelated.to_string();
        } else {
            pins.parent_tip = unrelated.to_string();
        }
        assert!(fixture.prepare(&pins).is_err());
    }
    fixture.assert_refs_unchanged();
}

#[test]
fn nonlinear_parent_advance_refuses() {
    let fixture = Fixture::new();
    let side = commit(
        &fixture.repo,
        tree(&fixture.repo, b"side branch\n"),
        &[fixture.baseline],
        "side",
    );
    let merge = commit(
        &fixture.repo,
        tree(&fixture.repo, b"parent merged\n"),
        &[fixture.parent, side],
        "nonlinear parent",
    );
    let mut pins = fixture.pins();
    pins.parent_tip = merge.to_string();
    assert!(fixture.prepare(&pins).is_err());
    fixture.assert_refs_unchanged();
}

#[test]
fn lfs_pointer_at_parent_tip_refuses() {
    let fixture = Fixture::new();
    let pointer = reposync_core::lfs::create_lfs_pointer(b"large fixture content");
    let unsupported = commit(
        &fixture.repo,
        tree(&fixture.repo, pointer.as_bytes()),
        &[fixture.parent],
        "LFS addition",
    );
    let mut pins = fixture.pins();
    pins.parent_tip = unsupported.to_string();
    assert!(fixture.prepare(&pins).is_err());
    fixture.assert_refs_unchanged();
}

#[test]
fn deleted_intermediate_lfs_pointer_still_refuses_new_history() {
    let fixture = Fixture::new();
    let pointer = reposync_core::lfs::create_lfs_pointer(b"large fixture content");
    let unsupported = commit(
        &fixture.repo,
        tree(&fixture.repo, pointer.as_bytes()),
        &[fixture.parent],
        "LFS addition",
    );
    let clean_tip = commit(
        &fixture.repo,
        tree(&fixture.repo, b"regular again\n"),
        &[unsupported],
        "LFS removal",
    );
    let mut pins = fixture.pins();
    pins.parent_tip = clean_tip.to_string();
    assert!(fixture.prepare(&pins).is_err());
    fixture.assert_refs_unchanged();
}

#[test]
fn nested_lfs_macro_and_other_attribute_rules_refuse() {
    let fixture = Fixture::new();
    for attributes in [
        b"*.bin filter=lfs\n".as_slice(),
        b"[attr]large filter=lfs\n*.bin large\n",
        b"*.txt text eol=crlf\n",
    ] {
        let mut nested = fixture.repo.treebuilder(None).unwrap();
        nested
            .insert(
                ".gitattributes",
                fixture.repo.blob(attributes).unwrap(),
                0o100644,
            )
            .unwrap();
        let mut root = fixture.repo.treebuilder(None).unwrap();
        root.insert("nested", nested.write().unwrap(), 0o040000)
            .unwrap();
        let unsupported = commit(
            &fixture.repo,
            root.write().unwrap(),
            &[fixture.parent],
            "attributes",
        );
        let mut pins = fixture.pins();
        pins.parent_tip = unsupported.to_string();
        assert!(fixture.prepare(&pins).is_err());
    }
    fixture.assert_refs_unchanged();
}

#[test]
fn regular_binary_executable_and_comment_only_attributes_are_supported() {
    let fixture = Fixture::new();
    let mut builder = fixture.repo.treebuilder(None).unwrap();
    builder
        .insert(
            "binary.bin",
            fixture.repo.blob(b"\0\xffbinary\n").unwrap(),
            0o100644,
        )
        .unwrap();
    builder
        .insert(
            "script.sh",
            fixture.repo.blob(b"#!/bin/sh\nexit 0\n").unwrap(),
            0o100755,
        )
        .unwrap();
    builder
        .insert(
            ".gitattributes",
            fixture.repo.blob(b"# no active attributes\n\n").unwrap(),
            0o100644,
        )
        .unwrap();
    let supported = commit(
        &fixture.repo,
        builder.write().unwrap(),
        &[fixture.parent],
        "supported content",
    );
    let mut pins = fixture.pins();
    pins.parent_tip = supported.to_string();
    let result = fixture.prepare(&pins).unwrap();
    assert_eq!(
        result.tree_sha,
        fixture
            .repo
            .find_commit(supported)
            .unwrap()
            .tree_id()
            .to_string()
    );
    assert_eq!(result.parent_advance_commits, 2);
    fixture.assert_refs_unchanged();
}

#[test]
fn symlinks_and_submodules_refuse() {
    let fixture = Fixture::new();
    for (object, mode) in [
        (fixture.repo.blob(b"file.txt").unwrap(), 0o120000),
        (fixture.parent, 0o160000),
    ] {
        let mut builder = fixture.repo.treebuilder(None).unwrap();
        builder.insert("unsupported", object, mode).unwrap();
        let unsupported = commit(
            &fixture.repo,
            builder.write().unwrap(),
            &[fixture.parent],
            "unsupported entry",
        );
        let mut pins = fixture.pins();
        pins.parent_tip = unsupported.to_string();
        assert!(fixture.prepare(&pins).is_err());
    }
    fixture.assert_refs_unchanged();
}

#[test]
fn incomplete_object_ids_and_invalid_operation_identity_refuse() {
    let fixture = Fixture::new();
    let mut pins = fixture.pins();
    pins.parent_tip.truncate(7);
    assert!(fixture.prepare(&pins).is_err());
    assert!(
        prepare_clean_child_git(&fixture.repo, &fixture.pins(), Uuid::nil(), PREPARED_AT).is_err()
    );
    assert!(
        prepare_clean_child_git(&fixture.repo, &fixture.pins(), Uuid::from_u128(42), -1).is_err()
    );
    fixture.assert_refs_unchanged();
}

#[test]
fn shallow_graph_cannot_bypass_preparation_guard() {
    let fixture = Fixture::new();
    std::fs::write(
        fixture.repo.path().join("shallow"),
        format!("{}\n", fixture.child),
    )
    .unwrap();
    assert!(fixture.prepare(&fixture.pins()).is_err());
    fixture.assert_refs_unchanged();
}

#[test]
fn subsequent_parent_advance_preserves_prior_refresh_commit() {
    let fixture = Fixture::new();
    let first = fixture.prepare(&fixture.pins()).unwrap();
    let first_id = Oid::from_str(&first.prepared_sha).unwrap();
    let next_parent = commit(
        &fixture.repo,
        tree(&fixture.repo, b"next parent advance\n"),
        &[fixture.parent],
        "next parent",
    );
    let pins = CleanChildGitPins {
        child_tip: first.prepared_sha,
        parent_tip: next_parent.to_string(),
        parent_baseline_tip: fixture.parent.to_string(),
    };
    let next = prepare_clean_child_git(&fixture.repo, &pins, Uuid::from_u128(43), PREPARED_AT + 1)
        .unwrap();
    let result = fixture
        .repo
        .find_commit(Oid::from_str(&next.prepared_sha).unwrap())
        .unwrap();
    assert_eq!(result.parent_id(0).unwrap(), first_id);
    assert_eq!(result.parent_id(1).unwrap(), next_parent);
    assert!(fixture
        .repo
        .graph_descendant_of(result.id(), fixture.child)
        .unwrap());
    assert_eq!(
        result.tree_id(),
        fixture.repo.find_commit(next_parent).unwrap().tree_id()
    );
    fixture.assert_refs_unchanged();
}
