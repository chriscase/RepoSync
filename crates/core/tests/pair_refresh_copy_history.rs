use reposync_core::svn::parse_svn_copy_history;

const ORIGIN: &str = r#"<logentry revision="2"><paths><path action="A" copyfrom-path="/trunk" copyfrom-rev="1">/branches/child</path></paths><msg>copy</msg></logentry>"#;

#[test]
fn malformed_delete_cannot_disappear_before_origin() {
    let xml = format!(
        r#"<log><logentry revision="5"><paths><path action="D">/branches/child</paths></logentry>{ORIGIN}</log>"#
    );
    assert!(parse_svn_copy_history(&xml).is_err());
}

#[test]
fn missing_paths_closure_is_refused() {
    let xml = format!(
        r#"<log><logentry revision="5"><paths><path action="R">/branches/child</path></logentry>{ORIGIN}</log>"#
    );
    assert!(parse_svn_copy_history(&xml).is_err());
}

#[test]
fn empty_or_missing_verbose_paths_are_refused() {
    for change in ["", "<paths/>", "<paths></paths>"] {
        let xml = format!(r#"<log><logentry revision="5">{change}</logentry>{ORIGIN}</log>"#);
        assert!(parse_svn_copy_history(&xml).is_err());
    }
}

#[test]
fn duplicate_or_invalid_revision_cannot_be_skipped() {
    for change in [
        r#"<logentry revision="bad"><paths><path action="D">/branches/child</path></paths></logentry>"#,
        r#"<logentry revision="0"><paths><path action="D">/branches/child</path></paths></logentry>"#,
        r#"<logentry revision="5" revision="4"><paths><path action="D">/branches/child</path></paths></logentry>"#,
    ] {
        assert!(parse_svn_copy_history(&format!("<log>{change}{ORIGIN}</log>")).is_err());
    }
}

#[test]
fn dropped_unknown_or_nested_path_is_refused() {
    for path in [
        r#"<path>/branches/child</path>"#,
        r#"<path action="unknown">/branches/child</path>"#,
        r#"<path action="D">/branches/<nested/>child</path>"#,
        r#"<unknown action="D">/branches/child</unknown>"#,
        r#"<path action="D"></path>"#,
        r#"<path action="A" copyfrom-path="/trunk">/branches/child</path>"#,
    ] {
        let xml = format!(
            r#"<log><logentry revision="5"><paths>{path}</paths></logentry>{ORIGIN}</log>"#
        );
        assert!(parse_svn_copy_history(&xml).is_err());
    }
}

#[test]
fn every_well_formed_path_is_retained_and_entities_are_decoded() {
    let xml = format!(
        r#"<log><logentry revision="5"><paths><path action="D">/branches/child</path><path action="M">/other&amp;name</path></paths><msg>marker&lt;value&gt;<![CDATA[ suffix]]></msg></logentry>{ORIGIN}</log>"#
    );
    let entries = parse_svn_copy_history(&xml).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].changed_paths.len(), 2);
    assert_eq!(entries[0].changed_paths[0].action, "D");
    assert_eq!(entries[0].changed_paths[1].path, "/other&name");
    assert_eq!(entries[0].message, "marker<value> suffix");
    assert_eq!(entries[1].changed_paths[0].copy_from_rev, Some(1));
}

#[test]
fn external_entities_and_non_log_documents_are_refused() {
    for xml in [
        r#"<!DOCTYPE log [<!ENTITY x SYSTEM "file:///etc/passwd">]><log>&x;</log>"#,
        "<not-log/>",
        "<log><logentry revision=\"5\"><paths/></logentry></log><log/>",
    ] {
        assert!(parse_svn_copy_history(xml).is_err());
    }
}

#[test]
fn xml_comments_cannot_hide_a_delete_or_change_marker_text() {
    let xml = format!(
        r#"<log><logentry revision="5"><paths><path action="D">/branches/<!--ignored-->child</path></paths><msg>marker<!--ignored--> value</msg></logentry>{ORIGIN}</log>"#
    );
    let entries = parse_svn_copy_history(&xml).unwrap();
    assert_eq!(entries[0].changed_paths[0].action, "D");
    assert_eq!(entries[0].changed_paths[0].path, "/branches/child");
    assert_eq!(entries[0].message, "marker value");
}

#[tokio::test]
async fn logical_lineage_refusal_is_permanent_and_distinct_from_xml_failure() {
    use reposync_core::errors::SvnError;
    use reposync_core::pair_refresh::lineage::{inspect_svn_copy_origin, SvnLineagePins};
    use reposync_core::svn::SvnClient;

    let client = SvnClient::new("http://127.0.0.1:1/unreachable", "", "");
    let pins = SvnLineagePins {
        repository_uuid: String::new(),
        child_path: "/branches/child".into(),
        parent_path: "/trunk".into(),
        child_revision: 2,
        child_mapping_revision: 2,
        parent_baseline_revision: 1,
    };
    let refused = inspect_svn_copy_origin(&client, &pins).await.unwrap_err();
    assert!(refused.is_permanent());
    assert!(
        matches!(refused, SvnError::LineageRefused { reason, .. } if reason == "unproved_pair_refresh_svn_lineage")
    );
    assert!(matches!(
        parse_svn_copy_history("<log>"),
        Err(SvnError::XmlParseError(_))
    ));
}

#[test]
fn namespaced_attribute_twins_cannot_hide_action_source_or_revision() {
    for change in [
        r#"<logentry revision="5"><paths><path xmlns:x="urn:x" x:action="M" action="D">/branches/child</path></paths></logentry>"#,
        r#"<logentry xmlns:x="urn:x" x:revision="2" revision="5"><paths><path action="D">/branches/child</path></paths></logentry>"#,
        r#"<logentry revision="2"><paths><path xmlns:x="urn:x" action="A" x:copyfrom-path="/trunk" copyfrom-path="/elsewhere" x:copyfrom-rev="1" copyfrom-rev="9">/branches/child</path></paths></logentry>"#,
    ] {
        assert!(parse_svn_copy_history(&format!("<log>{change}{ORIGIN}</log>")).is_err());
    }
}

#[test]
fn alternate_destructive_path_spellings_cannot_be_ignored() {
    for path in [
        "/branches/child/../child",
        "/branches/./child",
        "/branches/child/",
        "/branches//child",
        " /branches/child",
    ] {
        let xml = format!(
            r#"<log><logentry revision="5"><paths><path action="D">{path}</path></paths></logentry>{ORIGIN}</log>"#
        );
        assert!(parse_svn_copy_history(&xml).is_err());
    }
}

#[test]
fn unknown_or_namespaced_metadata_attributes_are_refused() {
    for xml in [
        format!(r#"<log unexpected="value">{ORIGIN}</log>"#),
        format!(
            r#"<log><logentry revision="5" unexpected="value"><paths><path action="D">/branches/child</path></paths></logentry>{ORIGIN}</log>"#
        ),
        format!(
            r#"<log><logentry revision="5"><paths><path xmlns:x="urn:x" action="D" x:kind="dir">/branches/child</path></paths></logentry>{ORIGIN}</log>"#
        ),
    ] {
        assert!(parse_svn_copy_history(&xml).is_err());
    }
}

#[test]
fn invalid_copy_source_path_or_revision_is_refused() {
    for attributes in [
        r#"copyfrom-path="/trunk/../trunk" copyfrom-rev="1""#,
        r#"copyfrom-path="/trunk/" copyfrom-rev="1""#,
        r#"copyfrom-path="relative" copyfrom-rev="1""#,
        r#"copyfrom-path="/trunk" copyfrom-rev="-1""#,
    ] {
        let xml = format!(
            r#"<log><logentry revision="2"><paths><path action="A" {attributes}>/branches/child</path></paths></logentry></log>"#
        );
        assert!(parse_svn_copy_history(&xml).is_err());
    }
}

#[test]
fn ordinary_svn_path_metadata_and_repository_root_are_retained() {
    let xml = r#"<log><logentry revision="5"><paths><path action="M" kind="dir" text-mods="false" prop-mods="true">/</path><path action="D" kind="file" text-mods="true" prop-mods="false">/branches/child/file.txt</path></paths></logentry></log>"#;
    let entries = parse_svn_copy_history(xml).unwrap();
    assert_eq!(entries[0].changed_paths.len(), 2);
    assert_eq!(entries[0].changed_paths[0].path, "/");
    assert_eq!(entries[0].changed_paths[1].action, "D");
}

const CAPTURED_COPY_HISTORY_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<log>
<logentry
   revision="3">
<author></author>
<date>2026-11-08T01:29:55.135470Z</date>
<paths>
<path
   action="A"
   prop-mods="false"
   text-mods="true"
   kind="file">/ space 雪 </path>
<path
   text-mods="true"
   kind="file"
   action="A"
   prop-mods="false">/--username</path>
<path
   text-mods="true"
   kind="file"
   action="A"
   prop-mods="false">/-dash</path>
<path
   text-mods="false"
   kind="dir"
   action="A"
   prop-mods="false">/nested</path>
<path
   text-mods="true"
   kind="file"
   action="A"
   prop-mods="false">/nested/深@1</path>
<path
   text-mods="true"
   kind="file"
   action="A"
   prop-mods="false">/ordinary.txt</path>
<path
   text-mods="true"
   kind="file"
   action="A"
   prop-mods="false">/space name</path>
<path
   text-mods="true"
   kind="file"
   action="A"
   prop-mods="false">/x@HEAD</path>
<path
   prop-mods="false"
   text-mods="true"
   kind="file"
   action="A">/xml&amp;&lt;name&gt;</path>
</paths>
<msg>#113 literal names


[reposync] synced from Git 2d8e08d7
RepoSync-Operation: cc66c526-ecce-413b-abc8-d2687f7aa2dc
RepoSync-Git-SHA: 2d8e08d7f4a38326a7de47f27c2e1e710b5cc552</msg>
</logentry>
<logentry
   revision="2">
<author>fixture</author>
<date>2026-11-08T01:29:54.210103Z</date>
<paths>
<path
   action="A"
   prop-mods="false"
   text-mods="true"
   kind="file">/origin.txt</path>
</paths>
<msg>Verified SVN origin</msg>
</logentry>
<logentry
   revision="1">
<author>fixture</author>
<date>2026-11-08T01:29:54.111835Z</date>
<paths>
<path
   action="A"
   prop-mods="false"
   text-mods="true"
   kind="file">/.gitkeep</path>
</paths>
<msg>Initial SVN anchor</msg>
</logentry>
</log>
"#;

fn copy_history_fields(
    change: &reposync_core::svn::SvnChangedPath,
) -> (&str, &str, Option<&str>, Option<i64>) {
    (
        change.action.as_str(),
        change.path.as_str(),
        change.copy_from_path.as_deref(),
        change.copy_from_rev,
    )
}

#[test]
fn captured_literal_copy_history_xml_retains_component_edge_spaces() {
    assert_eq!(CAPTURED_COPY_HISTORY_XML.len(), 1701);
    assert_eq!(
        &CAPTURED_COPY_HISTORY_XML.as_bytes()[215..227],
        b"\x2f\x20\x73\x70\x61\x63\x65\x20\xe9\x9b\xaa\x20"
    );
    let entries = parse_svn_copy_history(CAPTURED_COPY_HISTORY_XML).unwrap();
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.revision)
            .collect::<Vec<_>>(),
        vec![3, 2, 1]
    );
    assert_eq!(entries[0].author, "");
    assert_eq!(entries[0].date, "2026-11-08T01:29:55.135470Z");
    assert_eq!(
        entries[0].message,
        "#113 literal names\n\n\n[reposync] synced from Git 2d8e08d7\nRepoSync-Operation: cc66c526-ecce-413b-abc8-d2687f7aa2dc\nRepoSync-Git-SHA: 2d8e08d7f4a38326a7de47f27c2e1e710b5cc552"
    );
    let r3: Vec<_> = entries[0]
        .changed_paths
        .iter()
        .map(copy_history_fields)
        .collect();
    assert_eq!(
        r3,
        vec![
            ("A", "/ space 雪 ", None, None),
            ("A", "/--username", None, None),
            ("A", "/-dash", None, None),
            ("A", "/nested", None, None),
            ("A", "/nested/深@1", None, None),
            ("A", "/ordinary.txt", None, None),
            ("A", "/space name", None, None),
            ("A", "/x@HEAD", None, None),
            ("A", "/xml&<name>", None, None),
        ]
    );
    assert_eq!(
        r3[0].1.as_bytes(),
        b"\x2f\x20\x73\x70\x61\x63\x65\x20\xe9\x9b\xaa\x20"
    );
    assert_eq!(entries[0].changed_paths.len(), 9);
    assert_eq!(
        copy_history_fields(&entries[1].changed_paths[0]),
        ("A", "/origin.txt", None, None)
    );
    assert_eq!(entries[1].author, "fixture");
    assert_eq!(entries[1].date, "2026-11-08T01:29:54.210103Z");
    assert_eq!(entries[1].message, "Verified SVN origin");
    assert_eq!(
        copy_history_fields(&entries[2].changed_paths[0]),
        ("A", "/.gitkeep", None, None)
    );
    assert_eq!(entries[2].author, "fixture");
    assert_eq!(entries[2].date, "2026-11-08T01:29:54.111835Z");
    assert_eq!(entries[2].message, "Initial SVN anchor");
    assert_eq!(
        entries[1].changed_paths.len() + entries[2].changed_paths.len(),
        2
    );
}

#[test]
fn destructive_literal_space_paths_are_retained_distinct_from_unspaced() {
    let xml = concat!(
        r#"<log><logentry revision="6"><paths>"#,
        r#"<path action="D">/branches/child </path>"#,
        r#"<path action="D">/branches/child</path>"#,
        r#"<path action="R" copyfrom-path="/elsewhere" copyfrom-rev="3">/branches/child </path>"#,
        r#"<path action="D">/branches/child/file.txt</path>"#,
        r#"<path action="D">/</path>"#,
        r#"<path action="D">/ child</path>"#,
        r#"<path action="D">/nested/深@1 </path>"#,
        r#"</paths></logentry></log>"#,
    );
    let entries = parse_svn_copy_history(xml).unwrap();
    let paths: Vec<_> = entries[0]
        .changed_paths
        .iter()
        .map(copy_history_fields)
        .collect();
    assert_eq!(
        paths,
        vec![
            ("D", "/branches/child ", None, None),
            ("D", "/branches/child", None, None),
            ("R", "/branches/child ", Some("/elsewhere"), Some(3),),
            ("D", "/branches/child/file.txt", None, None),
            ("D", "/", None, None),
            ("D", "/ child", None, None),
            ("D", "/nested/深@1 ", None, None),
        ]
    );
    assert_ne!(paths[0].1, paths[1].1);
    assert_eq!(paths[0].1.as_bytes().last().copied(), Some(b' '));
    assert_ne!(paths[1].1.as_bytes().last().copied(), Some(b' '));
}

#[test]
fn literal_space_copyfrom_is_retained_distinctly_from_unspaced() {
    let xml = concat!(
        r#"<log><logentry revision="9"><paths>"#,
        r#"<path action="A" copyfrom-path="/branches/old " copyfrom-rev="8">/branches/child </path>"#,
        r#"<path action="A" copyfrom-path="/branches/old" copyfrom-rev="7">/branches/child</path>"#,
        r#"</paths></logentry></log>"#,
    );
    let entries = parse_svn_copy_history(xml).unwrap();
    let paths: Vec<_> = entries[0]
        .changed_paths
        .iter()
        .map(copy_history_fields)
        .collect();
    assert_eq!(
        paths,
        vec![
            ("A", "/branches/child ", Some("/branches/old "), Some(8),),
            ("A", "/branches/child", Some("/branches/old"), Some(7),),
        ]
    );
    assert_ne!(paths[0].2, paths[1].2);
    assert_ne!(paths[0].3, paths[1].3);
}

#[test]
fn malformed_copyfrom_and_changed_paths_remain_refused() {
    for attributes in [
        r#"copyfrom-path="/trunk/../trunk" copyfrom-rev="1""#,
        r#"copyfrom-path="/trunk/" copyfrom-rev="1""#,
        r#"copyfrom-path="relative" copyfrom-rev="1""#,
        r#"copyfrom-path="/trunk" copyfrom-rev="-1""#,
        r#"copyfrom-path=" /trunk" copyfrom-rev="1""#,
        r#"copyfrom-path="/trunk//x" copyfrom-rev="1""#,
        r#"copyfrom-path="/trunk/." copyfrom-rev="1""#,
        r#"copyfrom-path="/trunk/.." copyfrom-rev="1""#,
    ] {
        let xml = format!(
            r#"<log><logentry revision="2"><paths><path action="A" {attributes}>/branches/child</path></paths></logentry></log>"#
        );
        assert!(parse_svn_copy_history(&xml).is_err(), "{attributes}");
    }
    for path in [
        " /branches/child",
        "branches/child",
        "/branches/child/../child",
        "/branches/./child",
        "/branches/child/",
        "/branches//child",
        "/branches/child\u{0009}",
        "",
    ] {
        let xml = format!(
            r#"<log><logentry revision="5"><paths><path action="D">{path}</path></paths></logentry>{ORIGIN}</log>"#
        );
        assert!(parse_svn_copy_history(&xml).is_err(), "{path:?}");
    }
}
