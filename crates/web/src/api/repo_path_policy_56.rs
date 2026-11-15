//! #56 repository path policy: typed read model and supplied-field validation.
//!
//! Storage is unchanged: `repositories.allowed_paths` and
//! `repositories.blocked_patterns` stay nullable JSON text. NULL and `[]` both
//! mean "no rule of this kind" (see the scheduler). A stored value that is not
//! a JSON array of strings is reported as `malformed`; it is never presented as
//! an empty list, so a client cannot turn unreadable data into a clear.
//!
//! Matching semantics are not defined here. The engine's
//! `reposync_core::path_projection` remains the only implementation; this
//! module only rejects supplied entries that could never match or are not a
//! JSON array of strings, before any write.

use reposync_core::path_projection::normalize_policy_path;
use serde::Serialize;

/// Supplied lists longer than this are refused.
pub(crate) const MAX_ENTRIES: usize = 256;
/// Supplied entries longer than this many UTF-8 bytes are refused.
pub(crate) const MAX_ENTRY_BYTES: usize = 1024;
/// Version of the `path_policy` read object.
pub(crate) const READ_VERSION: u32 = 1;

const MALFORMED_REASON: &str =
    "stored value is not a JSON array of strings; the scheduler refuses to \
     sync this repository until the value is replaced or explicitly cleared";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PolicyField {
    AllowedPaths,
    BlockedPatterns,
}

impl PolicyField {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::AllowedPaths => "allowed_paths",
            Self::BlockedPatterns => "blocked_patterns",
        }
    }
}

/// One stored policy column as read. `List { entries: [] }` is a stored `[]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum PolicyFieldRead {
    Unset,
    List {
        entries: Vec<String>,
    },
    Malformed {
        stored_bytes: usize,
        reason: &'static str,
    },
}

/// `path_policy` in repository read responses. The raw columns stay
/// write-only fields of `PUT /api/repos/:id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct RepoPathPolicyRead {
    pub version: u32,
    pub allowed_paths: PolicyFieldRead,
    pub blocked_patterns: PolicyFieldRead,
    /// The only direction this build filters with these rules.
    pub enforced_direction: &'static str,
    /// SVN→Git path filtering (#53) is not part of this build.
    pub svn_to_git_filtering: &'static str,
    /// Included paths keep their branch-relative path; rewriting is not editable.
    pub path_rewriting: &'static str,
}

pub(crate) fn read_field(stored: Option<&str>) -> PolicyFieldRead {
    match stored {
        None => PolicyFieldRead::Unset,
        // Same parse as the scheduler, so read and enforcement agree.
        Some(raw) => match serde_json::from_str::<Vec<String>>(raw) {
            Ok(entries) => PolicyFieldRead::List { entries },
            Err(_) => PolicyFieldRead::Malformed {
                stored_bytes: raw.len(),
                reason: MALFORMED_REASON,
            },
        },
    }
}

pub(crate) fn read_policy(allowed: Option<&str>, blocked: Option<&str>) -> RepoPathPolicyRead {
    RepoPathPolicyRead {
        version: READ_VERSION,
        allowed_paths: read_field(allowed),
        blocked_patterns: read_field(blocked),
        enforced_direction: "git_to_svn",
        svn_to_git_filtering: "not_applied",
        path_rewriting: "none_read_only",
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PolicyValidationError {
    pub field: PolicyField,
    pub index: Option<usize>,
    pub code: &'static str,
    pub message: &'static str,
}

impl std::fmt::Display for PolicyValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.index {
            Some(index) => write!(
                f,
                "{}[{}] {}: {}; nothing was saved",
                self.field.name(),
                index,
                self.code,
                self.message
            ),
            None => write!(
                f,
                "{} {}: {}; nothing was saved",
                self.field.name(),
                self.code,
                self.message
            ),
        }
    }
}

/// One entry. The same rules, codes and order are mirrored by
/// `web-ui/src/pathPolicy56.ts` for inline validation.
pub(crate) fn validate_entry(
    field: PolicyField,
    entry: &str,
) -> Result<(), (&'static str, &'static str)> {
    if entry.is_empty() {
        return Err(("empty_entry", "must not be empty"));
    }
    if entry.len() > MAX_ENTRY_BYTES {
        return Err(("entry_too_long", "must be at most 1024 UTF-8 bytes"));
    }
    if entry.chars().any(char::is_control) {
        return Err(("control_character", "must not contain control characters"));
    }
    if entry.starts_with(' ') || entry.ends_with(' ') {
        return Err(("surrounding_space", "must not start or end with a space"));
    }
    let normalized = normalize_policy_path(entry);
    let body = normalized.trim_end_matches('/');
    if body.is_empty() {
        return Err((
            "names_no_path",
            "must name a path; '/' alone matches nothing",
        ));
    }
    for component in body.split('/') {
        if component.is_empty() {
            return Err((
                "empty_component",
                "must not contain an empty path component ('//')",
            ));
        }
        if component == "." || component == ".." {
            return Err((
                "dot_component",
                "must not contain '.' or '..' path components",
            ));
        }
    }
    if field == PolicyField::AllowedPaths && body.contains('*') {
        return Err((
            "wildcard_in_allowed",
            "allowed paths are literal component prefixes; '*' is not a wildcard there",
        ));
    }
    Ok(())
}

/// A supplied field: absent keeps the stored value, `null` clears it, and a
/// string must be a valid JSON array of valid entries.
pub(crate) fn validate_supplied(
    field: PolicyField,
    supplied: &Option<Option<String>>,
) -> Result<(), PolicyValidationError> {
    let Some(Some(raw)) = supplied else {
        return Ok(());
    };
    let error = |index, code, message| PolicyValidationError {
        field,
        index,
        code,
        message,
    };
    let entries: Vec<String> = serde_json::from_str(raw).map_err(|_| {
        error(
            None,
            "not_array",
            "must be a JSON array of strings; send null to clear explicitly",
        )
    })?;
    if entries.len() > MAX_ENTRIES {
        return Err(error(
            None,
            "too_many_entries",
            "must contain at most 256 entries",
        ));
    }
    for (index, entry) in entries.iter().enumerate() {
        validate_entry(field, entry)
            .map_err(|(code, message)| error(Some(index), code, message))?;
    }
    Ok(())
}

/// Checked after durable receipt recovery and before any CAS or repository
/// write: an exact replay of an already-accepted request keeps its receipt,
/// while a novel request with an invalid field is refused unwritten.
pub(crate) fn validate_update(
    allowed: &Option<Option<String>>,
    blocked: &Option<Option<String>>,
) -> Result<(), PolicyValidationError> {
    validate_supplied(PolicyField::AllowedPaths, allowed)?;
    validate_supplied(PolicyField::BlockedPatterns, blocked)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALLOWED: PolicyField = PolicyField::AllowedPaths;
    const BLOCKED: PolicyField = PolicyField::BlockedPatterns;

    #[test]
    fn rs56_entry_rule_table_matches_the_ui_table() {
        // Kept identical to ENTRY_RULES in web-ui/src/pathPolicy56.test.ts.
        let long = "a".repeat(MAX_ENTRY_BYTES + 1);
        let cases: Vec<(PolicyField, &str, Option<&str>)> = vec![
            (ALLOWED, "source/", None),
            (ALLOWED, "/source", None),
            (ALLOWED, "team\\docs", None),
            (ALLOWED, "", Some("empty_entry")),
            (ALLOWED, long.as_str(), Some("entry_too_long")),
            (ALLOWED, "a\tb", Some("control_character")),
            (ALLOWED, " source", Some("surrounding_space")),
            (ALLOWED, "source ", Some("surrounding_space")),
            (ALLOWED, "/", Some("names_no_path")),
            (ALLOWED, "a//b", Some("empty_component")),
            (ALLOWED, "a/../b", Some("dot_component")),
            (ALLOWED, "./a", Some("dot_component")),
            (ALLOWED, "src/*", Some("wildcard_in_allowed")),
            (BLOCKED, "*.exe", None),
            (BLOCKED, "secret-*", None),
            (BLOCKED, "*", None),
            (BLOCKED, "temp/", None),
            (BLOCKED, "\\", Some("names_no_path")),
            (BLOCKED, "x/./y", Some("dot_component")),
        ];
        for (field, entry, expected) in cases {
            assert_eq!(
                validate_entry(field, entry).err().map(|(code, _)| code),
                expected,
                "{field:?} {entry:?}"
            );
        }
    }

    #[test]
    fn rs56_supplied_absent_null_and_string_forms() {
        assert!(validate_supplied(ALLOWED, &None).is_ok());
        assert!(validate_supplied(ALLOWED, &Some(None)).is_ok());
        assert!(validate_supplied(ALLOWED, &Some(Some("[]".into()))).is_ok());
        for raw in ["", "null", "{}", "\"a\"", "[1]", "[\"a\"] x"] {
            let err = validate_supplied(ALLOWED, &Some(Some(raw.into()))).unwrap_err();
            assert_eq!((err.code, err.index), ("not_array", None), "{raw:?}");
        }
        let many = serde_json::to_string(&vec!["a"; MAX_ENTRIES + 1]).unwrap();
        assert_eq!(
            validate_supplied(BLOCKED, &Some(Some(many)))
                .unwrap_err()
                .code,
            "too_many_entries"
        );
        let err = validate_supplied(BLOCKED, &Some(Some("[\"ok\",\"\"]".into()))).unwrap_err();
        assert_eq!((err.code, err.index), ("empty_entry", Some(1)));
        assert_eq!(
            err.to_string(),
            "blocked_patterns[1] empty_entry: must not be empty; nothing was saved"
        );
    }

    #[test]
    fn rs56_read_distinguishes_unset_empty_list_and_malformed() {
        assert_eq!(read_field(None), PolicyFieldRead::Unset);
        assert_eq!(
            read_field(Some("[]")),
            PolicyFieldRead::List { entries: vec![] }
        );
        assert_eq!(
            read_field(Some("[\"a\",\"b\"]")),
            PolicyFieldRead::List {
                entries: vec!["a".into(), "b".into()]
            }
        );
        assert!(matches!(
            read_field(Some("source/")),
            PolicyFieldRead::Malformed {
                stored_bytes: 7,
                ..
            }
        ));
        let json = serde_json::to_value(read_policy(Some("not json"), None)).unwrap();
        assert_eq!(json["allowed_paths"]["state"], "malformed");
        assert!(json["allowed_paths"].get("entries").is_none());
        assert_eq!(
            json["blocked_patterns"],
            serde_json::json!({"state": "unset"})
        );
        assert_eq!(json["svn_to_git_filtering"], "not_applied");
    }
}
