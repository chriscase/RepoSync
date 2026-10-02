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

/// Trim trailing slashes from an SVN URL for stable prefix comparison.
pub fn normalize_svn_url(url: &str) -> String {
    url.trim_end_matches('/').to_string()
}

/// Pinned SVN path identity for Git→SVN journal and lost-reply matching.
///
/// Repository-relative SVN log paths and Git intent paths are compared only
/// after mapping both sides into the same branch-relative namespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SvnPathIdentity {
    /// Repository root URL from `svn info` (for example `file:///repo`).
    pub root_url: String,
    /// Branch path relative to root (`trunk`, `branches/feature`, or empty at repo root).
    pub branch_path: String,
}

impl SvnPathIdentity {
    pub fn new(root_url: impl Into<String>, target_url: impl Into<String>) -> Self {
        svn_path_identity(&root_url.into(), &target_url.into())
    }
}

/// Derive the canonical branch-relative namespace from pinned SVN URLs.
pub fn svn_path_identity(root_url: &str, target_url: &str) -> SvnPathIdentity {
    let root = normalize_svn_url(root_url);
    let target = normalize_svn_url(target_url);
    let branch_path = if target == root {
        String::new()
    } else if let Some(rest) = target.strip_prefix(&format!("{root}/")) {
        normalize_policy_path(rest)
    } else {
        String::new()
    };
    SvnPathIdentity {
        root_url: root,
        branch_path,
    }
}

/// Git intent paths are already branch-relative; normalize separators only.
pub fn git_intent_path(path: &str) -> String {
    normalize_policy_path(path)
}

/// Map a repository-relative SVN log path into the pinned branch-relative namespace.
///
/// Returns `None` when the path is outside the pinned branch (component-aware;
/// no suffix-only matching).
pub fn svn_log_path_to_branch_relative(
    log_path: &str,
    identity: &SvnPathIdentity,
) -> Option<String> {
    let repo_relative = normalize_policy_path(log_path);
    if identity.branch_path.is_empty() {
        return Some(repo_relative);
    }
    let prefix = identity.branch_path.trim_end_matches('/');
    if repo_relative == prefix {
        return Some(String::new());
    }
    let with_slash = format!("{prefix}/");
    if repo_relative.starts_with(&with_slash) {
        return Some(repo_relative[with_slash.len()..].to_string());
    }
    None
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

    #[test]
    fn svn_path_identity_maps_trunk_and_branches() {
        let root = "file:///srv/repo";
        assert_eq!(svn_path_identity(root, root).branch_path, "");
        assert_eq!(
            svn_path_identity(root, "file:///srv/repo/trunk").branch_path,
            "trunk"
        );
        assert_eq!(
            svn_path_identity(root, "file:///srv/repo/branches/feature").branch_path,
            "branches/feature"
        );
        assert_eq!(
            svn_path_identity(root, "file:///srv/repo/projects/app").branch_path,
            "projects/app"
        );
    }

    #[test]
    fn svn_log_path_maps_into_branch_relative_namespace() {
        let trunk = SvnPathIdentity::new("file:///repo", "file:///repo/trunk");
        assert_eq!(
            svn_log_path_to_branch_relative("/trunk/feature.txt", &trunk),
            Some("feature.txt".into())
        );
        assert_eq!(
            svn_log_path_to_branch_relative("trunk/nested/x.txt", &trunk),
            Some("nested/x.txt".into())
        );
        let branch = SvnPathIdentity::new("file:///repo", "file:///repo/branches/team");
        assert_eq!(
            svn_log_path_to_branch_relative("/branches/team/a.txt", &branch),
            Some("a.txt".into())
        );
        assert_eq!(
            svn_log_path_to_branch_relative("/branches/team-other/a.txt", &branch),
            None
        );
        let root = SvnPathIdentity::new("file:///repo", "file:///repo");
        assert_eq!(
            svn_log_path_to_branch_relative("/feature.txt", &root),
            Some("feature.txt".into())
        );
    }

    #[test]
    fn svn_log_path_rejects_sibling_same_basename_prefix() {
        let allow = SvnPathIdentity::new("file:///repo", "file:///repo/allow");
        assert_eq!(svn_log_path_to_branch_relative("/allow.txt", &allow), None);
        assert_eq!(
            svn_log_path_to_branch_relative("/allow/x.txt", &allow),
            Some("x.txt".into())
        );
    }
}
