//! Shared SVN snapshot / selected-revision engine.
//!
//! Personal `ImportMode::Snapshot` and team onboarding both pin a numeric
//! revision once, export that pegged tree under the active file policy, and
//! verify projected file contents. Do not add a second snapshot implementation.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use tracing::info;

use crate::db::import_operations::SnapshotPin;
use crate::db::Database;
use crate::file_policy::{FilePolicy, FilePolicyDecision};
use crate::import::{copy_tree_with_policy, CopyStats, VerificationResult};
use crate::svn::SvnClient;

/// Requested snapshot revision. `HEAD` is resolved once to a number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotRevision {
    Head,
    Number(i64),
}

impl SnapshotRevision {
    pub fn parse(raw: Option<&str>) -> Result<Self> {
        match raw.map(str::trim).filter(|s| !s.is_empty()) {
            None | Some("HEAD") | Some("head") => Ok(Self::Head),
            Some(value) => {
                let rev: i64 = value.parse().map_err(|_| {
                    anyhow::anyhow!("svn_revision must be HEAD or a positive integer")
                })?;
                if rev < 1 {
                    bail!("svn_revision must be a positive integer, got {rev}");
                }
                Ok(Self::Number(rev))
            }
        }
    }

    pub fn requested_label(&self) -> String {
        match self {
            Self::Head => "HEAD".into(),
            Self::Number(rev) => rev.to_string(),
        }
    }
}

/// Resolve HEAD or an explicit revision to a pinned identity. Call this once.
/// Later SVN advancement must not change the pin.
pub async fn resolve_snapshot_pin(
    svn: &SvnClient,
    requested: SnapshotRevision,
) -> Result<SnapshotPin> {
    let head = svn.info().await.context("failed to read live SVN info")?;
    let operative = match requested {
        SnapshotRevision::Head => head.latest_rev,
        SnapshotRevision::Number(rev) => {
            if rev > head.latest_rev {
                bail!(
                    "requested SVN revision r{rev} is beyond current HEAD r{}",
                    head.latest_rev
                );
            }
            rev
        }
    };
    if operative < 1 {
        bail!("SVN repository has no selectable revision");
    }
    let pinned = svn.info_at_rev(operative).await.with_context(|| {
        format!("requested SVN revision r{operative} is invalid or inaccessible")
    })?;
    if pinned.uuid != head.uuid {
        bail!(
            "SVN UUID at r{operative} ({}) differs from live HEAD ({})",
            pinned.uuid,
            head.uuid
        );
    }
    let (copy_from_path, copy_from_rev) = copy_ancestry_if_available(svn, operative).await;
    info!(
        uuid = %pinned.uuid,
        url = %pinned.url,
        operative,
        requested = %requested.requested_label(),
        "pinned SVN snapshot identity"
    );
    Ok(SnapshotPin {
        svn_uuid: pinned.uuid,
        canonical_url: pinned.url,
        operative_rev: operative,
        peg_rev: operative,
        copy_from_path,
        copy_from_rev,
        requested: requested.requested_label(),
    })
}

async fn copy_ancestry_if_available(svn: &SvnClient, rev: i64) -> (Option<String>, Option<i64>) {
    match svn.log(rev, rev).await {
        Ok(entries) => {
            for entry in entries {
                for path in entry.changed_paths {
                    if path.copy_from_path.is_some() {
                        return (path.copy_from_path, path.copy_from_rev);
                    }
                }
            }
            (None, None)
        }
        Err(_) => (None, None),
    }
}

/// Export the pinned peg revision and copy it under the active file policy.
/// Callers must pass the stored pin, not a freshly resolved HEAD.
pub async fn materialize_snapshot(
    svn: &SvnClient,
    git_workdir: &Path,
    policy: &FilePolicy,
    db: &Database,
    pin: &SnapshotPin,
) -> Result<CopyStats> {
    let live = svn.info().await.context("failed to re-read SVN identity")?;
    if live.uuid != pin.svn_uuid {
        bail!(
            "SVN UUID changed after pin ({} -> {}); snapshot refuses to continue",
            pin.svn_uuid,
            live.uuid
        );
    }
    let export_dir = tempfile::tempdir().context("failed to create snapshot export directory")?;
    svn.export("", pin.operative_rev, export_dir.path())
        .await
        .with_context(|| format!("failed to export pinned SVN r{}", pin.operative_rev))?;
    copy_tree_with_policy(export_dir.path(), git_workdir, policy, db)
        .context("failed to apply file policy while materializing snapshot")
}

/// Re-export the pin and compare projected file bytes with the Git tree.
/// Name/count equality is not enough.
pub async fn verify_projected_snapshot(
    svn: &SvnClient,
    git_workdir: &Path,
    policy: &FilePolicy,
    db: &Database,
    pin: &SnapshotPin,
) -> Result<VerificationResult> {
    let export_dir = tempfile::tempdir().context("failed to create snapshot verify export")?;
    svn.export("", pin.operative_rev, export_dir.path())
        .await
        .context("failed to re-export pinned revision for verification")?;
    let projected = tempfile::tempdir().context("failed to create snapshot projection")?;
    copy_tree_with_policy(export_dir.path(), projected.path(), policy, db)
        .context("failed to project pinned tree for verification")?;

    let expected = content_index(projected.path())?;
    let actual = content_index(git_workdir)?;
    let mut result = VerificationResult {
        files_checked: expected.len() as u64,
        ..VerificationResult::default()
    };
    for (path, hash) in &expected {
        match actual.get(path) {
            Some(other) if other == hash => result.files_matched += 1,
            Some(_) => result.mismatches.push(path.clone()),
            None => result.svn_only.push(path.clone()),
        }
    }
    for path in actual.keys() {
        if !expected.contains_key(path) {
            result.git_only.push(path.clone());
        }
    }
    result.sample_hashed = result.files_checked;
    result.verified = result.mismatches.is_empty()
        && result.svn_only.is_empty()
        && result.git_only.is_empty()
        && result.files_matched == result.files_checked;
    if !result.verified {
        bail!(
            "snapshot projection mismatch at r{}: mismatches={:?} svn_only={:?} git_only={:?}",
            pin.operative_rev,
            result.mismatches,
            result.svn_only,
            result.git_only
        );
    }
    Ok(result)
}

fn content_index(root: &Path) -> Result<BTreeMap<String, String>> {
    let mut files = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in
            std::fs::read_dir(&dir).with_context(|| format!("failed to read {}", dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            let name = entry.file_name();
            if name == ".git" || name == ".svn" {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            let bytes = std::fs::read(&path)
                .with_context(|| format!("failed to read snapshot file {rel}"))?;
            files.insert(rel, hex::encode(Sha256::digest(&bytes)));
        }
    }
    Ok(files)
}

/// Baseline commit message used by personal and team snapshot imports.
pub fn snapshot_commit_message(pin: &SnapshotPin) -> String {
    format!(
        "Initial import from SVN (snapshot at r{})\n\n{}",
        pin.operative_rev,
        pin.history_boundary()
    )
}

pub fn snapshot_workdir_is_born(git_workdir: &Path) -> Result<bool> {
    if !git_workdir.join(".git").exists() {
        return Ok(false);
    }
    let repo = git2::Repository::open(git_workdir)
        .with_context(|| format!("failed to open {}", git_workdir.display()))?;
    Ok(repo.head().ok().and_then(|head| head.target()).is_some())
}

/// Walk a Git commit tree (not the dirty workdir) for content verification
/// after the baseline commit exists.
pub fn git_commit_content_index(git_workdir: &Path, sha: &str) -> Result<BTreeMap<String, String>> {
    let repo = git2::Repository::open(git_workdir).context("failed to open Git workdir")?;
    let oid = git2::Oid::from_str(sha).context("snapshot Git SHA is malformed")?;
    let commit = repo
        .find_commit(oid)
        .context("snapshot Git commit is missing")?;
    let tree = commit.tree().context("snapshot Git tree is missing")?;
    let mut files = BTreeMap::new();
    tree.walk(git2::TreeWalkMode::PreOrder, |dir, entry| {
        if entry.kind() == Some(git2::ObjectType::Blob) {
            if let Some(name) = entry.name() {
                if let Ok(blob) = repo.find_blob(entry.id()) {
                    let path = format!("{dir}{name}");
                    files.insert(path, hex::encode(Sha256::digest(blob.content())));
                }
            }
        }
        git2::TreeWalkResult::Ok
    })
    .context("failed to walk snapshot Git tree")?;
    Ok(files)
}

enum CommittedBlob {
    /// Small enough to be an LFS pointer (or a normal small file).
    Inline(Vec<u8>),
    /// Larger than a pointer; only the raw digest is kept.
    Digest(String),
}

fn git_commit_blobs(git_workdir: &Path, sha: &str) -> Result<BTreeMap<String, CommittedBlob>> {
    let repo = git2::Repository::open(git_workdir).context("failed to open Git workdir")?;
    let oid = git2::Oid::from_str(sha).context("snapshot Git SHA is malformed")?;
    let commit = repo
        .find_commit(oid)
        .context("snapshot Git commit is missing")?;
    let tree = commit.tree().context("snapshot Git tree is missing")?;
    let mut files = BTreeMap::new();
    tree.walk(git2::TreeWalkMode::PreOrder, |dir, entry| {
        if entry.kind() == Some(git2::ObjectType::Blob) {
            if let Some(name) = entry.name() {
                if let Ok(blob) = repo.find_blob(entry.id()) {
                    let path = format!("{dir}{name}");
                    let content = blob.content();
                    let record = if content.len() <= 512 {
                        CommittedBlob::Inline(content.to_vec())
                    } else {
                        CommittedBlob::Digest(hex::encode(Sha256::digest(content)))
                    };
                    files.insert(path, record);
                }
            }
        }
        git2::TreeWalkResult::Ok
    })
    .context("failed to walk snapshot Git tree")?;
    Ok(files)
}

fn committed_blob_matches(
    policy: &FilePolicy,
    path: &str,
    expected_hash: &str,
    expected_size: u64,
    blob: &CommittedBlob,
) -> bool {
    match policy.evaluate(path, expected_size) {
        FilePolicyDecision::LfsTrack { .. } => {
            let CommittedBlob::Inline(bytes) = blob else {
                return false;
            };
            let Some(pointer) = crate::lfs::parse_lfs_pointer(bytes) else {
                return false;
            };
            pointer.oid == expected_hash && pointer.size == expected_size
        }
        _ => {
            let actual = match blob {
                CommittedBlob::Inline(bytes) => hex::encode(Sha256::digest(bytes)),
                CommittedBlob::Digest(hash) => hash.clone(),
            };
            actual == expected_hash
        }
    }
}

pub fn verify_commit_matches_projection(
    projected_root: &Path,
    git_workdir: &Path,
    sha: &str,
    policy: &FilePolicy,
) -> Result<VerificationResult> {
    let expected = content_index(projected_root)?;
    let actual = git_commit_blobs(git_workdir, sha)?;
    let mut result = VerificationResult {
        files_checked: expected.len() as u64,
        ..VerificationResult::default()
    };
    for (path, hash) in &expected {
        let size = match std::fs::metadata(projected_root.join(path)) {
            Ok(meta) => meta.len(),
            Err(_) => {
                result.mismatches.push(path.clone());
                continue;
            }
        };
        match actual.get(path) {
            Some(blob) if committed_blob_matches(policy, path, hash, size, blob) => {
                result.files_matched += 1;
            }
            Some(_) => result.mismatches.push(path.clone()),
            None => result.svn_only.push(path.clone()),
        }
    }
    for path in actual.keys() {
        if !expected.contains_key(path) {
            result.git_only.push(path.clone());
        }
    }
    result.sample_hashed = result.files_checked;
    result.verified = result.mismatches.is_empty()
        && result.svn_only.is_empty()
        && result.git_only.is_empty()
        && result.files_matched == result.files_checked;
    if !result.verified {
        bail!(
            "committed snapshot tree does not match projected SVN content: mismatches={:?} svn_only={:?} git_only={:?}",
            result.mismatches,
            result.svn_only,
            result.git_only
        );
    }
    Ok(result)
}

/// Project the pin into a temp directory for post-commit tree comparison.
pub async fn project_snapshot_tree(
    svn: &SvnClient,
    policy: &FilePolicy,
    db: &Database,
    pin: &SnapshotPin,
) -> Result<tempfile::TempDir> {
    let export_dir = tempfile::tempdir().context("failed to export snapshot for commit verify")?;
    svn.export("", pin.operative_rev, export_dir.path())
        .await
        .context("failed to export pinned revision for commit verify")?;
    let projected = tempfile::tempdir().context("failed to create commit-verify projection")?;
    copy_tree_with_policy(export_dir.path(), projected.path(), policy, db)
        .context("failed to project pinned tree for commit verify")?;
    let _ = export_dir;
    Ok(projected)
}

pub fn relative_paths(root: &Path) -> Result<Vec<PathBuf>> {
    Ok(content_index(root)?
        .into_keys()
        .map(PathBuf::from)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_revision_parse_defaults_to_head() {
        assert_eq!(
            SnapshotRevision::parse(None).unwrap(),
            SnapshotRevision::Head
        );
        assert_eq!(
            SnapshotRevision::parse(Some("HEAD")).unwrap(),
            SnapshotRevision::Head
        );
        assert_eq!(
            SnapshotRevision::parse(Some("  4  ")).unwrap(),
            SnapshotRevision::Number(4)
        );
        assert!(SnapshotRevision::parse(Some("0")).is_err());
        assert!(SnapshotRevision::parse(Some("-2")).is_err());
        assert!(SnapshotRevision::parse(Some("tip")).is_err());
    }

    #[test]
    fn projected_content_index_detects_byte_mismatch() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        std::fs::write(a.path().join("keep.txt"), "same\n").unwrap();
        std::fs::write(b.path().join("keep.txt"), "different\n").unwrap();
        let left = content_index(a.path()).unwrap();
        let right = content_index(b.path()).unwrap();
        assert_ne!(left.get("keep.txt"), right.get("keep.txt"));
    }

    #[test]
    fn history_boundary_never_claims_earlier_history() {
        let pin = SnapshotPin {
            svn_uuid: "uuid".into(),
            canonical_url: "file:///tmp/repo/trunk".into(),
            operative_rev: 7,
            peg_rev: 7,
            copy_from_path: None,
            copy_from_rev: None,
            requested: "HEAD".into(),
        };
        let text = pin.history_boundary();
        assert!(text.contains("r7"));
        assert!(text.contains("was not imported"));
        assert!(!text.to_lowercase().contains("full history"));
    }

    fn commit_files(dir: &Path, files: &[(&str, &[u8])]) -> String {
        let repo = git2::Repository::init(dir).unwrap();
        let mut index = repo.index().unwrap();
        for (name, bytes) in files {
            std::fs::write(dir.join(name), bytes).unwrap();
            index.add_path(Path::new(name)).unwrap();
        }
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("Snapshot", "snapshot@example.com").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "baseline", &tree, &[])
            .unwrap()
            .to_string()
    }

    #[test]
    fn lfs_pointer_matches_projected_bytes_and_fat_or_wrong_pointer_does_not() {
        let raw = b"0123456789abcdefRAW";
        assert_eq!(raw.len(), 19);
        let policy = FilePolicy::with_lfs(0, vec![], 8, &[]);
        let plain = FilePolicy::new(0, vec![]);
        let pointer = crate::lfs::create_lfs_pointer(raw);
        let projected = tempfile::tempdir().unwrap();
        std::fs::write(projected.path().join("large.bin"), raw).unwrap();
        std::fs::write(projected.path().join("note.txt"), b"ok\n").unwrap();

        let good = tempfile::tempdir().unwrap();
        let sha = commit_files(
            good.path(),
            &[("large.bin", pointer.as_bytes()), ("note.txt", b"ok\n")],
        );
        assert!(
            verify_commit_matches_projection(projected.path(), good.path(), &sha, &policy).is_ok()
        );

        let fat = tempfile::tempdir().unwrap();
        let fat_sha = commit_files(fat.path(), &[("large.bin", raw), ("note.txt", b"ok\n")]);
        assert!(
            verify_commit_matches_projection(projected.path(), fat.path(), &fat_sha, &policy)
                .is_err()
        );
        assert!(
            verify_commit_matches_projection(projected.path(), fat.path(), &fat_sha, &plain)
                .is_ok()
        );

        let wrong = pointer.replace("size 19", "size 18");
        let bad = tempfile::tempdir().unwrap();
        let bad_sha = commit_files(
            bad.path(),
            &[("large.bin", wrong.as_bytes()), ("note.txt", b"ok\n")],
        );
        assert!(
            verify_commit_matches_projection(projected.path(), bad.path(), &bad_sha, &policy)
                .is_err()
        );
    }

    #[test]
    fn snapshot_materialize_writer_preserves_dotfiles_and_rejects_outside_canary() {
        use crate::db::Database;
        use crate::import::{
            independent_tree_manifest, verify_against_independent_manifest, ManifestEntry,
        };

        let export = tempfile::tempdir().unwrap();
        let workdir = tempfile::tempdir().unwrap();
        std::fs::write(export.path().join(".gitignore"), "target/\n").unwrap();
        std::fs::write(export.path().join("app.rs"), "fn main() {}\n").unwrap();
        std::fs::create_dir(workdir.path().join(".git")).unwrap();

        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        let policy = FilePolicy::new(0, vec![]);
        let stats = copy_tree_with_policy(export.path(), workdir.path(), &policy, &db).unwrap();
        assert_eq!(stats.copied, 2);
        let manifest = independent_tree_manifest(export.path()).unwrap();
        verify_against_independent_manifest(workdir.path(), &manifest, &policy).unwrap();

        let outside = tempfile::tempdir().unwrap();
        let canary = b"SNAPSHOT-OUTSIDE-CANARY-88";
        std::fs::write(outside.path().join("secret"), canary).unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret"), export.path().join("escape"))
            .unwrap();
        let poisoned = tempfile::tempdir().unwrap();
        let err = copy_tree_with_policy(export.path(), poisoned.path(), &policy, &db).unwrap_err();
        assert!(err.to_string().contains("unsupported symlink"), "{err}");
        assert!(!poisoned.path().join("escape").exists());

        let src_manifest = independent_tree_manifest(export.path()).unwrap();
        assert!(matches!(
            src_manifest.entries.get("escape"),
            Some(ManifestEntry::Unsupported { kind, .. }) if kind == "symlink"
        ));
        assert!(!src_manifest.contains_file_digest_of(canary));
    }

    #[test]
    fn snapshot_materialize_writer_rejects_outside_hardlink_canary() {
        use crate::db::Database;
        use crate::import::{independent_tree_manifest, ManifestEntry};

        let export = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let canary = b"SNAPSHOT-HARDLINK-CANARY-88";
        std::fs::write(outside.path().join("secret"), canary).unwrap();
        std::fs::write(export.path().join("app.rs"), "fn main() {}\n").unwrap();
        std::fs::hard_link(outside.path().join("secret"), export.path().join("escape")).unwrap();

        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        let policy = FilePolicy::new(0, vec![]);
        let poisoned = tempfile::tempdir().unwrap();
        let err = copy_tree_with_policy(export.path(), poisoned.path(), &policy, &db).unwrap_err();
        assert!(err.to_string().contains("unsupported hardlink"), "{err}");
        assert!(!poisoned.path().join("escape").exists());
        let published_canary = std::fs::read_dir(poisoned.path()).unwrap().any(|entry| {
            let entry = entry.unwrap();
            std::fs::read(entry.path())
                .map(|bytes| bytes.windows(canary.len()).any(|w| w == canary))
                .unwrap_or(false)
        });
        assert!(
            !published_canary,
            "snapshot writer published outside-root hardlink bytes"
        );

        let src_manifest = independent_tree_manifest(export.path()).unwrap();
        assert!(matches!(
            src_manifest.entries.get("escape"),
            Some(ManifestEntry::Unsupported { kind, detail })
                if kind == "hardlink" && detail.contains("not opened")
        ));
        assert!(!src_manifest.contains_file_digest_of(canary));
    }
}
