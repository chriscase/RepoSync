//! Typed Git→SVN projected changeset.
//!
//! Team Git→SVN computes this set **before** any SVN working-copy mutation.
//! Staging, no-delta verification, the commit journal, and no-target receipts
//! in this slice consume the same included paths. SVN→Git filtering (#53),
//! personal-mode rules (#59), and the full monorepo rewrite product (#52/#57)
//! stay out of scope.

/// One Git path after allow/block projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectedGitToSvnChange {
    pub action: String,
    pub path: String,
    pub content: Option<Vec<u8>>,
}

/// Projected Git→SVN changeset: included paths drive the write; excluded
/// paths must not touch the SVN working copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectedGitToSvnChangeset {
    pub included: Vec<ProjectedGitToSvnChange>,
    pub excluded: Vec<(String, String)>,
}

impl ProjectedGitToSvnChangeset {
    pub fn is_empty(&self) -> bool {
        self.included.is_empty()
    }

    pub fn into_file_contents(self) -> Vec<(String, String, Option<Vec<u8>>)> {
        self.included
            .into_iter()
            .map(|change| (change.action, change.path, change.content))
            .collect()
    }

    pub fn exclusion_messages(&self, allowed: &[String], blocked: &[String]) -> Vec<String> {
        self.excluded
            .iter()
            .map(|(_, path)| exclusion_message(path, allowed, blocked))
            .collect()
    }
}

/// Normalize a relative Git/SVN path for policy matching.
pub fn normalize_policy_path(path: &str) -> String {
    path.replace('\\', "/").trim_start_matches('/').to_string()
}

/// Component-aware prefix match.
///
/// `team` matches `team` and `team/foo`, not sibling `team-other`.
/// A trailing slash on the prefix is ignored (`team/` ≡ `team`).
pub fn path_matches_prefix(path: &str, prefix: &str) -> bool {
    let path = normalize_policy_path(path);
    let prefix = normalize_policy_path(prefix);
    let prefix = prefix.trim_end_matches('/');
    if prefix.is_empty() {
        return false;
    }
    path == prefix || path.starts_with(&format!("{prefix}/"))
}

/// Blocked-pattern match (suffix glob, directory prefix, or exact/component path).
pub fn path_matches_blocked(path: &str, pattern: &str) -> bool {
    let path = normalize_policy_path(path);
    if pattern.is_empty() {
        return false;
    }
    if let Some(suffix) = pattern.strip_prefix('*') {
        path.ends_with(suffix)
    } else {
        path_matches_prefix(&path, pattern)
    }
}

/// Whether `path` is inside the active Git→SVN projection.
///
/// Deletes use the same rule as adds/modifies. Empty allow *and* empty block
/// means the whole tree is in scope.
pub fn path_is_projected(path: &str, allowed: &[String], blocked: &[String]) -> bool {
    if allowed.is_empty() && blocked.is_empty() {
        return true;
    }
    if !allowed.is_empty()
        && !allowed
            .iter()
            .any(|prefix| path_matches_prefix(path, prefix))
    {
        return false;
    }
    !blocked
        .iter()
        .any(|pattern| path_matches_blocked(path, pattern))
}

pub fn exclusion_message(path: &str, allowed: &[String], blocked: &[String]) -> String {
    if !allowed.is_empty()
        && !allowed
            .iter()
            .any(|prefix| path_matches_prefix(path, prefix))
    {
        format!("'{path}' not under allowed paths {allowed:?}")
    } else if let Some(pattern) = blocked
        .iter()
        .find(|pattern| path_matches_blocked(path, pattern))
    {
        format!("'{path}' matches blocked pattern '{pattern}'")
    } else {
        format!("'{path}' is outside the active projection")
    }
}

/// Compute the typed projected changeset from raw Git file contents.
///
/// This is the single Git→SVN path filter: allow prefixes, blocked patterns,
/// and deletes. Call it before checkout, write, `svn add`, or `svn rm`.
pub fn project_git_to_svn_changeset(
    files: Vec<(String, String, Option<Vec<u8>>)>,
    allowed: &[String],
    blocked: &[String],
) -> ProjectedGitToSvnChangeset {
    if allowed.is_empty() && blocked.is_empty() {
        return ProjectedGitToSvnChangeset {
            included: files
                .into_iter()
                .map(|(action, path, content)| ProjectedGitToSvnChange {
                    action,
                    path,
                    content,
                })
                .collect(),
            excluded: Vec::new(),
        };
    }
    let mut included = Vec::new();
    let mut excluded = Vec::new();
    for (action, path, content) in files {
        if path_is_projected(&path, allowed, blocked) {
            included.push(ProjectedGitToSvnChange {
                action,
                path,
                content,
            });
        } else {
            excluded.push((action, path));
        }
    }
    ProjectedGitToSvnChangeset { included, excluded }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(action: &str, path: &str) -> (String, String, Option<Vec<u8>>) {
        (action.to_string(), path.to_string(), None)
    }

    #[test]
    fn prefix_is_component_aware() {
        assert!(path_matches_prefix("team/foo.txt", "team"));
        assert!(path_matches_prefix("team/foo.txt", "team/"));
        assert!(path_matches_prefix("team", "team"));
        assert!(!path_matches_prefix("team-other/foo.txt", "team"));
        assert!(!path_matches_prefix("team-other/foo.txt", "team/"));
        assert!(!path_matches_prefix("teams/foo.txt", "team"));
        assert!(!path_matches_prefix("allow.txt", "allow"));
        assert!(path_matches_prefix("allow.txt", "allow.txt"));
    }

    #[test]
    fn blocked_suffix_and_directory() {
        assert!(path_matches_blocked("program.exe", "*.exe"));
        assert!(!path_matches_blocked("program.exe.bak", "*.exe"));
        assert!(path_matches_blocked("secret/leak.txt", "secret/"));
        assert!(path_matches_blocked("secret/leak.txt", "secret"));
        assert!(!path_matches_blocked("secret-other/leak.txt", "secret"));
        assert!(!path_matches_blocked("secret-other/leak.txt", "secret/"));
    }

    #[test]
    fn deletes_obey_allow_and_block() {
        let allowed = vec!["keep/".to_string()];
        assert!(path_is_projected("keep/a.txt", &allowed, &[]));
        assert!(!path_is_projected("origin.txt", &allowed, &[]));
        let blocked = vec!["secret/".to_string()];
        assert!(!path_is_projected("secret/gone.txt", &[], &blocked));
        assert!(path_is_projected("public.txt", &[], &blocked));
    }

    #[test]
    fn project_splits_mixed_and_keeps_deletes_in_scope() {
        let files = vec![
            file("A", "team/ok.txt"),
            file("A", "team-other/leak.txt"),
            file("D", "origin.txt"),
            file("M", "team/inner.txt"),
            file("A", "skip.exe"),
        ];
        let projected =
            project_git_to_svn_changeset(files, &["team".to_string()], &["*.exe".to_string()]);
        let included: Vec<_> = projected
            .included
            .iter()
            .map(|c| (c.action.as_str(), c.path.as_str()))
            .collect();
        assert_eq!(
            included,
            vec![("A", "team/ok.txt"), ("M", "team/inner.txt")]
        );
        let excluded: Vec<_> = projected
            .excluded
            .iter()
            .map(|(a, p)| (a.as_str(), p.as_str()))
            .collect();
        assert_eq!(
            excluded,
            vec![
                ("A", "team-other/leak.txt"),
                ("D", "origin.txt"),
                ("A", "skip.exe")
            ]
        );
    }

    #[test]
    fn empty_rules_include_everything() {
        let files = vec![file("A", "anywhere.txt"), file("D", "gone.txt")];
        let projected = project_git_to_svn_changeset(files, &[], &[]);
        assert_eq!(projected.included.len(), 2);
        assert!(projected.excluded.is_empty());
    }
}
