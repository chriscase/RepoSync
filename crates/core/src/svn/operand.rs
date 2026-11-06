//! Literal SVN operands. Options and peg revisions are never repository data.

use crate::errors::SvnError;

fn refused(value: &str, detail: &str) -> SvnError {
    SvnError::WorkingCopyError {
        path: crate::errors::redact_vcs_error_detail(value),
        detail: detail.into(),
    }
}

/// Validate a repository-relative name without normalizing away evidence.
/// Backslashes/drive syntax and VCS administrative components are not portable
/// repository names. Spaces, Unicode, hyphens and literal `@` are data. SVN rejects control
/// characters (including tab/newline), so those refuse before staging.
pub fn validate_relative_path(value: &str) -> Result<(), SvnError> {
    if value.is_empty()
        || value.chars().any(char::is_control)
        || value.contains(['\\', ':'])
        || value.split('/').any(|part| {
            part.is_empty()
                || matches!(part, "." | "..")
                || part.eq_ignore_ascii_case(".svn")
                || part.eq_ignore_ascii_case(".git")
        })
    {
        return Err(refused(
            value,
            "expected a confined repository-relative path",
        ));
    }
    Ok(())
}

/// An empty peg suffix disables SVN's interpretation of literal `@` names.
pub fn literal_path(value: &str) -> Result<String, SvnError> {
    validate_relative_path(value)?;
    Ok(format!("{value}@"))
}

/// Decode one URI path component exactly once. Separators may not be encoded
/// into a component: URL containment is established before deriving log paths.
pub(crate) fn decode_url_component(component: &str) -> Result<String, SvnError> {
    let bytes = component.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            let pair = bytes
                .get(at + 1..at + 3)
                .ok_or_else(|| refused("[SVN URL]", "invalid URL escape"))?;
            let hex = |b: u8| (b as char).to_digit(16).map(|v| v as u8);
            let hi = hex(pair[0]).ok_or_else(|| refused("[SVN URL]", "invalid URL escape"))?;
            let lo = hex(pair[1]).ok_or_else(|| refused("[SVN URL]", "invalid URL escape"))?;
            decoded.push(hi * 16 + lo);
            at += 3;
        } else {
            decoded.push(bytes[at]);
            at += 1;
        }
    }
    String::from_utf8(decoded).map_err(|_| refused("[SVN URL]", "URL path must be UTF-8"))
}

/// Validate a configured URL separately from a repository-relative filename.
pub fn checked_url(value: &str) -> Result<reqwest::Url, SvnError> {
    // URL parsers normalize dot segments and discard ASCII tab/newline. Refuse
    // that evidence before parsing rather than silently selecting another node.
    if value.trim() != value || value.chars().any(char::is_control) {
        return Err(refused(
            "[REDACTED SVN URL]",
            "SVN URL must not contain discarded whitespace",
        ));
    }
    let raw_path = value
        .split_once("://")
        .and_then(|(_, tail)| tail.split_once('/'))
        .map(|(_, path)| path)
        .unwrap_or("");
    if raw_path.starts_with('/') || raw_path.contains("//") {
        return Err(refused(
            "[REDACTED SVN URL]",
            "empty SVN URL path component",
        ));
    }
    for component in raw_path
        .trim_end_matches('/')
        .split('/')
        .filter(|c| !c.is_empty())
    {
        let component = decode_url_component(component)?;
        if matches!(component.as_str(), "." | "..")
            || component.contains(['/', '\\'])
            || component.chars().any(char::is_control)
        {
            return Err(refused(
                "[REDACTED SVN URL]",
                "nonliteral or unsupported SVN URL component",
            ));
        }
    }
    let url =
        reqwest::Url::parse(value).map_err(|_| refused("[REDACTED SVN URL]", "invalid SVN URL"))?;
    if !matches!(url.scheme(), "file" | "http" | "https" | "svn" | "svn+ssh")
        || url.query().is_some()
        || url.fragment().is_some()
        || value.contains('\0')
        || url.password().is_some()
        || (!url.username().is_empty() && url.scheme() != "svn+ssh")
    {
        return Err(refused(
            "[REDACTED SVN URL]",
            "unsupported SVN URL authority or suffix",
        ));
    }
    Ok(url)
}

pub fn literal_url(value: &str, revision: Option<i64>) -> Result<String, SvnError> {
    let url = checked_url(value)?;
    match revision {
        Some(rev) if rev >= 0 => Ok(format!("{url}@{rev}")),
        Some(rev) => Err(SvnError::RevisionNotFound(rev)),
        None => Ok(format!("{url}@")),
    }
}

/// Encode each raw component; neither `%` nor `#`/`?` can change URL meaning.
pub fn repository_url(base: &str, path: &str) -> Result<String, SvnError> {
    let mut url = checked_url(base)?;
    if !path.is_empty() {
        validate_relative_path(path)?;
        url.path_segments_mut()
            .map_err(|_| refused(base, "SVN URL has no path"))?
            .pop_if_empty()
            .extend(path.split('/'));
    }
    Ok(url.into())
}

/// Configured branches are raw repository-relative names, not URI suffixes.
/// Empty and `/` select the configured root; one leading slash is the legacy
/// root-relative spelling. Repeated separators and dot components still refuse.
pub fn configured_branch_url(base: &str, branch: &str) -> Result<String, SvnError> {
    repository_url(base, branch.strip_prefix('/').unwrap_or(branch))
}

pub fn local_destination(path: &std::path::Path) -> Result<&str, SvnError> {
    path.to_str().filter(|p| !p.contains('\0')).ok_or_else(|| {
        refused(
            &path.display().to_string(),
            "SVN destination must be lossless UTF-8 without NUL",
        )
    })
}

/// Compatibility entry for the small set of checked internal WC commands.
/// Rebuild options before -- and encode only the verb's actual target roles.
/// Property names/values and revision ranges are never guessed to be paths.
pub fn working_copy_arguments(
    root: &std::path::Path,
    args: &[&str],
) -> Result<Vec<String>, SvnError> {
    let (verb, rest) = args
        .split_first()
        .ok_or_else(|| refused("", "missing SVN verb"))?;
    if !matches!(
        *verb,
        "info" | "status" | "proplist" | "propset" | "propdel" | "merge" | "add" | "rm" | "revert"
    ) {
        return Err(refused(verb, "unsupported internal working-copy command"));
    }
    let mut result = vec![verb.to_string()];
    let mut at = 0;
    while at < rest.len() {
        let token = rest[at];
        if token == "--" {
            at += 1;
            break;
        }
        if matches!(
            token,
            "--xml"
                | "--verbose"
                | "--recursive"
                | "--show-inherited-props"
                | "--no-ignore"
                | "--force"
                | "--parents"
                | "--dry-run"
                | "--record-only"
                | "--ignore-ancestry"
                | "-R"
        ) {
            result.push(token.into());
            at += 1;
        } else if matches!(token, "--depth" | "-r" | "--revision" | "--accept" | "-c") {
            let value = rest
                .get(at + 1)
                .ok_or_else(|| refused(token, "missing SVN option value"))?;
            result.extend([token.into(), (*value).into()]);
            at += 2;
        } else if token.starts_with("--config")
            || matches!(
                token,
                "--username"
                    | "--password"
                    | "--targets"
                    | "--editor-cmd"
                    | "--diff-cmd"
                    | "--force-interactive"
            )
        {
            return Err(refused(
                token,
                "internal command cannot override client authority or helpers",
            ));
        } else {
            break;
        }
    }
    result.push("--".into());
    let scalars = match *verb {
        "propset" => 2,
        "propdel" => 1,
        _ => 0,
    };
    let targets = &rest[at..];
    if targets.len() < scalars {
        return Err(refused(verb, "missing property fields"));
    }
    for (index, value) in targets.iter().enumerate() {
        if index < scalars {
            result.push((*value).into());
            continue;
        }
        if *verb == "merge" && index == 0 {
            // This source is an explicitly pinned URL from the lineage caller.
            let (url, revision) = value
                .rsplit_once('@')
                .ok_or_else(|| refused(value, "merge requires a pinned source"))?;
            let revision = revision
                .parse()
                .map_err(|_| refused(value, "invalid merge peg"))?;
            result.push(literal_url(url, Some(revision))?);
        } else if *verb == "merge" {
            if index != 1 {
                return Err(refused(
                    value,
                    "merge requires one working-copy destination",
                ));
            }
            if *value != "." {
                let path = std::path::Path::new(value);
                let relative = if path.is_absolute() {
                    path.strip_prefix(root)
                        .map_err(|_| refused(value, "merge destination escapes root"))?
                } else {
                    path
                };
                if !relative.as_os_str().is_empty() {
                    validate_relative_path(local_destination(relative)?)?;
                }
            }
            result.push((*value).into());
        } else if reqwest::Url::parse(value).is_ok() {
            result.push(literal_url(value, None)?);
        } else if *value == "." {
            result.push(".@".into());
        } else if std::path::Path::new(value).is_absolute() {
            let relative = std::path::Path::new(value)
                .strip_prefix(root)
                .map_err(|_| refused(value, "working-copy operand escapes its root"))?;
            if !relative.as_os_str().is_empty() {
                validate_relative_path(local_destination(relative)?)?;
            }
            result.push(format!("{value}@"));
        } else {
            result.push(literal_path(value)?);
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn literal_names_and_url_components_are_not_options_or_pegs() {
        for name in [
            "ordinary",
            "-x",
            "--config-dir=outside",
            "a@1",
            "a b",
            "雪/é",
        ] {
            assert_eq!(literal_path(name).unwrap(), format!("{name}@"));
            let joined = repository_url("https://example.invalid/svn/trunk", name).unwrap();
            assert!(joined.starts_with("https://example.invalid/svn/trunk/"));
            assert!(literal_url(&joined, Some(9)).unwrap().ends_with("@9"));
        }
        assert_eq!(
            repository_url("file:///repo", "a%2fb#c?d").unwrap(),
            "file:///repo/a%252fb%23c%3Fd"
        );
        for path in [
            "",
            "/escape",
            "a/../b",
            "a//b",
            "a/./b",
            "C:x",
            "a\\b",
            ".svn/entries",
            "a/.GIT/config",
            "a\0b",
            "a\tb",
            "a\nb",
        ] {
            assert!(literal_path(path).is_err(), "{path:?}");
        }
    }
}
