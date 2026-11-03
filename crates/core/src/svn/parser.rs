//! Parsers for SVN XML output.

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::errors::SvnError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SvnInfo {
    pub root_url: String,
    pub uuid: String,
    pub latest_rev: i64,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SvnLogEntry {
    pub revision: i64,
    pub author: String,
    pub date: String,
    pub message: String,
    pub changed_paths: Vec<SvnChangedPath>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SvnChangedPath {
    pub action: String,
    pub path: String,
    pub copy_from_path: Option<String>,
    pub copy_from_rev: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SvnDiffEntry {
    pub kind: String,
    pub props_changed: bool,
    pub path: String,
    pub item: String,
}

pub fn parse_svn_info(xml: &str) -> Result<SvnInfo, SvnError> {
    debug!("parsing svn info XML ({} bytes)", xml.len());
    let url = extract_tag_content(xml, "url")
        .ok_or_else(|| SvnError::XmlParseError("missing <url> in svn info".into()))?;
    let root_url = extract_tag_content(xml, "root")
        .ok_or_else(|| SvnError::XmlParseError("missing <root> in svn info".into()))?;
    let uuid = extract_tag_content(xml, "uuid")
        .ok_or_else(|| SvnError::XmlParseError("missing <uuid> in svn info".into()))?;
    let latest_rev = extract_attribute(xml, "entry", "revision")
        .or_else(|| extract_attribute(xml, "commit", "revision"))
        .and_then(|s| s.parse::<i64>().ok())
        .ok_or_else(|| SvnError::XmlParseError("missing revision in svn info".into()))?;
    Ok(SvnInfo {
        root_url,
        uuid,
        latest_rev,
        url,
    })
}

pub fn parse_svn_log(xml: &str) -> Result<Vec<SvnLogEntry>, SvnError> {
    debug!("parsing svn log XML ({} bytes)", xml.len());
    let mut entries = Vec::new();
    let parts: Vec<&str> = xml.split("<logentry").collect();
    for part in parts.iter().skip(1) {
        let entry_xml = match part.find("</logentry>") {
            Some(pos) => &part[..pos],
            None => part,
        };
        let revision = match extract_attribute_from_fragment(entry_xml, "revision")
            .and_then(|s| s.parse::<i64>().ok())
        {
            Some(rev) => rev,
            None => {
                warn!("skipping SVN log entry with missing or unparseable revision attribute");
                continue;
            }
        };
        let author = extract_tag_content(entry_xml, "author").unwrap_or_default();
        let date = extract_tag_content(entry_xml, "date").unwrap_or_default();
        let message = extract_tag_content(entry_xml, "msg").unwrap_or_default();
        let changed_paths = parse_changed_paths(entry_xml);
        entries.push(SvnLogEntry {
            revision,
            author,
            date,
            message,
            changed_paths,
        });
    }
    debug!(count = entries.len(), "parsed svn log entries");
    Ok(entries)
}

pub fn parse_svn_diff_summarize(xml: &str) -> Result<Vec<SvnDiffEntry>, SvnError> {
    debug!("parsing svn diff --summarize XML ({} bytes)", xml.len());
    let mut entries = Vec::new();
    let parts: Vec<&str> = xml.split("<path ").collect();
    for part in parts.iter().skip(1) {
        let fragment = match part.find("</path>") {
            Some(pos) => &part[..pos],
            None => continue,
        };
        let item = extract_attribute_from_fragment(fragment, "item").unwrap_or_default();
        let kind_attr = extract_attribute_from_fragment(fragment, "kind").unwrap_or_default();
        let props = extract_attribute_from_fragment(fragment, "props").unwrap_or_default();
        let path = match fragment.find('>') {
            Some(pos) => fragment[pos + 1..].trim().to_string(),
            None => String::new(),
        };
        entries.push(SvnDiffEntry {
            kind: item,
            props_changed: props != "none",
            path,
            item: kind_attr,
        });
    }
    debug!(count = entries.len(), "parsed svn diff entries");
    Ok(entries)
}

fn extract_tag_content(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{}", tag);
    let close = format!("</{}>", tag);
    let mut search_from = 0;
    while let Some(rel_pos) = xml[search_from..].find(&open) {
        let start_pos = search_from + rel_pos;
        let after_open = &xml[start_pos + open.len()..];
        // Ensure we matched the tag exactly (next char must be '>' or whitespace for attributes)
        if let Some(ch) = after_open.chars().next() {
            if ch != '>' && !ch.is_ascii_whitespace() {
                // False match (e.g. <urlencoded> when looking for <url>), keep searching
                search_from = start_pos + open.len();
                continue;
            }
        }
        let content_start = match after_open.find('>') {
            Some(pos) => pos + 1,
            None => return None,
        };
        let content = &after_open[content_start..];
        let end_pos = content.find(&close)?;
        return Some(xml_unescape(content[..end_pos].trim()));
    }
    None
}

/// Unescape standard XML entities.
fn xml_unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}

fn extract_attribute(xml: &str, tag: &str, attr: &str) -> Option<String> {
    let open = format!("<{}", tag);
    let start_pos = xml.find(&open)?;
    let after_tag = &xml[start_pos + open.len()..];
    let tag_end = after_tag.find('>')?;
    extract_attr_from_str(&after_tag[..tag_end], attr)
}

fn extract_attribute_from_fragment(fragment: &str, attr: &str) -> Option<String> {
    extract_attr_from_str(fragment, attr)
}

fn extract_attr_from_str(s: &str, attr: &str) -> Option<String> {
    let pattern_dq = format!("{}=\"", attr);
    if let Some(pos) = s.find(&pattern_dq) {
        let after = &s[pos + pattern_dq.len()..];
        let end = after.find('"')?;
        return Some(after[..end].to_string());
    }
    let pattern_sq = format!("{}='", attr);
    if let Some(pos) = s.find(&pattern_sq) {
        let after = &s[pos + pattern_sq.len()..];
        let end = after.find('\'')?;
        return Some(after[..end].to_string());
    }
    None
}

fn parse_changed_paths(entry_xml: &str) -> Vec<SvnChangedPath> {
    let mut paths = Vec::new();
    let paths_block = match entry_xml.find("<paths>") {
        Some(start) => {
            let rest = &entry_xml[start..];
            match rest.find("</paths>") {
                Some(end) => &rest[..end],
                None => return paths,
            }
        }
        None => return paths,
    };
    let parts: Vec<&str> = paths_block.split("<path").collect();
    for part in parts.iter().skip(1) {
        let fragment = match part.find("</path>") {
            Some(pos) => &part[..pos],
            None => continue,
        };
        let action = extract_attribute_from_fragment(fragment, "action").unwrap_or_default();
        let copy_from_path = extract_attribute_from_fragment(fragment, "copyfrom-path");
        let copy_from_rev = extract_attribute_from_fragment(fragment, "copyfrom-rev")
            .and_then(|s| s.parse::<i64>().ok());
        let path = match fragment.find('>') {
            Some(pos) => fragment[pos + 1..].trim().to_string(),
            None => String::new(),
        };
        paths.push(SvnChangedPath {
            action,
            path,
            copy_from_path,
            copy_from_rev,
        });
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_svn_info() {
        let xml = r#"<info><entry kind="dir" path="." revision="1234">
<url>https://svn.example.com/repo/trunk</url>
<repository><root>https://svn.example.com/repo</root>
<uuid>a1b2c3d4</uuid></repository>
<commit revision="1234"></commit></entry></info>"#;
        let info = parse_svn_info(xml).unwrap();
        assert_eq!(info.latest_rev, 1234);
    }

    #[test]
    fn test_parse_svn_log() {
        let xml = r#"<log><logentry revision="100"><author>alice</author><date>2025-01-10</date>
<paths><path action="M" kind="file">/trunk/main.rs</path></paths><msg>fix</msg></logentry></log>"#;
        let entries = parse_svn_log(xml).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].revision, 100);
    }

    #[test]
    fn test_parse_svn_log_multiple_entries() {
        let xml = r#"<log>
<logentry revision="100"><author>alice</author><date>2025-01-10</date>
<paths><path action="M" kind="file">/trunk/main.rs</path></paths><msg>fix A</msg></logentry>
<logentry revision="101"><author>bob</author><date>2025-01-11</date>
<paths><path action="A" kind="file">/trunk/new.rs</path></paths><msg>add new</msg></logentry>
</log>"#;
        let entries = parse_svn_log(xml).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].revision, 100);
        assert_eq!(entries[0].author, "alice");
        assert_eq!(entries[1].revision, 101);
        assert_eq!(entries[1].author, "bob");
    }

    #[test]
    fn test_parse_svn_log_skips_invalid_revision() {
        let xml = r#"<log>
<logentry><author>alice</author><date>2025-01-10</date><msg>no rev</msg></logentry>
<logentry revision="101"><author>bob</author><date>2025-01-11</date><msg>good</msg></logentry>
</log>"#;
        let entries = parse_svn_log(xml).unwrap();
        // Entry without revision should be skipped
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].revision, 101);
    }

    #[test]
    fn test_parse_svn_log_xml_entities() {
        let xml = r#"<log><logentry revision="50"><author>alice</author><date>2025-01-10</date>
<paths><path action="M" kind="file">/trunk/foo &amp; bar.rs</path></paths>
<msg>fix &lt;bug&gt; &amp; improve</msg></logentry></log>"#;
        let entries = parse_svn_log(xml).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].message, "fix <bug> & improve");
    }

    #[test]
    fn test_parse_svn_log_empty() {
        let xml = r#"<log></log>"#;
        let entries = parse_svn_log(xml).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn test_parse_svn_log_missing_author() {
        let xml = r#"<log><logentry revision="99"><date>2025-01-10</date>
<msg>anonymous commit</msg></logentry></log>"#;
        let entries = parse_svn_log(xml).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].author, "");
    }

    #[test]
    fn test_parse_svn_log_copy_from() {
        let xml = r#"<log><logentry revision="200"><author>alice</author><date>2025-01-10</date>
<paths><path action="A" kind="dir" copyfrom-path="/trunk" copyfrom-rev="199">/branches/feature</path></paths>
<msg>branch</msg></logentry></log>"#;
        let entries = parse_svn_log(xml).unwrap();
        assert_eq!(entries[0].changed_paths.len(), 1);
        assert_eq!(
            entries[0].changed_paths[0].copy_from_path.as_deref(),
            Some("/trunk")
        );
        assert_eq!(entries[0].changed_paths[0].copy_from_rev, Some(199));
    }

    #[test]
    fn test_parse_svn_diff_summarize() {
        let xml = r#"<?xml version="1.0"?>
<diff><paths>
<path item="modified" kind="file" props="none">/trunk/src/main.rs</path>
<path item="added" kind="file" props="none">/trunk/src/new.rs</path>
</paths></diff>"#;
        let entries = parse_svn_diff_summarize(xml).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].kind, "modified");
        assert_eq!(entries[0].path, "/trunk/src/main.rs");
        assert_eq!(entries[1].kind, "added");
    }

    #[test]
    fn test_parse_svn_diff_summarize_empty() {
        let xml = r#"<?xml version="1.0"?><diff><paths></paths></diff>"#;
        let entries = parse_svn_diff_summarize(xml).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn test_parse_svn_diff_summarize_props_changed() {
        let xml = r#"<diff><paths>
<path item="none" kind="file" props="modified">/trunk/src/main.rs</path>
</paths></diff>"#;
        let entries = parse_svn_diff_summarize(xml).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].props_changed);
    }

    #[test]
    fn test_xml_unescape() {
        assert_eq!(xml_unescape("foo &amp; bar"), "foo & bar");
        assert_eq!(xml_unescape("a &lt; b &gt; c"), "a < b > c");
        assert_eq!(xml_unescape("&quot;hello&quot;"), "\"hello\"");
        assert_eq!(xml_unescape("it&apos;s"), "it's");
        assert_eq!(xml_unescape("no entities"), "no entities");
    }

    #[test]
    fn test_extract_tag_content_no_prefix_match() {
        // Searching for <url> should NOT match <urlencoded>
        let xml = r#"<urlencoded>wrong</urlencoded><url>right</url>"#;
        let result = extract_tag_content(xml, "url");
        assert_eq!(result, Some("right".to_string()));
    }

    #[test]
    fn test_parse_svn_info_with_entities() {
        let xml = r#"<info><entry kind="dir" path="." revision="5">
<url>https://svn.example.com/repo/trunk</url>
<repository><root>https://svn.example.com/repo</root>
<uuid>a1b2c3d4</uuid></repository>
<commit revision="5"></commit></entry></info>"#;
        let info = parse_svn_info(xml).unwrap();
        assert_eq!(info.url, "https://svn.example.com/repo/trunk");
    }
}
