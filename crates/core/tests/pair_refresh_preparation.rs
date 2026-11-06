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

// Object-only changed-child review. No commands, transport, SVN client or live
// service is invoked by this module. All repository writes create test inputs
// before the read-only calls (or construct an independent expected tree).
mod changed_child_review {
    use super::commit;
    use git2::{ObjectType, Oid, Repository};
    use reposync_core::db::pair_refresh_operations::RefreshRegistrationPin;
    use reposync_core::pair_refresh::actor_policy::RefreshActorPolicyPin;
    use reposync_core::pair_refresh::preparation::*;
    use reposync_core::pair_refresh::publication::PinnedPushPolicy;
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    type File<'a> = (&'a str, &'a [u8], i32);
    type TreeCase<'a> = (Vec<File<'a>>, Vec<File<'a>>, Vec<File<'a>>);

    // Recursive libgit2 TreeBuilder is independent of the inspector's bottom-up
    // in-memory Git serialization. These writes are fixture construction only.
    fn write_tree(repo: &Repository, files: &[File<'_>]) -> Oid {
        let mut builder = repo.treebuilder(None).unwrap();
        let mut directories: BTreeMap<&str, Vec<File<'_>>> = BTreeMap::new();
        for &(path, bytes, mode) in files {
            if let Some((directory, tail)) = path.split_once('/') {
                directories
                    .entry(directory)
                    .or_default()
                    .push((tail, bytes, mode));
            } else {
                let blob = repo.blob(bytes).unwrap();
                builder.insert(path, blob, mode).unwrap();
            }
        }
        for (name, children) in directories {
            builder
                .insert(name, write_tree(repo, &children), 0o040000)
                .unwrap();
        }
        builder.write().unwrap()
    }

    fn role(repo: &Repository, commit: Oid) -> ChangedChildGitInput {
        ChangedChildGitInput {
            commit: commit.to_string(),
            tree: repo.find_commit(commit).unwrap().tree_id().to_string(),
        }
    }

    fn registration(id: &str, scope: &str, git: &str, svn: i64) -> RefreshRegistrationPin {
        RefreshRegistrationPin {
            repo_id: id.into(),
            scope_uuid: scope.into(),
            configuration_digest: "a".repeat(64),
            echo_generation: 1,
            last_git_sha: git.into(),
            last_svn_rev: svn,
            scoped_git_checkpoint: Some(git.into()),
            scoped_svn_checkpoint: Some(svn.to_string()),
        }
    }

    struct Scenario {
        directory: tempfile::TempDir,
        repo: Repository,
        inputs: ChangedChildReviewInputs,
    }

    impl Scenario {
        fn new(base: &[File<'_>], child: &[File<'_>], parent: &[File<'_>]) -> Self {
            Self::build(base, child, parent, true)
        }

        fn build(base: &[File<'_>], child: &[File<'_>], parent: &[File<'_>], bare: bool) -> Self {
            let directory = tempfile::tempdir().unwrap();
            let repo = if bare {
                Repository::init_bare(directory.path()).unwrap()
            } else {
                Repository::init(directory.path()).unwrap()
            };
            let baseline = commit(&repo, write_tree(&repo, base), &[], "review baseline");
            let child = commit(
                &repo,
                write_tree(&repo, child),
                &[baseline],
                "child advance",
            );
            let parent = commit(
                &repo,
                write_tree(&repo, parent),
                &[baseline],
                "parent advance",
            );
            repo.reference("refs/heads/child", child, false, "fixture")
                .unwrap();
            repo.reference("refs/heads/parent", parent, false, "fixture")
                .unwrap();
            let inputs = ChangedChildReviewInputs {
                schema: 1,
                policy: CHANGED_CHILD_REVIEW_POLICY.into(),
                baseline: role(&repo, baseline),
                child: role(&repo, child),
                parent: role(&repo, parent),
                context: ChangedChildReviewContext {
                    child: registration(
                        "child",
                        "11111111-1111-4111-8111-111111111111",
                        &child.to_string(),
                        2,
                    ),
                    parent: registration(
                        "parent",
                        "22222222-2222-4222-8222-222222222222",
                        &parent.to_string(),
                        3,
                    ),
                    git_remote_url: "https://git.example.invalid/repo.git".into(),
                    child_git_branch: "child".into(),
                    parent_git_branch: "parent".into(),
                    transport_policy: PinnedPushPolicy::SmartHttpV1,
                    svn: ChangedChildSvnContext {
                        repository_uuid: "33333333-3333-4333-8333-333333333333".into(),
                        repository_root_url: "https://svn.example.invalid/repo".into(),
                        child_path: "/branches/child".into(),
                        parent_path: "/trunk".into(),
                        child_revision: 2,
                        parent_revision: 3,
                        baseline_revision: 1,
                        inspected_head_revision: 3,
                        lineage_evidence: None,
                        property_evidence: None,
                    },
                    baseline_evidence: None,
                    actor_policy: None,
                    credential_context_digest: None,
                    projection: ChangedChildProjection {
                        policy: "full_tree_no_lfs_v1".into(),
                        allowed_paths: Vec::new(),
                        blocked_patterns: Vec::new(),
                        lfs_threshold_mb: 0,
                    },
                },
            };
            Self {
                directory,
                repo,
                inputs,
            }
        }

        fn conflict() -> Self {
            Self::new(
                &[("a", b"base", 0o100644)],
                &[("a", b"child", 0o100644)],
                &[("a", b"parent", 0o100644)],
            )
        }

        fn inspect(&self) -> ChangedChildReviewManifest {
            inspect_changed_child_review(&self.repo, &self.inputs).unwrap()
        }

        fn set_child(&mut self, id: Oid) {
            self.inputs.child = role(&self.repo, id);
            self.inputs.context.child.last_git_sha = id.to_string();
            self.inputs.context.child.scoped_git_checkpoint = Some(id.to_string());
        }
    }

    fn resolution(manifest: &ChangedChildReviewManifest) -> ChangedChildResolution {
        ChangedChildResolution {
            schema: 1,
            policy: CHANGED_CHILD_REVIEW_POLICY.into(),
            inputs: manifest.inputs.clone(),
            manifest_digest: manifest.manifest_digest.clone(),
            choices: manifest
                .paths
                .iter()
                .filter(|path| matches!(path.disposition, ChangedChildDisposition::Conflict))
                .map(|path| ChangedChildConflictChoice {
                    path: path.path.clone(),
                    baseline: path.baseline.clone(),
                    child: path.child.clone(),
                    parent: path.parent.clone(),
                    choice: ChangedChildChoice::Child,
                })
                .collect(),
        }
    }

    fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        fn visit(root: &Path, path: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
            for entry in std::fs::read_dir(path).unwrap() {
                let path = entry.unwrap().path();
                let metadata = std::fs::symlink_metadata(&path).unwrap();
                assert!(!metadata.file_type().is_symlink());
                if metadata.is_dir() {
                    visit(root, &path, files);
                } else {
                    files.insert(
                        path.strip_prefix(root).unwrap().into(),
                        std::fs::read(&path).unwrap(),
                    );
                }
            }
        }
        let mut files = BTreeMap::new();
        visit(root, root, &mut files);
        files
    }

    #[test]
    fn clean_child_and_old_execution_refusal_are_preserved() {
        let clean = Scenario::new(
            &[("a", b"base", 0o100644)],
            &[("a", b"base", 0o100644)],
            &[("a", b"parent", 0o100644)],
        );
        let manifest = clean.inspect();
        assert_eq!(
            manifest.candidate_tree.as_deref(),
            Some(clean.inputs.parent.tree.as_str())
        );
        assert!(manifest.child_changes.is_empty());
        assert_eq!(manifest.parent_changes, ["a"]);
        let changed = Scenario::conflict();
        let pins = CleanChildGitPins {
            child_tip: changed.inputs.child.commit.clone(),
            parent_tip: changed.inputs.parent.commit.clone(),
            parent_baseline_tip: changed.inputs.baseline.commit.clone(),
        };
        let before = snapshot(changed.directory.path());
        assert!(
            prepare_clean_child_git(&changed.repo, &pins, uuid::Uuid::from_u128(42), 1).is_err()
        );
        assert_eq!(before, snapshot(changed.directory.path()));
    }

    #[test]
    fn distinct_path_changes_preserve_both_sides_and_exact_tree_ordering() {
        let s = Scenario::new(
            &[("old", b"base", 0o100644)],
            &[("old", b"base", 0o100644), ("foo/z", b"child", 0o100755)],
            &[
                ("old", b"parent", 0o100644),
                ("foo.bar", b"parent add", 0o100644),
            ],
        );
        let expected = write_tree(
            &s.repo,
            &[
                ("old", b"parent", 0o100644),
                ("foo/z", b"child", 0o100755),
                ("foo.bar", b"parent add", 0o100644),
            ],
        );
        let before = snapshot(s.directory.path());
        let manifest = s.inspect();
        assert_eq!(manifest.candidate_tree, Some(expected.to_string()));
        assert_eq!(manifest.child_changes, ["foo", "foo/z"]);
        assert_eq!(manifest.parent_changes, ["foo.bar", "old"]);
        assert_eq!(manifest, s.inspect());
        assert_eq!(before, snapshot(s.directory.path()));
    }

    #[test]
    fn identical_changes_still_record_each_side_delta() {
        let s = Scenario::new(
            &[],
            &[("new", b"same", 0o100644)],
            &[("new", b"same", 0o100644)],
        );
        let manifest = s.inspect();
        assert_eq!(manifest.child_changes, ["new"]);
        assert_eq!(manifest.parent_changes, ["new"]);
        assert_eq!(manifest.candidate_tree, Some(s.inputs.child.tree.clone()));
        assert!(!manifest.execution_eligible);
    }

    #[test]
    fn opaque_overlap_conflicts_include_modes_absence_and_exact_triples() {
        let cases: Vec<TreeCase<'_>> = vec![
            (
                vec![("a", b"base", 0o100644)],
                vec![("a", b"child", 0o100644)],
                vec![("a", b"parent", 0o100644)],
            ),
            (
                vec![],
                vec![("a", b"child", 0o100644)],
                vec![("a", b"parent", 0o100644)],
            ),
            (
                vec![("a", b"base", 0o100644)],
                vec![],
                vec![("a", b"parent", 0o100644)],
            ),
            (
                vec![("a", b"base", 0o100644)],
                vec![("a", b"child", 0o100644)],
                vec![("a", b"base", 0o100755)],
            ),
        ];
        for (base, child, parent) in cases {
            let s = Scenario::new(&base, &child, &parent);
            let manifest = s.inspect();
            assert!(manifest.candidate_tree.is_none());
            assert_eq!(manifest.paths.len(), 1);
            let path = &manifest.paths[0];
            assert!(matches!(
                path.disposition,
                ChangedChildDisposition::Conflict
            ));
            assert_ne!(path.child, path.parent);
            assert_ne!(path.child, path.baseline);
            assert_ne!(path.parent, path.baseline);
            if child.is_empty() {
                assert_eq!(path.child, ChangedChildEntry::Absent {});
            }
            let mut choice = resolution(&manifest);
            for (selected, expected) in [
                (ChangedChildChoice::Base, &s.inputs.baseline.tree),
                (ChangedChildChoice::Child, &s.inputs.child.tree),
                (ChangedChildChoice::Parent, &s.inputs.parent.tree),
            ] {
                choice.choices[0].choice = selected;
                let result = resolve_changed_child_review(&s.repo, &s.inputs, &choice).unwrap();
                assert_eq!(&result.candidate_tree, expected);
                assert!(!result.execution_eligible);
            }
        }
    }

    #[test]
    fn binary_bytes_are_opaque_and_conflicting_choice_is_explicit() {
        let s = Scenario::new(
            &[("data", b"\0\xffbase", 0o100644)],
            &[("data", b"\0\xfechild", 0o100755)],
            &[("data", b"\0\xfdparent", 0o100644)],
        );
        let manifest = s.inspect();
        assert!(manifest.candidate_tree.is_none());
        let mut choice = resolution(&manifest);
        choice.choices[0].choice = ChangedChildChoice::Parent;
        let before = snapshot(s.directory.path());
        let result = resolve_changed_child_review(&s.repo, &s.inputs, &choice).unwrap();
        assert_eq!(result.candidate_tree, s.inputs.parent.tree);
        assert_eq!(
            result.paths[0].disposition,
            ChangedChildDisposition::Selected {
                entry: manifest.paths[0].parent.clone(),
            }
        );
        assert_eq!(before, snapshot(s.directory.path()));
    }

    #[test]
    fn resolution_order_is_canonical_and_original_conflicts_survive() {
        let s = Scenario::new(
            &[("a", b"base", 0o100644), ("b", b"base", 0o100644)],
            &[("a", b"child", 0o100644), ("b", b"child", 0o100644)],
            &[("a", b"parent", 0o100644), ("b", b"parent", 0o100644)],
        );
        let manifest = s.inspect();
        let mut choice = resolution(&manifest);
        let first = resolve_changed_child_review(&s.repo, &s.inputs, &choice).unwrap();
        choice.choices.reverse();
        assert_eq!(
            first,
            resolve_changed_child_review(&s.repo, &s.inputs, &choice).unwrap()
        );
        assert_eq!(first.original_manifest_digest, manifest.manifest_digest);
        assert_eq!(first.paths[0].baseline, manifest.paths[0].baseline);
        choice.choices[0].choice = ChangedChildChoice::Parent;
        let different = resolve_changed_child_review(&s.repo, &s.inputs, &choice).unwrap();
        assert_ne!(first.resolution_digest, different.resolution_digest);
        assert_eq!(manifest, s.inspect());
    }

    #[test]
    fn changed_commit_and_context_refuse_old_resolution_before_choices() {
        let mut s = Scenario::conflict();
        let choice = resolution(&s.inspect());
        let old = Oid::from_str(&s.inputs.child.commit).unwrap();
        let id = commit(
            &s.repo,
            Oid::from_str(&s.inputs.child.tree).unwrap(),
            &[old],
            "child history drift",
        );
        s.set_child(id);
        let error = resolve_changed_child_review(&s.repo, &s.inputs, &choice).unwrap_err();
        assert!(error.to_string().contains("stale resolution identity"));
        let s = Scenario::conflict();
        let choice = resolution(&s.inspect());
        type Mutation = fn(&mut ChangedChildReviewInputs);
        let mutations: &[Mutation] = &[
            |i| i.context.child.repo_id.push_str("-new"),
            |i| i.context.child.scope_uuid = "44444444-4444-4444-8444-444444444444".into(),
            |i| i.context.child.configuration_digest = "b".repeat(64),
            |i| i.context.child.echo_generation += 1,
            |i| i.context.git_remote_url = "https://git.example.invalid/other.git".into(),
            |i| i.context.child_git_branch = "other-child".into(),
            |i| i.context.svn.repository_uuid.push_str("-different"),
            |i| i.context.svn.repository_root_url = "https://svn.example.invalid/other".into(),
            |i| i.context.svn.child_path = "/branches/other".into(),
            |i| i.context.svn.inspected_head_revision += 1,
            |i| {
                i.context.svn.child_revision = 3;
                i.context.child.last_svn_rev = 3;
                i.context.child.scoped_svn_checkpoint = Some("3".into());
            },
            |i| {
                i.context.baseline_evidence = Some(ChangedChildEvidence {
                    reference: "receipt-1".into(),
                    sha256: "c".repeat(64),
                })
            },
            |i| {
                i.context.svn.lineage_evidence = Some(ChangedChildEvidence {
                    reference: "lineage-1".into(),
                    sha256: "d".repeat(64),
                })
            },
            |i| {
                i.context.svn.property_evidence = Some(ChangedChildEvidence {
                    reference: "props-1".into(),
                    sha256: "e".repeat(64),
                })
            },
            |i| i.context.credential_context_digest = Some("f".repeat(64)),
            |i| {
                i.context.actor_policy = Some(RefreshActorPolicyPin {
                    version: 2,
                    actor_id: "actor".into(),
                    session_context_digest: "b".repeat(64),
                    policy_context_digest: "c".repeat(64),
                })
            },
        ];
        for mutate in mutations {
            let mut current = s.inputs.clone();
            mutate(&mut current);
            let error = resolve_changed_child_review(&s.repo, &current, &choice).unwrap_err();
            assert!(
                error.to_string().contains("stale resolution identity"),
                "{error}"
            );
        }
    }

    #[test]
    fn duplicate_missing_foreign_nonconflict_and_forged_choices_refuse() {
        let s = Scenario::conflict();
        let original = resolution(&s.inspect());
        let mut invalid = original.clone();
        invalid.choices.push(invalid.choices[0].clone());
        assert!(resolve_changed_child_review(&s.repo, &s.inputs, &invalid)
            .unwrap_err()
            .to_string()
            .contains("duplicate"));
        invalid = original.clone();
        invalid.choices.clear();
        assert!(resolve_changed_child_review(&s.repo, &s.inputs, &invalid)
            .unwrap_err()
            .to_string()
            .contains("missing conflict"));
        invalid = original.clone();
        invalid.choices[0].child = ChangedChildEntry::Absent {};
        assert!(resolve_changed_child_review(&s.repo, &s.inputs, &invalid)
            .unwrap_err()
            .to_string()
            .contains("triple differs"));
        invalid = original.clone();
        invalid.choices[0].path = "foreign".into();
        assert!(resolve_changed_child_review(&s.repo, &s.inputs, &invalid).is_err());
        invalid = original.clone();
        invalid.manifest_digest = "0".repeat(64);
        assert!(resolve_changed_child_review(&s.repo, &s.inputs, &invalid).is_err());
        invalid = original.clone();
        invalid.schema = 2;
        assert!(resolve_changed_child_review(&s.repo, &s.inputs, &invalid).is_err());
        invalid = original;
        invalid.policy.push_str("-unknown");
        assert!(resolve_changed_child_review(&s.repo, &s.inputs, &invalid).is_err());
        let clean = Scenario::new(&[], &[], &[("a", b"parent", 0o100644)]);
        let manifest = clean.inspect();
        let mut invalid = resolution(&manifest);
        let path = &manifest.paths[0];
        invalid.choices.push(ChangedChildConflictChoice {
            path: path.path.clone(),
            baseline: path.baseline.clone(),
            child: path.child.clone(),
            parent: path.parent.clone(),
            choice: ChangedChildChoice::Parent,
        });
        assert!(
            resolve_changed_child_review(&clean.repo, &clean.inputs, &invalid)
                .unwrap_err()
                .to_string()
                .contains("non-conflict")
        );
    }

    #[test]
    fn strict_wire_schema_refuses_unknown_duplicate_and_malformed_fields() {
        let s = Scenario::conflict();
        let encoded = serde_json::to_value(resolution(&s.inspect())).unwrap();
        for pointer in [
            "",
            "/inputs",
            "/inputs/context",
            "/inputs/context/projection",
            "/inputs/context/child",
            "/choices/0",
            "/choices/0/child",
        ] {
            let mut bad = encoded.clone();
            bad.pointer_mut(pointer)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert("unknown".into(), true.into());
            assert!(
                serde_json::from_value::<ChangedChildResolution>(bad).is_err(),
                "{pointer}"
            );
        }
        let raw = serde_json::to_string(&encoded).unwrap();
        let duplicate = format!("{{\"schema\":1,{}", &raw[1..]);
        assert!(serde_json::from_str::<ChangedChildResolution>(&duplicate).is_err());
        for replacement in [
            serde_json::json!("Other"),
            serde_json::json!(true),
            serde_json::Value::Null,
        ] {
            let mut bad = encoded.clone();
            bad["choices"][0]["choice"] = replacement;
            assert!(serde_json::from_value::<ChangedChildResolution>(bad).is_err());
        }
        let absent = Scenario::new(
            &[],
            &[("a", b"child", 0o100644)],
            &[("a", b"parent", 0o100644)],
        );
        let mut bad = serde_json::to_value(resolution(&absent.inspect())).unwrap();
        bad["choices"][0]["baseline"]["unknown"] = true.into();
        assert!(serde_json::from_value::<ChangedChildResolution>(bad).is_err());
    }

    #[test]
    fn topology_conflicts_cannot_be_resolved_by_choosing_a_blob() {
        let cases: Vec<TreeCase<'_>> = vec![
            (
                vec![],
                vec![("x", b"file", 0o100644)],
                vec![("x/y", b"nested", 0o100644)],
            ),
            (
                vec![("x/a", b"base", 0o100644)],
                vec![],
                vec![("x/a", b"base", 0o100644), ("x/b", b"new", 0o100644)],
            ),
        ];
        for (base, child, parent) in cases {
            let s = Scenario::new(&base, &child, &parent);
            let manifest = s.inspect();
            assert!(manifest.candidate_tree.is_none());
            assert!(manifest
                .paths
                .iter()
                .any(|p| matches!(p.disposition, ChangedChildDisposition::Refused { .. })));
            assert!(
                resolve_changed_child_review(&s.repo, &s.inputs, &resolution(&manifest)).is_err()
            );
        }
    }

    #[test]
    fn unsupported_content_and_paths_refuse_even_when_selected_away() {
        let bad: Vec<File<'_>> = vec![
            ("bad name", b"bytes", 0o100644),
            ("link", b"target", 0o120000),
            (
                "pointer",
                b"version https://git-lfs.github.com/spec/v1\noid sha256:0000\nsize 1\n",
                0o100644,
            ),
            (".gitattributes", b"*.dat filter=custom\n", 0o100644),
        ];
        for file in bad {
            let s = Scenario::new(&[file], &[], &[("safe", b"parent", 0o100644)]);
            let before = snapshot(s.directory.path());
            assert!(inspect_changed_child_review(&s.repo, &s.inputs).is_err());
            assert_eq!(before, snapshot(s.directory.path()));
        }
    }

    #[test]
    fn intermediate_unsupported_child_content_is_not_hidden_by_deletion() {
        let mut s = Scenario::conflict();
        let bad = commit(
            &s.repo,
            write_tree(&s.repo, &[(".gitattributes", b"* filter=custom", 0o100644)]),
            &[Oid::from_str(&s.inputs.child.commit).unwrap()],
            "temporary attributes",
        );
        let clean = commit(
            &s.repo,
            Oid::from_str(&s.inputs.child.tree).unwrap(),
            &[bad],
            "delete attributes",
        );
        s.set_child(clean);
        assert!(inspect_changed_child_review(&s.repo, &s.inputs).is_err());
    }

    #[test]
    fn unrelated_and_nonlinear_histories_refuse_despite_equal_trees() {
        let mut s = Scenario::conflict();
        let same_tree = Oid::from_str(&s.inputs.child.tree).unwrap();
        let unrelated = commit(&s.repo, same_tree, &[], "unrelated matching bytes");
        s.set_child(unrelated);
        assert!(inspect_changed_child_review(&s.repo, &s.inputs).is_err());
        let baseline = Oid::from_str(&s.inputs.baseline.commit).unwrap();
        let nonlinear = commit(
            &s.repo,
            same_tree,
            &[baseline, unrelated],
            "nonlinear child",
        );
        s.set_child(nonlinear);
        assert!(inspect_changed_child_review(&s.repo, &s.inputs).is_err());
    }

    #[test]
    fn metadata_and_object_pin_failures_do_not_create_objects() {
        let s = Scenario::conflict();
        let before = snapshot(s.directory.path());
        let mut bad = s.inputs.clone();
        bad.child.tree = bad.parent.tree.clone();
        assert!(inspect_changed_child_review(&s.repo, &bad)
            .unwrap_err()
            .to_string()
            .contains("commit/tree pin"));
        bad = s.inputs.clone();
        bad.child.commit = "0".repeat(40);
        bad.context.child.last_git_sha = bad.child.commit.clone();
        bad.context.child.scoped_git_checkpoint = Some(bad.child.commit.clone());
        assert!(inspect_changed_child_review(&s.repo, &bad).is_err());
        bad = s.inputs.clone();
        bad.context.projection.allowed_paths.push("a".into());
        assert!(inspect_changed_child_review(&s.repo, &bad)
            .unwrap_err()
            .to_string()
            .contains("projection"));
        bad = s.inputs.clone();
        bad.context.child.scoped_git_checkpoint = None;
        assert!(inspect_changed_child_review(&s.repo, &bad).is_err());
        bad = s.inputs.clone();
        bad.context.baseline_evidence = Some(ChangedChildEvidence {
            reference: "receipt".into(),
            sha256: "bad".into(),
        });
        assert!(inspect_changed_child_review(&s.repo, &bad).is_err());
        assert_eq!(before, snapshot(s.directory.path()));
    }

    #[test]
    fn shallow_grafted_and_replaced_graphs_refuse() {
        for overlay in ["shallow", "info/grafts", "replace"] {
            let s = Scenario::conflict();
            if overlay == "replace" {
                s.repo
                    .reference(
                        &format!("refs/replace/{}", s.inputs.child.commit),
                        Oid::from_str(&s.inputs.parent.commit).unwrap(),
                        false,
                        "fixture replace",
                    )
                    .unwrap();
            } else {
                let path = s.repo.path().join(overlay);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, format!("{}\n", s.inputs.child.commit)).unwrap();
            }
            let before = snapshot(s.directory.path());
            assert!(
                inspect_changed_child_review(&s.repo, &s.inputs).is_err(),
                "{overlay}"
            );
            assert_eq!(before, snapshot(s.directory.path()));
        }
    }

    #[test]
    fn finite_linear_child_bound_is_explicit() {
        let mut s = Scenario::conflict();
        let tree = Oid::from_str(&s.inputs.child.tree).unwrap();
        let mut tip = Oid::from_str(&s.inputs.baseline.commit).unwrap();
        for index in 0..1000 {
            tip = commit(&s.repo, tree, &[tip], &format!("child step {index}"));
        }
        s.set_child(tip);
        assert!(inspect_changed_child_review(&s.repo, &s.inputs).is_ok());
        let excess = commit(&s.repo, tree, &[tip], "child step 1001");
        s.set_child(excess);
        assert!(inspect_changed_child_review(&s.repo, &s.inputs)
            .unwrap_err()
            .to_string()
            .contains("oversized"));
    }

    #[test]
    fn review_does_not_modify_nonbare_refs_index_worktree_or_objects() {
        let s = Scenario::build(
            &[("a", b"base", 0o100644)],
            &[("a", b"child", 0o100644)],
            &[("a", b"parent", 0o100644)],
            false,
        );
        std::fs::write(s.directory.path().join("working-file"), b"must not change").unwrap();
        {
            let mut index = s.repo.index().unwrap();
            index.add_path(Path::new("working-file")).unwrap();
            index.write().unwrap();
        }
        let before = snapshot(s.directory.path());
        let manifest = s.inspect();
        let choice = resolution(&manifest);
        let result = resolve_changed_child_review(&s.repo, &s.inputs, &choice).unwrap();
        assert_eq!(result.candidate_tree, s.inputs.child.tree);
        assert_eq!(before, snapshot(s.directory.path()));
    }

    #[test]
    fn metadata_never_becomes_authenticated_correspondence_or_execution() {
        let mut s = Scenario::conflict();
        let missing = s.inspect();
        assert!(missing
            .unverified
            .contains(&"svn_property_evidence_missing".into()));
        let evidence = ChangedChildEvidence {
            reference: "caller-reference".into(),
            sha256: "b".repeat(64),
        };
        s.inputs.context.baseline_evidence = Some(evidence.clone());
        s.inputs.context.svn.lineage_evidence = Some(evidence.clone());
        s.inputs.context.svn.property_evidence = Some(evidence);
        s.inputs.context.actor_policy = Some(RefreshActorPolicyPin {
            version: 2,
            actor_id: "actor".into(),
            session_context_digest: "c".repeat(64),
            policy_context_digest: "d".repeat(64),
        });
        s.inputs.context.credential_context_digest = Some("e".repeat(64));
        let supplied = s.inspect();
        assert!(!supplied.execution_eligible);
        assert!(supplied
            .unverified
            .contains(&"baseline_provenance_not_authenticated".into()));
        assert!(supplied
            .unverified
            .contains(&"svn_correspondence_not_verified".into()));
        assert!(supplied
            .unverified
            .contains(&"pending_work_not_independently_observed".into()));
        let resolved =
            resolve_changed_child_review(&s.repo, &s.inputs, &resolution(&supplied)).unwrap();
        assert!(!resolved.execution_eligible);
        assert_eq!(resolved.unverified, supplied.unverified);
        assert_ne!(missing.manifest_digest, supplied.manifest_digest);
    }

    #[test]
    fn submodule_non_utf8_and_empty_subtree_are_explicit_refusals() {
        let mut s = Scenario::conflict();
        let base = Oid::from_str(&s.inputs.baseline.commit).unwrap();
        let submodule = {
            let mut builder = s.repo.treebuilder(None).unwrap();
            builder.insert("module", base, 0o160000).unwrap();
            builder.write().unwrap()
        };
        let id = commit(&s.repo, submodule, &[base], "gitlink");
        s.set_child(id);
        assert!(inspect_changed_child_review(&s.repo, &s.inputs).is_err());
        let blob = s.repo.blob(b"opaque").unwrap();
        let mut raw = b"100644 \xff\0".to_vec();
        raw.extend_from_slice(blob.as_bytes());
        let non_utf8 = s.repo.odb().unwrap().write(ObjectType::Tree, &raw).unwrap();
        let id = commit(&s.repo, non_utf8, &[base], "non UTF8");
        s.set_child(id);
        assert!(inspect_changed_child_review(&s.repo, &s.inputs).is_err());
        let empty = s.repo.treebuilder(None).unwrap().write().unwrap();
        let nested_empty = {
            let mut builder = s.repo.treebuilder(None).unwrap();
            builder.insert("empty", empty, 0o040000).unwrap();
            builder.write().unwrap()
        };
        let id = commit(&s.repo, nested_empty, &[base], "empty directory");
        s.set_child(id);
        assert!(inspect_changed_child_review(&s.repo, &s.inputs)
            .unwrap_err()
            .to_string()
            .contains("empty subtree"));
    }

    #[test]
    fn combined_tree_identity_is_not_materialized_in_the_source_odb() {
        let s = Scenario::new(
            &[],
            &[("child", b"child", 0o100644)],
            &[("parent", b"parent", 0o100755)],
        );
        let expected_directory = tempfile::tempdir().unwrap();
        let expected_repo = Repository::init_bare(expected_directory.path()).unwrap();
        let expected = write_tree(
            &expected_repo,
            &[
                ("child", b"child", 0o100644),
                ("parent", b"parent", 0o100755),
            ],
        );
        assert!(s.repo.find_tree(expected).is_err());
        let before = snapshot(s.directory.path());
        let manifest = s.inspect();
        assert_eq!(manifest.candidate_tree, Some(expected.to_string()));
        let result =
            resolve_changed_child_review(&s.repo, &s.inputs, &resolution(&manifest)).unwrap();
        assert_eq!(result.candidate_tree, expected.to_string());
        assert!(s.repo.find_tree(expected).is_err());
        assert_eq!(before, snapshot(s.directory.path()));
    }

    #[test]
    fn identical_directory_deletion_is_not_a_delete_modify_conflict() {
        let s = Scenario::new(&[("x/a", b"base", 0o100644)], &[], &[]);
        let manifest = s.inspect();
        assert_eq!(manifest.candidate_tree, Some(s.inputs.child.tree.clone()));
        assert_eq!(manifest.child_changes, ["x", "x/a"]);
        assert_eq!(manifest.parent_changes, ["x", "x/a"]);
        assert!(manifest.paths.iter().all(|path| !matches!(
            path.disposition,
            ChangedChildDisposition::Conflict | ChangedChildDisposition::Refused { .. }
        )));
    }
}
