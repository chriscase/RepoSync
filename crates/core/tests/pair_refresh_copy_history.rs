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
        "/branches/child ",
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
