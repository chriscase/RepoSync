//! Confined cleanup of one repository's RepoSync-owned working tree.
//!
//! The managed root is `{data_dir}/repos/{repo_id}`. Cleanup refuses symlink
//! roots, path traversal, and any directory that is not a direct child of the
//! canonical managed root. Nested symlinks are unlinked as links and are not
//! followed. Sibling directories and the managed root itself are left in place.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Why a local cleanup path was refused or could not be removed.
#[derive(Debug)]
pub struct OwnedPathError {
    pub message: String,
}

impl std::fmt::Display for OwnedPathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for OwnedPathError {}

fn path_err(message: impl Into<String>) -> OwnedPathError {
    OwnedPathError {
        message: message.into(),
    }
}

/// Repository ids become a single path component. Reject traversal and separators.
pub fn validate_repo_id(repo_id: &str) -> Result<(), OwnedPathError> {
    if repo_id.is_empty()
        || repo_id.len() > 128
        || repo_id == "."
        || repo_id == ".."
        || !repo_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(path_err(
            "repository id is not a single safe path component under the managed root",
        ));
    }
    Ok(())
}

fn strict_child(parent: &Path, child: &Path) -> bool {
    child.parent() == Some(parent) && child.starts_with(parent) && child != parent
}

fn canonical_dir(path: &Path) -> Result<PathBuf, OwnedPathError> {
    path.canonicalize()
        .map_err(|error| path_err(format!("cannot canonicalize {}: {error}", path.display())))
}

/// Delete `{data_dir}/repos/{repo_id}` when it is a real directory or file
/// owned by that managed root. A missing path is success. A symlink at the
/// managed path is an error and is not followed.
pub fn remove_owned_repo_tree(data_dir: &Path, repo_id: &str) -> Result<(), OwnedPathError> {
    validate_repo_id(repo_id)?;
    let data_canon = canonical_dir(data_dir)?;
    let repos = data_dir.join("repos");
    let target = repos.join(repo_id);
    let meta = match fs::symlink_metadata(&target) {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(path_err(format!(
                "cannot inspect owned path {}: {error}",
                target.display()
            )))
        }
    };
    if meta.file_type().is_symlink() {
        return Err(path_err(format!(
            "refusing to follow symlink at managed path {}",
            target.display()
        )));
    }
    if !repos.exists() {
        return Err(path_err(
            "managed repos root disappeared while inspecting an owned path",
        ));
    }
    let repos_meta = fs::symlink_metadata(&repos).map_err(|error| {
        path_err(format!(
            "cannot inspect managed repos root {}: {error}",
            repos.display()
        ))
    })?;
    if repos_meta.file_type().is_symlink() {
        return Err(path_err(
            "refusing to follow a symlink at the managed repos root",
        ));
    }
    let repos_canon = canonical_dir(&repos)?;
    if !repos_canon.starts_with(&data_canon) {
        return Err(path_err("managed repos root is outside the data directory"));
    }
    let target_canon = canonical_dir(&target)?;
    if !strict_child(&repos_canon, &target_canon)
        || target_canon.file_name().and_then(|n| n.to_str()) != Some(repo_id)
    {
        return Err(path_err(format!(
            "owned path {} escaped the managed repos root",
            target.display()
        )));
    }
    if meta.is_dir() {
        remove_tree_nofollow(&target_canon, &repos_canon)?;
    } else {
        fs::remove_file(&target_canon).map_err(|error| {
            path_err(format!(
                "failed to remove owned file {}: {error}",
                target_canon.display()
            ))
        })?;
    }
    Ok(())
}

fn remove_tree_nofollow(dir: &Path, repos_root: &Path) -> Result<(), OwnedPathError> {
    if !dir.starts_with(repos_root) || dir == repos_root {
        return Err(path_err(format!(
            "refusing to delete {} outside the managed repos root",
            dir.display()
        )));
    }
    let entries = fs::read_dir(dir).map_err(|error| {
        path_err(format!(
            "cannot read owned directory {}: {error}",
            dir.display()
        ))
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            path_err(format!(
                "cannot read entry under {}: {error}",
                dir.display()
            ))
        })?;
        let path = entry.path();
        let meta = fs::symlink_metadata(&path)
            .map_err(|error| path_err(format!("cannot inspect {}: {error}", path.display())))?;
        if meta.file_type().is_symlink() {
            fs::remove_file(&path).map_err(|error| {
                path_err(format!(
                    "failed to unlink symlink {} without following it: {error}",
                    path.display()
                ))
            })?;
            continue;
        }
        if meta.is_dir() {
            let canon = canonical_dir(&path)?;
            if !canon.starts_with(dir) || canon == repos_root {
                return Err(path_err(format!(
                    "refusing escaped directory {}",
                    path.display()
                )));
            }
            remove_tree_nofollow(&canon, repos_root)?;
        } else {
            let canon = canonical_dir(&path)?;
            if !canon.starts_with(dir) {
                return Err(path_err(format!(
                    "refusing escaped file {}",
                    path.display()
                )));
            }
            fs::remove_file(&path).map_err(|error| {
                path_err(format!("failed to remove {}: {error}", path.display()))
            })?;
        }
    }
    fs::remove_dir(dir).map_err(|error| {
        path_err(format!(
            "failed to remove owned directory {}: {error}",
            dir.display()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_r02_path_confinement_blocks_symlink_and_sibling_escape() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        let repos = data.join("repos");
        fs::create_dir_all(repos.join("sibling")).unwrap();
        fs::write(repos.join("sibling").join("keep.txt"), "sibling\n").unwrap();
        let outside = tmp.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret.txt"), "do-not-delete\n").unwrap();

        let owned = repos.join("repo-1");
        fs::create_dir_all(owned.join("git-repo")).unwrap();
        fs::write(owned.join("git-repo").join("owned.txt"), "owned\n").unwrap();
        std::os::unix::fs::symlink(outside.join("secret.txt"), owned.join("escape")).unwrap();

        remove_owned_repo_tree(&data, "repo-1").unwrap();
        assert!(!owned.exists());
        assert_eq!(
            fs::read_to_string(outside.join("secret.txt")).unwrap(),
            "do-not-delete\n"
        );
        assert_eq!(
            fs::read_to_string(repos.join("sibling").join("keep.txt")).unwrap(),
            "sibling\n"
        );
        assert!(repos.is_dir());

        fs::create_dir_all(&owned).unwrap();
        std::os::unix::fs::symlink(&outside, repos.join("linked")).unwrap();
        let error = remove_owned_repo_tree(&data, "linked").unwrap_err();
        assert!(error.message.contains("symlink"), "{error}");
        assert_eq!(
            fs::read_to_string(outside.join("secret.txt")).unwrap(),
            "do-not-delete\n"
        );
        assert!(repos
            .join("linked")
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink());

        for id in ["../outside", "repo/child", "", ".", "..", "repo 1"] {
            let error = remove_owned_repo_tree(&data, id).unwrap_err();
            assert!(
                error.message.contains("safe path component"),
                "{id}: {error}"
            );
        }
        assert_eq!(
            fs::read_to_string(outside.join("secret.txt")).unwrap(),
            "do-not-delete\n"
        );
        assert!(owned.is_dir());
    }
}
