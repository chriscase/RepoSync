//! Typed Git→SVN projected changeset.
//!
//! Team Git→SVN computes this set **before** any SVN working-copy mutation.
//! Staging, no-delta verification, the commit journal, and no-target receipts
//! in this slice consume the same included paths. SVN→Git filtering (#53),
//! personal-mode rules (#59), and the full monorepo rewrite product (#52/#57)
//! stay out of scope.

use thiserror::Error;

/// A Git rename (`R`) arrived without its source path.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("git rename for '{path}' is missing rename_from")]
pub struct IncompleteGitRenameError {
    pub path: String,
}

/// One Git path after allow/block projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectedGitToSvnChange {
    pub action: String,
    pub path: String,
    pub content: Option<Vec<u8>>,
}

/// Raw Git change before allow/block projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitToSvnInputChange {
    pub action: String,
    pub path: String,
    pub content: Option<Vec<u8>>,
    pub rename_from: Option<String>,
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

/// Escape `s` for use inside single-quoted Bash literals.
pub fn bash_single_quoted_literal(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Bash `[[ ... ]]` test for one allowed prefix (component-aware, matches
/// [`path_matches_prefix`]). Prefixes are embedded as single-quoted literals so
/// `$(...)`, backticks, and newlines cannot execute.
pub fn bash_allowed_prefix_test(file_expr: &str, prefix: &str) -> String {
    let prefix = normalize_policy_path(prefix);
    let prefix = prefix.trim_end_matches('/');
    if prefix.is_empty() {
        return "false".to_string();
    }
    let quoted = bash_single_quoted_literal(prefix);
    format!(
        "[[ {file} == {quoted} || {file} == {quoted}/* ]]",
        file = file_expr
    )
}

/// One `case` arm for a blocked pattern (matches [`path_matches_blocked`]).
pub fn bash_blocked_pattern_case_arm(pattern: &str) -> String {
    let pattern = normalize_policy_path(pattern);
    if pattern.is_empty() {
        return String::new();
    }
    if let Some(suffix) = pattern.strip_prefix('*') {
        return format!("*{}", bash_single_quoted_literal(suffix));
    }
    let prefix = pattern.trim_end_matches('/');
    if prefix.is_empty() {
        return String::new();
    }
    format!(
        "{literal}|{literal}/*",
        literal = bash_single_quoted_literal(prefix)
    )
}

/// Render the pre-commit hook script that enforces repository path rules.
pub fn render_pre_commit_hook_script(allowed: &[String], blocked: &[String]) -> String {
    let mut script = String::from("#!/bin/bash\n");
    script.push_str("# RepoSync pre-commit hook — validates file paths against SVN rules\n");
    script.push_str(
        "# Install: cp this file .git/hooks/pre-commit && chmod +x .git/hooks/pre-commit\n",
    );
    script.push_str(
        "# Or: mkdir -p .githooks && cp this file .githooks/pre-commit && git config core.hooksPath .githooks\n\n",
    );

    if allowed.is_empty() && blocked.is_empty() {
        script.push_str("# No path rules configured for this repository.\nexit 0\n");
        return script;
    }

    script.push_str("ERRORS=0\n\n");

    if !allowed.is_empty() {
        script.push_str(
            "# Allowed path prefixes (component-aware; team does not match team-other)\n",
        );
        script.push_str("for file in $(git diff --cached --name-only --diff-filter=ACM); do\n");
        script.push_str("  ALLOWED=0\n");
        for prefix in allowed {
            let test = bash_allowed_prefix_test("$file", prefix);
            script.push_str(&format!("  if {test}; then\n"));
            script.push_str("    ALLOWED=1\n");
            script.push_str("    break\n");
            script.push_str("  fi\n");
        }
        script.push_str("  if [ $ALLOWED -eq 0 ]; then\n");
        script.push_str("    echo \"ERROR: '$file' is not under an allowed path prefix\"\n");
        script.push_str("    ERRORS=$((ERRORS + 1))\n");
        script.push_str("  fi\n");
        script.push_str("done\n\n");
    }

    if !blocked.is_empty() {
        script.push_str("# Blocked patterns (component-aware; secret matches secret/leak.txt)\n");
        let arms: Vec<String> = blocked
            .iter()
            .map(|pattern| bash_blocked_pattern_case_arm(pattern))
            .filter(|arm| !arm.is_empty())
            .collect();
        if !arms.is_empty() {
            script.push_str(
                "for file in $(git diff --cached --name-only --diff-filter=ACM); do\n  case \"$file\" in\n",
            );
            for (index, arm) in arms.iter().enumerate() {
                if index > 0 {
                    script.push('|');
                }
                script.push_str(arm);
            }
            script.push_str(
                ")\n      echo \"ERROR: '$file' matches a blocked path pattern\"\n      ERRORS=$((ERRORS + 1))\n      ;;\n  esac\ndone\n\n",
            );
        }
    }

    script.push_str("if [ $ERRORS -gt 0 ]; then\n");
    script.push_str("  echo \"\"\n");
    script.push_str("  echo \"Commit blocked: $ERRORS file(s) violate SVN path rules.\"\n");
    script.push_str("  echo \"These files would be rejected by the SVN server.\"\n");
    script.push_str("  exit 1\n");
    script.push_str("fi\n");
    script
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

/// Project one Git rename/move per endpoint.
///
/// Allowed-old to blocked/out-of-scope-new keeps only the delete. Blocked/out-
/// of-scope-old to allowed-new keeps only the add. Both allowed becomes delete
/// plus add. Both blocked excludes the rename entirely.
pub fn project_rename_endpoints(
    rename_from: &str,
    rename_to: &str,
    content: Option<Vec<u8>>,
    allowed: &[String],
    blocked: &[String],
) -> (Vec<ProjectedGitToSvnChange>, Vec<(String, String)>) {
    let old_in = path_is_projected(rename_from, allowed, blocked);
    let new_in = path_is_projected(rename_to, allowed, blocked);
    let mut included = Vec::new();
    let mut excluded = Vec::new();
    match (old_in, new_in) {
        (true, true) => {
            included.push(ProjectedGitToSvnChange {
                action: "D".into(),
                path: rename_from.to_string(),
                content: None,
            });
            included.push(ProjectedGitToSvnChange {
                action: "A".into(),
                path: rename_to.to_string(),
                content,
            });
        }
        (true, false) => {
            included.push(ProjectedGitToSvnChange {
                action: "D".into(),
                path: rename_from.to_string(),
                content: None,
            });
            excluded.push(("R".into(), format!("{rename_from} -> {rename_to}")));
        }
        (false, true) => {
            included.push(ProjectedGitToSvnChange {
                action: "A".into(),
                path: rename_to.to_string(),
                content,
            });
            excluded.push(("R".into(), format!("{rename_from} -> {rename_to}")));
        }
        (false, false) => {
            excluded.push(("R".into(), format!("{rename_from} -> {rename_to}")));
        }
    }
    (included, excluded)
}

/// Compute the typed projected changeset from raw Git file contents.
///
/// This is the single Git→SVN path filter: allow prefixes, blocked patterns,
/// deletes, and rename endpoint splitting. Call it before checkout, write,
/// `svn add`, or `svn rm`.
pub fn project_git_to_svn_changeset(
    files: Vec<GitToSvnInputChange>,
    allowed: &[String],
    blocked: &[String],
) -> Result<ProjectedGitToSvnChangeset, IncompleteGitRenameError> {
    let mut included = Vec::new();
    let mut excluded = Vec::new();
    for change in files {
        if change.action == "R" {
            let rename_from = change.rename_from.ok_or_else(|| IncompleteGitRenameError {
                path: change.path.clone(),
            })?;
            let (rename_included, rename_excluded) = project_rename_endpoints(
                &rename_from,
                &change.path,
                change.content,
                allowed,
                blocked,
            );
            included.extend(rename_included);
            excluded.extend(rename_excluded);
            continue;
        }
        if path_is_projected(&change.path, allowed, blocked) {
            included.push(ProjectedGitToSvnChange {
                action: change.action,
                path: change.path,
                content: change.content,
            });
        } else {
            excluded.push((change.action, change.path));
        }
    }
    Ok(ProjectedGitToSvnChangeset { included, excluded })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(action: &str, path: &str) -> GitToSvnInputChange {
        GitToSvnInputChange {
            action: action.to_string(),
            path: path.to_string(),
            content: None,
            rename_from: None,
        }
    }

    fn rename(from: &str, to: &str) -> GitToSvnInputChange {
        GitToSvnInputChange {
            action: "R".into(),
            path: to.to_string(),
            content: Some(b"renamed payload".to_vec()),
            rename_from: Some(from.to_string()),
        }
    }

    fn init_git_repo(repo: &std::path::Path) {
        std::fs::create_dir_all(repo).unwrap();
        assert!(std::process::Command::new("git")
            .args(["init"])
            .current_dir(repo)
            .status()
            .unwrap()
            .success());
        std::process::Command::new("git")
            .args(["config", "user.email", "t@example.invalid"])
            .current_dir(repo)
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.name", "t"])
            .current_dir(repo)
            .status()
            .unwrap();
    }

    fn run_hook_on_staged_paths(
        script: &str,
        staged_paths: &[(&str, &str)],
    ) -> std::process::ExitStatus {
        let dir = tempfile::tempdir().unwrap();
        let hook = dir.path().join("pre-commit");
        std::fs::write(&hook, script).unwrap();
        let repo = dir.path().join("repo");
        init_git_repo(&repo);
        for (rel, contents) in staged_paths {
            let path = repo.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&path, contents).unwrap();
            std::process::Command::new("git")
                .args(["add", rel])
                .current_dir(&repo)
                .status()
                .unwrap();
        }
        std::process::Command::new("bash")
            .arg(&hook)
            .current_dir(&repo)
            .status()
            .unwrap()
    }

    #[test]
    fn bash_allowed_prefix_test_matches_component_rules() {
        assert_eq!(
            bash_allowed_prefix_test("$file", "team"),
            "[[ $file == 'team' || $file == 'team'/* ]]"
        );
        assert!(path_matches_prefix("team/foo", "team"));
        assert!(!path_matches_prefix("team-other/foo", "team"));
    }

    #[test]
    fn bash_blocked_suffix_pattern_uses_quoted_literal() {
        assert_eq!(bash_blocked_pattern_case_arm("*[0].txt"), "*'[0].txt'");
    }

    #[test]
    fn pre_commit_hook_uses_component_aware_allowed_prefix_tests() {
        let script = render_pre_commit_hook_script(&["team".into()], &[]);
        assert!(script.contains("$file == 'team'/*"));
        assert!(!script.contains("== \"$prefix\"*"));
    }

    #[test]
    fn pre_commit_hook_allowed_prefix_team_rules_in_bash() {
        let script = render_pre_commit_hook_script(&["team".into()], &[]);
        assert!(
            run_hook_on_staged_paths(&script, &[("team", "x\n")]).success(),
            "exact prefix team must be allowed"
        );
        assert!(
            run_hook_on_staged_paths(&script, &[("team/foo.txt", "x\n")]).success(),
            "team/foo.txt must be allowed"
        );
        assert!(
            !run_hook_on_staged_paths(&script, &[("team-other/foo.txt", "x\n")]).success(),
            "team-other/foo must be rejected"
        );
    }

    #[test]
    fn pre_commit_hook_rejects_metacharacter_prefixes_via_bash() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("evil-marker");
        let prefix = format!("$(touch {})", marker.display());
        let script = render_pre_commit_hook_script(&[prefix], &[]);
        let hook = dir.path().join("pre-commit");
        std::fs::write(&hook, script).unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("safe.txt"), "ok\n").unwrap();
        init_git_repo(&repo);
        std::process::Command::new("git")
            .args(["add", "safe.txt"])
            .current_dir(&repo)
            .status()
            .unwrap();
        let evil = std::process::Command::new("bash")
            .arg(&hook)
            .current_dir(&repo)
            .status()
            .unwrap();
        assert!(!evil.success(), "out-of-prefix file must be rejected");
        assert!(
            !marker.exists(),
            "metacharacters in prefix must not execute"
        );
        let backtick_marker = dir.path().join("backtick-marker");
        let backtick_prefix = format!("`touch {}`", backtick_marker.display());
        let backtick_script = render_pre_commit_hook_script(&[backtick_prefix], &[]);
        assert!(!run_hook_on_staged_paths(&backtick_script, &[("safe.txt", "ok\n")]).success());
        assert!(!backtick_marker.exists());
        let newline_prefix = "team\n$(touch newline-evil)".to_string();
        let newline_script = render_pre_commit_hook_script(&[newline_prefix], &[]);
        let newline_marker = dir.path().join("newline-evil");
        assert!(!run_hook_on_staged_paths(&newline_script, &[("safe.txt", "ok\n")]).success());
        assert!(!newline_marker.exists());
        let apostrophe_script = render_pre_commit_hook_script(&["it's".into()], &[]);
        assert!(
            run_hook_on_staged_paths(&apostrophe_script, &[("it's", "ok\n")]).success(),
            "embedded apostrophe in prefix must match literally"
        );
        assert!(!run_hook_on_staged_paths(&apostrophe_script, &[("its", "ok\n")]).success());
    }

    #[test]
    fn pre_commit_hook_blocked_pattern_matches_descendants_in_bash() {
        let script = render_pre_commit_hook_script(&[], &["secret".into()]);
        let dir = tempfile::tempdir().unwrap();
        let hook = dir.path().join("pre-commit");
        std::fs::write(&hook, script).unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join("secret")).unwrap();
        std::fs::write(repo.join("secret/leak.txt"), "no\n").unwrap();
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(&repo)
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.email", "t@example.invalid"])
            .current_dir(&repo)
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.name", "t"])
            .current_dir(&repo)
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["add", "secret/leak.txt"])
            .current_dir(&repo)
            .status()
            .unwrap();
        let blocked = std::process::Command::new("bash")
            .arg(&hook)
            .current_dir(&repo)
            .status()
            .unwrap();
        assert!(!blocked.success());
    }

    #[test]
    fn pre_commit_hook_blocked_suffix_pattern_literal_in_bash() {
        let script = render_pre_commit_hook_script(&[], &["*[0].txt".into()]);
        assert!(
            !run_hook_on_staged_paths(&script, &[("a[0].txt", "x\n")]).success(),
            "suffix glob must match literally"
        );
        assert!(
            run_hook_on_staged_paths(&script, &[("a0.txt", "x\n")]).success(),
            "non-matching path must be allowed"
        );
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("suffix-evil");
        let pattern = format!("*$(touch {})", marker.display());
        let evil_script = render_pre_commit_hook_script(&[], &[pattern]);
        assert!(
            run_hook_on_staged_paths(&evil_script, &[("safe.txt", "x\n")]).success(),
            "non-matching staged path must not expand blocked suffix pattern"
        );
        assert!(!marker.exists(), "suffix metacharacters must not execute");
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
            project_git_to_svn_changeset(files, &["team".to_string()], &["*.exe".to_string()])
                .unwrap();
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
        let projected = project_git_to_svn_changeset(files, &[], &[]).unwrap();
        assert_eq!(projected.included.len(), 2);
        assert!(projected.excluded.is_empty());
    }

    #[test]
    fn empty_rules_split_rename_to_delete_and_add() {
        let files = vec![rename("old.txt", "new.txt")];
        let projected = project_git_to_svn_changeset(files, &[], &[]).unwrap();
        let included: Vec<_> = projected
            .included
            .iter()
            .map(|c| (c.action.as_str(), c.path.as_str()))
            .collect();
        assert_eq!(included, vec![("D", "old.txt"), ("A", "new.txt")]);
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
    fn rename_allowed_to_blocked_projects_delete_only() {
        let allowed = vec!["team".to_string()];
        let (included, excluded) = project_rename_endpoints(
            "team/old.txt",
            "team-other/new.txt",
            Some(b"must not publish".to_vec()),
            &allowed,
            &[],
        );
        assert_eq!(
            included,
            vec![ProjectedGitToSvnChange {
                action: "D".into(),
                path: "team/old.txt".into(),
                content: None,
            }]
        );
        assert_eq!(excluded.len(), 1);
        assert!(excluded[0].1.contains("team-other/new.txt"));
    }

    #[test]
    fn rename_blocked_to_allowed_projects_add_only() {
        let allowed = vec!["team".to_string()];
        let (included, excluded) = project_rename_endpoints(
            "team-other/old.txt",
            "team/new.txt",
            Some(b"allowed add".to_vec()),
            &allowed,
            &[],
        );
        assert_eq!(
            included,
            vec![ProjectedGitToSvnChange {
                action: "A".into(),
                path: "team/new.txt".into(),
                content: Some(b"allowed add".to_vec()),
            }]
        );
        assert_eq!(excluded.len(), 1);
        assert!(excluded[0].1.contains("team-other/old.txt"));
    }

    #[test]
    fn rename_both_allowed_projects_delete_and_add() {
        let allowed = vec!["team".to_string()];
        let (included, excluded) = project_rename_endpoints(
            "team/a.txt",
            "team/b.txt",
            Some(b"moved".to_vec()),
            &allowed,
            &[],
        );
        assert_eq!(included.len(), 2);
        assert_eq!(included[0].action, "D");
        assert_eq!(included[0].path, "team/a.txt");
        assert_eq!(included[1].action, "A");
        assert_eq!(included[1].path, "team/b.txt");
        assert!(excluded.is_empty());
    }

    #[test]
    fn rename_both_blocked_projects_nothing() {
        let allowed = vec!["team".to_string()];
        let (included, excluded) = project_rename_endpoints(
            "team-other/a.txt",
            "secret/b.txt",
            Some(b"blocked".to_vec()),
            &allowed,
            &["secret/".to_string()],
        );
        assert!(included.is_empty());
        assert_eq!(excluded.len(), 1);
    }

    #[test]
    fn project_changeset_splits_rename_endpoints() {
        let files = vec![
            rename("team/keep.txt", "team-other/leak.txt"),
            file("A", "team/ok.txt"),
        ];
        let projected = project_git_to_svn_changeset(files, &["team".to_string()], &[]).unwrap();
        let included: Vec<_> = projected
            .included
            .iter()
            .map(|c| (c.action.as_str(), c.path.as_str()))
            .collect();
        assert_eq!(included, vec![("D", "team/keep.txt"), ("A", "team/ok.txt")]);
        assert_eq!(projected.excluded.len(), 1);
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

    #[test]
    fn rename_without_source_path_fails_closed() {
        let files = vec![GitToSvnInputChange {
            action: "R".into(),
            path: "new.txt".into(),
            content: Some(b"moved".to_vec()),
            rename_from: None,
        }];
        let err = project_git_to_svn_changeset(files, &[], &[]).unwrap_err();
        assert_eq!(err.path, "new.txt");
    }
}
