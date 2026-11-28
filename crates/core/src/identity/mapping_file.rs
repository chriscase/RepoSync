//! TOML-based identity mapping file reader/writer.
//!
//! The mapping file format:
//!
//! ```toml
//! [authors]
//! jdoe = { name = "John Doe", email = "jdoe@example.com" }
//! alice = { name = "Alice Smith", email = "alice@example.com" }
//! ```

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::errors::IdentityError;

/// A single author entry in the mapping file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthorEntry {
    /// Git display name.
    pub name: String,
    /// Git email address.
    pub email: String,
}

/// Wrapper around the TOML mapping file structure.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MappingFileData {
    /// The `[authors]` table mapping SVN username -> AuthorEntry.
    #[serde(default)]
    pub authors: HashMap<String, AuthorEntry>,
}

/// Utilities for loading and saving the identity mapping file.
pub struct MappingFile;

impl MappingFile {
    /// Load the mapping file from disk and return the author map.
    pub fn load<P: AsRef<Path>>(path: P) -> Result<HashMap<String, AuthorEntry>, IdentityError> {
        let path = path.as_ref();
        info!(path = %path.display(), "loading identity mapping file");

        if !path.exists() {
            return Err(IdentityError::MappingFileError {
                path: path.display().to_string(),
                detail: "file not found".into(),
            });
        }

        let contents = std::fs::read_to_string(path).map_err(IdentityError::IoError)?;
        let data: MappingFileData =
            toml::from_str(&contents).map_err(|e| IdentityError::ParseError(e.to_string()))?;

        debug!(count = data.authors.len(), "loaded author mappings");
        Ok(data.authors)
    }

    /// Save the author map back to disk in TOML format.
    pub fn save<P: AsRef<Path>>(
        path: P,
        mappings: &HashMap<String, AuthorEntry>,
    ) -> Result<(), IdentityError> {
        let path = path.as_ref();
        info!(path = %path.display(), "saving identity mapping file");

        let data = MappingFileData {
            authors: mappings.clone(),
        };

        let toml_str =
            toml::to_string_pretty(&data).map_err(|e| IdentityError::ParseError(e.to_string()))?;
        // Write a uniquely named sibling temporary file and rename it into
        // place, so a failed write never leaves a truncated mapping file. A
        // symlinked mapping file (even a dangling one) is updated at its
        // target, as a plain write would, rather than replaced by a regular
        // file, and the existing file's permissions are kept.
        let resolved = resolve_symlink_target(path);
        let path = resolved.as_path();
        let parent = match path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        // A new file is written directly so it gets the usual umask-based
        // mode (a temporary file would be created 0600); there is no
        // existing content a failed write could truncate.
        if std::fs::symlink_metadata(path).is_err() {
            std::fs::write(path, toml_str).map_err(IdentityError::IoError)?;
            debug!(count = mappings.len(), "saved new author mapping file");
            return Ok(());
        }
        let mut tmp = match tempfile::NamedTempFile::new_in(parent) {
            Ok(tmp) => tmp,
            // The directory may be writable only by an administrator while
            // the file itself was handed to the service; write in place then.
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                std::fs::write(path, toml_str).map_err(IdentityError::IoError)?;
                debug!(count = mappings.len(), "saved author mappings in place");
                return Ok(());
            }
            Err(e) => return Err(IdentityError::IoError(e)),
        };
        {
            use std::io::Write;
            tmp.write_all(toml_str.as_bytes())
                .and_then(|_| tmp.as_file().sync_all())
                .map_err(IdentityError::IoError)?;
        }
        if let Ok(meta) = std::fs::metadata(path) {
            std::fs::set_permissions(tmp.path(), meta.permissions())
                .map_err(IdentityError::IoError)?;
        }
        tmp.persist(path)
            .map_err(|e| IdentityError::IoError(e.error))?;

        debug!(count = mappings.len(), "saved author mappings");
        Ok(())
    }
}

/// Follow symlinks at `path` to the file a plain write would reach,
/// including the target of a dangling link.
fn resolve_symlink_target(path: &Path) -> std::path::PathBuf {
    if let Ok(p) = std::fs::canonicalize(path) {
        return p;
    }
    let mut current = path.to_path_buf();
    for _ in 0..40 {
        match std::fs::read_link(&current) {
            Ok(target) => {
                current = match current.parent() {
                    Some(parent) if target.is_relative() => parent.join(target),
                    _ => target,
                };
            }
            Err(_) => break,
        }
    }
    current
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_mapping_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("authors.toml");

        let content = r#"
[authors]
[authors.jdoe]
name = "John Doe"
email = "jdoe@example.com"

[authors.alice]
name = "Alice Smith"
email = "alice@example.com"
"#;
        std::fs::write(&path, content).unwrap();

        let mappings = MappingFile::load(&path).unwrap();
        assert_eq!(mappings.len(), 2);
        assert_eq!(mappings["jdoe"].name, "John Doe");
        assert_eq!(mappings["alice"].email, "alice@example.com");
    }

    #[test]
    fn test_save_and_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("authors.toml");

        let mut mappings = HashMap::new();
        mappings.insert(
            "bob".to_string(),
            AuthorEntry {
                name: "Bob Builder".to_string(),
                email: "bob@example.com".to_string(),
            },
        );

        MappingFile::save(&path, &mappings).unwrap();

        let reloaded = MappingFile::load(&path).unwrap();
        assert_eq!(reloaded.len(), 1);
        assert_eq!(reloaded["bob"].name, "Bob Builder");
    }

    #[cfg(unix)]
    #[test]
    fn save_updates_a_symlinked_file_at_its_target_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.toml");
        std::fs::write(&real, "[authors]\n").unwrap();
        let link = dir.path().join("authors.toml");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let mut mappings = HashMap::new();
        mappings.insert(
            "bob".to_string(),
            AuthorEntry {
                name: "Bob".to_string(),
                email: "bob@example.com".to_string(),
            },
        );
        MappingFile::save(&link, &mappings).unwrap();

        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(MappingFile::load(&real).unwrap().contains_key("bob"));
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 2, "{names:?}");
    }

    #[cfg(unix)]
    #[test]
    fn save_keeps_permissions_and_follows_a_dangling_link() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("authors.toml");
        std::fs::write(&path, "[authors]\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        MappingFile::save(&path, &HashMap::new()).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o640);

        let link = dir.path().join("link.toml");
        std::os::unix::fs::symlink("missing.toml", &link).unwrap();
        MappingFile::save(&link, &HashMap::new()).unwrap();
        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(dir.path().join("missing.toml").is_file());

        // A new file is not left private to the writing user (0600).
        let fresh = dir.path().join("fresh.toml");
        MappingFile::save(&fresh, &HashMap::new()).unwrap();
        let mode = std::fs::metadata(&fresh).unwrap().permissions().mode() & 0o777;
        let umask_mode = {
            let probe = dir.path().join("probe");
            std::fs::write(&probe, "").unwrap();
            std::fs::metadata(&probe).unwrap().permissions().mode() & 0o777
        };
        assert_eq!(mode, umask_mode);
    }

    #[test]
    fn test_load_nonexistent() {
        let result = MappingFile::load("/nonexistent/authors.toml");
        assert!(matches!(
            result,
            Err(IdentityError::MappingFileError { .. })
        ));
    }

    #[test]
    fn test_load_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.toml");
        std::fs::write(&path, "").unwrap();

        let mappings = MappingFile::load(&path).unwrap();
        assert!(mappings.is_empty());
    }
}
