//! Read-only remote proof and isolated normal SVN merge preparation. Admission,
//! live drift checks, identity, holds and publication remain the durable caller's
//! responsibility. This module never commits a remote revision.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use git2::{Oid, Repository, TreeWalkMode, TreeWalkResult};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::lineage::SvnRefreshLineageProof;
use super::preparation::require_supported_refresh_tree;
use super::publication::require_complete_git_graph;
use crate::errors::SvnError;
use crate::history_inspect::is_full_git_oid;
use crate::svn::SvnClient;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanChildSvnPins {
    pub lineage: SvnRefreshLineageProof,
    pub baseline_git_tree: String,
    pub parent_git_tree: String,
    /// Supplied only from an approved prior completed refresh receipt.
    pub previous_mergeinfo: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreparedCleanChildSvn {
    pub operation_id: Uuid,
    pub pins: CleanChildSvnPins,
    pub working_copy: PathBuf,
    pub mergeinfo: String,
    pub result_git_tree: String,
    pub result_properties: SvnPropertySnapshot,
}

pub type SvnPropertySnapshot = BTreeMap<String, BTreeMap<String, String>>;

async fn visible_checkout_properties(
    client: &SvnClient,
    path: &Path,
    info: &crate::svn::SvnInfo,
) -> Result<SvnPropertySnapshot, SvnError> {
    let canonical = path.canonicalize()?;
    let root = canonical
        .to_str()
        .ok_or_else(|| refused("working directory must be UTF-8"))?;
    let xml = client
        .run_svn_in_dir_public(
            &canonical,
            &[
                "proplist",
                "--xml",
                "--verbose",
                "--recursive",
                "--show-inherited-props",
                root,
            ],
        )
        .await?;
    bound_checkout_property_xml(&xml, root, &canonical, info)
}

fn bound_checkout_property_xml(
    xml: &str,
    root: &str,
    checkout: &Path,
    info: &crate::svn::SvnInfo,
) -> Result<SvnPropertySnapshot, SvnError> {
    let document =
        roxmltree::Document::parse(xml).map_err(|e| SvnError::XmlParseError(e.to_string()))?;
    let element = document.root_element();
    if !named(element, "properties") {
        return Err(refused("invalid local property proof"));
    }
    attributes(element, &[])?;
    let has_root = element
        .children()
        .any(|n| named(n, "target") && attribute(n, "path") == Some(root));
    if has_root {
        return parse_refresh_properties(xml, root);
    }
    // Local proplist legitimately omits an empty root. Add that target only
    // when the attested complete WC31 BASE metadata independently proves it.
    // The general XML parser retains its missing-root refusal oracle.
    if !super::svn_visibility::has_proved_empty_root_properties(checkout, info)? {
        return Err(refused(
            "local property proof omits a nonempty or unknown root",
        ));
    }
    let escaped = root
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\r', "&#13;")
        .replace('\n', "&#10;")
        .replace('\t', "&#9;");
    let children: String = element.children().map(|node| &xml[node.range()]).collect();
    parse_refresh_properties(
        &format!("<properties><target path=\"{escaped}\"/>{children}</properties>"),
        root,
    )
}

fn refused(detail: &str) -> SvnError {
    SvnError::LineageRefused {
        reason: "unproved_pair_refresh_svn_tree".into(),
        detail: detail.into(),
    }
}

fn named(node: roxmltree::Node<'_, '_>, name: &str) -> bool {
    node.is_element() && node.tag_name().namespace().is_none() && node.tag_name().name() == name
}

fn attributes(node: roxmltree::Node<'_, '_>, names: &[&str]) -> Result<(), SvnError> {
    if node
        .attributes()
        .any(|a| a.namespace().is_some() || !names.contains(&a.name()))
    {
        return Err(refused("unsupported property XML attribute"));
    }
    Ok(())
}

fn attribute<'a>(node: roxmltree::Node<'a, '_>, name: &str) -> Option<&'a str> {
    node.attributes()
        .find(|a| a.namespace().is_none() && a.name() == name)
        .map(|a| a.value())
}

fn supported_relative_path(path: &str) -> bool {
    !path.is_empty()
        && path.split('/').all(|part| {
            !matches!(part, "" | "." | ".." | ".svn" | ".git")
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        })
}

/// Strictly retain every target and property. Unsupported encoding, namespace,
/// inherited evidence or ambiguous target spelling is a permanent refusal.
pub fn parse_refresh_properties(xml: &str, root: &str) -> Result<SvnPropertySnapshot, SvnError> {
    let document = roxmltree::Document::parse(xml)
        .map_err(|error| SvnError::XmlParseError(error.to_string()))?;
    let element = document.root_element();
    if !named(element, "properties") || root.is_empty() || root.ends_with('/') {
        return Err(refused("invalid property proof root"));
    }
    attributes(element, &[])?;
    let mut result = BTreeMap::new();
    for target in element.children().filter(|n| n.is_element()) {
        if !named(target, "target") {
            return Err(refused("unsupported property target"));
        }
        attributes(target, &["path"])?;
        let path = attribute(target, "path").ok_or_else(|| refused("missing property path"))?;
        let relative = if path == root {
            ""
        } else {
            path.strip_prefix(root)
                .and_then(|suffix| suffix.strip_prefix('/'))
                .filter(|path| supported_relative_path(path))
                .ok_or_else(|| refused("property target outside pinned tree or unsupported path"))?
        };
        let mut values = BTreeMap::new();
        for property in target.children().filter(|n| n.is_element()) {
            if !named(property, "property") {
                return Err(refused("inherited or unsupported property evidence"));
            }
            attributes(property, &["name"])?;
            let name =
                attribute(property, "name").ok_or_else(|| refused("missing property name"))?;
            if property.children().any(|n| n.is_element()) {
                return Err(refused("nested property value"));
            }
            let value: String = property
                .children()
                .filter(|n| n.is_text())
                .filter_map(|n| n.text())
                .collect();
            if values.insert(name.to_owned(), value).is_some() {
                return Err(refused("duplicate property"));
            }
        }
        if result.insert(relative.to_owned(), values).is_some() {
            return Err(refused("duplicate property target"));
        }
    }
    if !result.contains_key("") {
        return Err(refused("property proof omits tree root"));
    }
    Ok(result)
}

fn merge_ranges(raw: Option<&str>, source: &str) -> Result<Vec<(i64, i64)>, SvnError> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    let ranges = raw
        .strip_prefix(source)
        .and_then(|s| s.strip_prefix(':'))
        .filter(|s| !s.is_empty())
        .ok_or_else(|| refused("unsupported mergeinfo source"))?;
    let revision = |raw: &str| -> Result<i64, SvnError> {
        let value: i64 = raw
            .parse()
            .map_err(|_| refused("invalid mergeinfo revision"))?;
        if value < 1 || value.to_string() != raw {
            return Err(refused("noncanonical mergeinfo revision"));
        }
        Ok(value)
    };
    let mut result = Vec::new();
    for range in ranges.split(',') {
        let (first, last) = match range.split_once('-') {
            Some((first, last)) => (revision(first)?, revision(last)?),
            None => {
                let value = revision(range)?;
                (value, value)
            }
        };
        if first > last || result.last().is_some_and(|(_, prior)| *prior >= first) {
            return Err(refused("unordered or overlapping mergeinfo"));
        }
        result.push((first, last));
    }
    Ok(result)
}

/// Status is separate evidence: a property/tree conflict can keep the expected
/// bytes. Retain every status entry and refuse namespace aliases or hidden nodes.
pub fn require_refresh_merge_status(xml: &str) -> Result<(), SvnError> {
    let document = roxmltree::Document::parse(xml)
        .map_err(|error| SvnError::XmlParseError(error.to_string()))?;
    let root = document.root_element();
    if !named(root, "status") {
        return Err(refused("invalid merge status root"));
    }
    attributes(root, &[])?;
    let targets: Vec<_> = root.children().filter(|n| n.is_element()).collect();
    if targets.len() != 1 || !named(targets[0], "target") {
        return Err(refused("unsupported merge status target"));
    }
    let target = targets[0];
    attributes(target, &["path"])?;
    if attribute(target, "path") != Some(".") {
        return Err(refused("merge status target differs from working copy"));
    }
    let mut paths = BTreeSet::new();
    for entry in target.children().filter(|n| n.is_element()) {
        if !named(entry, "entry") {
            return Err(refused("unsupported merge status entry"));
        }
        attributes(entry, &["path"])?;
        let path = attribute(entry, "path").ok_or_else(|| refused("missing merge status path"))?;
        if (path != "." && !supported_relative_path(path)) || !paths.insert(path) {
            return Err(refused("duplicate or unsupported merge status path"));
        }
        let statuses: Vec<_> = entry.children().filter(|n| n.is_element()).collect();
        if statuses.len() != 1 || !named(statuses[0], "wc-status") {
            return Err(refused("missing or unsupported working copy status"));
        }
        let status = statuses[0];
        attributes(
            status,
            &[
                "item",
                "props",
                "revision",
                "copied",
                "tree-conflicted",
                "switched",
                "wc-locked",
                "file-external",
            ],
        )?;
        if !matches!(
            attribute(status, "item"),
            Some("normal" | "modified" | "added" | "deleted" | "replaced")
        ) || !matches!(
            attribute(status, "props"),
            Some("none" | "normal" | "modified")
        ) || ["tree-conflicted", "switched", "wc-locked", "file-external"]
            .iter()
            .any(|key| attribute(status, key).is_some_and(|value| value != "false"))
            || attribute(status, "copied").is_some_and(|v| !matches!(v, "true" | "false"))
        {
            return Err(refused("SVN merge has unresolved or unsupported status"));
        }
        for commit in status.children().filter(|n| n.is_element()) {
            if !named(commit, "commit") {
                return Err(refused("unsupported merge status evidence"));
            }
            attributes(commit, &["revision"])?;
            for field in commit.children().filter(|n| n.is_element()) {
                if !named(field, "author") && !named(field, "date") {
                    return Err(refused("unsupported status commit field"));
                }
                attributes(field, &[])?;
                if field.children().any(|n| n.is_element()) {
                    return Err(refused("nested status commit field"));
                }
            }
        }
    }
    Ok(())
}

fn mergeinfo_string(source: &str, mut ranges: Vec<(i64, i64)>) -> String {
    ranges.sort_unstable();
    let mut normalized: Vec<(i64, i64)> = Vec::new();
    for (first, last) in ranges {
        match normalized.last_mut() {
            Some((_, previous)) if first <= previous.saturating_add(1) => {
                *previous = (*previous).max(last)
            }
            _ => normalized.push((first, last)),
        }
    }
    let ranges = normalized
        .into_iter()
        .map(|(first, last)| {
            if first == last {
                first.to_string()
            } else {
                format!("{first}-{last}")
            }
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("{source}:{ranges}")
}

/// Newly merged range is (baseline, parent], plus only approved prior evidence.
pub fn expected_refresh_mergeinfo(
    source: &str,
    baseline: i64,
    parent: i64,
    previous: Option<&str>,
) -> Result<String, SvnError> {
    if !source.starts_with('/')
        || !supported_relative_path(&source[1..])
        || baseline < 1
        || parent <= baseline
    {
        return Err(refused("invalid merge range pins"));
    }
    let mut ranges = merge_ranges(previous, source)?;
    if ranges.last().is_some_and(|(_, last)| *last > baseline) {
        return Err(refused("prior mergeinfo exceeds baseline"));
    }
    ranges.push((baseline + 1, parent));
    Ok(mergeinfo_string(source, ranges))
}

fn local_paths(
    root: &Path,
    relative: &str,
    files: &mut BTreeSet<String>,
    directories: &mut BTreeSet<String>,
) -> Result<(), SvnError> {
    for entry in std::fs::read_dir(root.join(relative))? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| refused("unsupported SVN filename"))?;
        if relative.is_empty() && name == ".svn" {
            continue;
        }
        let path = if relative.is_empty() {
            name
        } else {
            format!("{relative}/{name}")
        };
        if !supported_relative_path(&path) {
            return Err(refused("unsupported SVN filename"));
        }
        let metadata = std::fs::symlink_metadata(entry.path())?;
        if metadata.is_dir() {
            directories.insert(path.clone());
            local_paths(root, &path, files, directories)?;
        } else if metadata.is_file() {
            files.insert(path);
        } else {
            return Err(refused("unsupported SVN file kind"));
        }
    }
    Ok(())
}

/// Compare all bytes and executable properties with a pinned Git tree; include
/// directory identity so SVN-only empty directories cannot be silently dropped.
pub fn verify_refresh_svn_tree(
    git_path: &Path,
    tree_sha: &str,
    working_copy: &Path,
    properties: &SvnPropertySnapshot,
    expected_mergeinfo: Option<&str>,
    source_path: &str,
) -> Result<(), SvnError> {
    if !is_full_git_oid(tree_sha) {
        return Err(refused("invalid Git tree pin"));
    }
    let repo =
        Repository::open(git_path).map_err(|_| refused("Git proof repository unavailable"))?;
    require_complete_git_graph(&repo).map_err(|_| refused("incomplete Git proof graph"))?;
    let tree = repo
        .find_tree(Oid::from_str(tree_sha).map_err(|_| refused("invalid Git tree pin"))?)
        .map_err(|_| refused("Git tree pin unavailable"))?;
    require_supported_refresh_tree(&repo, &tree).map_err(|_| refused("unsupported Git tree"))?;
    let mut expected_files = BTreeMap::new();
    let mut expected_directories = BTreeSet::new();
    let mut unsupported = false;
    tree.walk(TreeWalkMode::PreOrder, |prefix, entry| {
        let Some(name) = entry.name() else {
            unsupported = true;
            return TreeWalkResult::Abort;
        };
        let path = format!("{prefix}{name}");
        if !supported_relative_path(&path) {
            unsupported = true;
            return TreeWalkResult::Abort;
        }
        if entry.filemode() == 0o040000 {
            expected_directories.insert(path);
        } else {
            expected_files.insert(path, (entry.id(), entry.filemode() == 0o100755));
        }
        TreeWalkResult::Ok
    })
    .map_err(|_| refused("Git tree enumeration failed"))?;
    if unsupported {
        return Err(refused("unsupported Git filename"));
    }
    let mut files = BTreeSet::new();
    let mut directories = BTreeSet::new();
    local_paths(working_copy, "", &mut files, &mut directories)?;
    if files != expected_files.keys().cloned().collect() || directories != expected_directories {
        return Err(refused("SVN and Git tree paths differ"));
    }
    for (path, values) in properties {
        if !path.is_empty() && !files.contains(path) && !directories.contains(path) {
            return Err(refused("property target missing from tree"));
        }
        for (name, value) in values {
            match name.as_str() {
                "svn:executable"
                    if value == "*"
                        && expected_files
                            .get(path)
                            .is_some_and(|(_, executable)| *executable) => {}
                // SVN assigns this property when adding binary bytes. Permit
                // only this fixed value on regular files; preparation below
                // also requires the result's complete properties to equal the
                // pinned parent, plus the one approved root mergeinfo change.
                "svn:mime-type" if value == "application/octet-stream" && files.contains(path) => {}
                "svn:mergeinfo" if path.is_empty() => {
                    let expected = merge_ranges(expected_mergeinfo, source_path)?;
                    if merge_ranges(Some(value), source_path)? != expected {
                        return Err(refused("SVN mergeinfo differs from approved range"));
                    }
                }
                _ => return Err(refused("unsupported SVN property semantics")),
            }
        }
    }
    let actual_mergeinfo = properties
        .get("")
        .and_then(|values| values.get("svn:mergeinfo"));
    if actual_mergeinfo.is_some() != expected_mergeinfo.is_some() {
        return Err(refused("SVN mergeinfo evidence missing or unexpected"));
    }
    for (path, (blob_id, executable)) in expected_files {
        let actual_executable = properties
            .get(&path)
            .and_then(|values| values.get("svn:executable"))
            .is_some();
        if actual_executable != executable {
            return Err(refused("SVN executable property differs from Git mode"));
        }
        let bytes = std::fs::read(working_copy.join(path))?;
        let blob = repo
            .find_blob(blob_id)
            .map_err(|_| refused("Git blob unavailable"))?;
        if bytes != blob.content() {
            return Err(refused("SVN content differs from pinned Git tree"));
        }
    }
    Ok(())
}

/// Prove both inputs, then perform a normal range merge in a fresh operation
/// directory. No remote commit, export-copy, update of shared checkout or ref
/// change occurs. Failed preparation is retained for diagnosis by the caller.
pub async fn prepare_clean_child_svn(
    child: &SvnClient,
    parent: &SvnClient,
    git_path: &Path,
    operation_root: &Path,
    operation_id: Uuid,
    pins: &CleanChildSvnPins,
) -> Result<PreparedCleanChildSvn, SvnError> {
    Ok(prepare_clean_child_svn_inner(
        child,
        parent,
        git_path,
        operation_root,
        operation_id,
        pins,
        None,
    )
    .await?
    .0)
}

pub(crate) async fn prepare_clean_child_svn_visible(
    child: &SvnClient,
    parent: &SvnClient,
    git_path: &Path,
    operation_root: &Path,
    operation_id: Uuid,
    pins: &CleanChildSvnPins,
    inspected_head: i64,
) -> Result<
    (
        PreparedCleanChildSvn,
        Vec<super::svn_visibility::SvnPresencePin>,
    ),
    SvnError,
> {
    prepare_clean_child_svn_inner(
        child,
        parent,
        git_path,
        operation_root,
        operation_id,
        pins,
        Some(inspected_head),
    )
    .await
}

async fn prepare_clean_child_svn_inner(
    child: &SvnClient,
    parent: &SvnClient,
    git_path: &Path,
    operation_root: &Path,
    operation_id: Uuid,
    pins: &CleanChildSvnPins,
    inspected_head: Option<i64>,
) -> Result<
    (
        PreparedCleanChildSvn,
        Vec<super::svn_visibility::SvnPresencePin>,
    ),
    SvnError,
> {
    let lineage = &pins.lineage.pins;
    let baseline = lineage.child.parent_baseline_revision;
    if operation_id.is_nil()
        || child.url() != lineage.child_url
        || parent.url() != lineage.parent_url
    {
        return Err(refused(
            "preparation client or operation identity differs from proof",
        ));
    }
    if inspected_head
        .is_some_and(|head| head < lineage.parent_revision || head < lineage.child.child_revision)
        || (inspected_head.is_some()
            && (!child.has_refresh_isolation() || !parent.has_refresh_isolation()))
    {
        return Err(refused(
            "visible preparation requires isolated inputs and a current HEAD",
        ));
    }
    let mergeinfo = expected_refresh_mergeinfo(
        &lineage.child.parent_path,
        baseline,
        lineage.parent_revision,
        pins.previous_mergeinfo.as_deref(),
    )?;
    for (client, revision) in [
        (child, lineage.child.child_revision),
        (parent, baseline),
        (parent, lineage.parent_revision),
    ] {
        let info = client.info_at_rev(revision).await?;
        if info.uuid != lineage.child.repository_uuid
            || info.root_url != lineage.repository_root_url
            || info.url != client.url()
            || info.latest_rev != revision
        {
            return Err(refused("preparation snapshot differs from lineage pins"));
        }
    }
    let root = operation_root.canonicalize()?;
    let directory = root.join(operation_id.to_string());
    std::fs::create_dir(&directory)?;
    let working_copy = directory.join("child");
    let mut expected_properties = BTreeMap::new();
    let mut presence = Vec::new();
    for (client, revision, name, tree, old_mergeinfo) in [
        (
            parent,
            baseline,
            "baseline",
            pins.baseline_git_tree.as_str(),
            None,
        ),
        (
            parent,
            lineage.parent_revision,
            "parent",
            pins.parent_git_tree.as_str(),
            None,
        ),
        (
            child,
            lineage.child.child_revision,
            "child",
            pins.baseline_git_tree.as_str(),
            pins.previous_mergeinfo.as_deref(),
        ),
    ] {
        let remote_properties_xml = client.tree_properties_at_rev(revision).await?;
        let checkout = directory.join(name);
        let properties = if inspected_head.is_some() {
            client
                .checkout_full_pinned_without_externals(&checkout, revision)
                .await?;
            let info = client.info_at_rev(revision).await?;
            if info.uuid != lineage.child.repository_uuid
                || info.root_url != lineage.repository_root_url
                || info.url != client.url()
                || info.latest_rev != revision
            {
                return Err(refused("visible snapshot differs from lineage pins"));
            }
            let mut pin = super::svn_visibility::presence_pin(name, &checkout, &info)?;
            pin.ancestors =
                super::svn_visibility::require_empty_visible_ancestors(client, &info).await?;
            presence.push(pin);
            let properties = bound_checkout_property_xml(
                &remote_properties_xml,
                client.url(),
                &checkout,
                &info,
            )?;
            let local = visible_checkout_properties(client, &checkout, &info).await?;
            let meaningful = |mut props: SvnPropertySnapshot| {
                props.retain(|path, values| path.is_empty() || !values.is_empty());
                props
            };
            if meaningful(local) != meaningful(properties.clone()) {
                return Err(refused(
                    "visible checkout properties differ from remote proof",
                ));
            }
            properties
        } else {
            let properties = parse_refresh_properties(&remote_properties_xml, client.url())?;
            client
                .checkout_pinned_without_externals(&checkout, revision)
                .await?;
            properties
        };
        verify_refresh_svn_tree(
            git_path,
            tree,
            &checkout,
            &properties,
            old_mergeinfo,
            &lineage.child.parent_path,
        )?;
        if name == "parent" {
            expected_properties = properties;
        }
    }
    if let Some(head) = inspected_head {
        for (client, checkpoint, role, tree, old_mergeinfo) in [
            (
                parent,
                lineage.parent_revision,
                "current_parent",
                pins.parent_git_tree.as_str(),
                None,
            ),
            (
                child,
                lineage.child.child_revision,
                "current_child",
                pins.baseline_git_tree.as_str(),
                pins.previous_mergeinfo.as_deref(),
            ),
        ] {
            let current = super::svn_visibility::observe_subtree(client, head).await?;
            current.require_unchanged_since(checkpoint)?;
            let info = current.info();
            if info.uuid != lineage.child.repository_uuid
                || info.root_url != lineage.repository_root_url
                || info.url != client.url()
            {
                return Err(refused("current visibility snapshot differs from lineage"));
            }
            let props = visible_checkout_properties(client, current.path(), info).await?;
            verify_refresh_svn_tree(
                git_path,
                tree,
                current.path(),
                &props,
                old_mergeinfo,
                &lineage.child.parent_path,
            )?;
            let mut pin = super::svn_visibility::presence_pin(role, current.path(), info)?;
            pin.ancestors =
                super::svn_visibility::require_empty_visible_ancestors(client, info).await?;
            presence.push(pin);
            let latest = client.info().await?;
            if latest.uuid != info.uuid
                || latest.root_url != info.root_url
                || latest.url != info.url
                || latest.latest_rev != head
            {
                return Err(refused("repository changed during visible preparation"));
            }
        }
    }
    let source = format!("{}@{}", parent.url(), lineage.parent_revision);
    child
        .run_svn_in_dir_public(
            &working_copy,
            &[
                "merge",
                "--accept",
                "postpone",
                "-r",
                &format!("{baseline}:{}", lineage.parent_revision),
                &source,
                ".",
            ],
        )
        .await?;
    // SVN emits conflicts as successful command output; tree comparison alone
    // is insufficient because a property or tree conflict may retain the bytes.
    let status = child
        .run_svn_in_dir_public(&working_copy, &["status", "--xml", "."])
        .await?;
    require_refresh_merge_status(&status)?;
    let xml = child
        .run_svn_in_dir_public(
            &working_copy,
            &[
                "proplist",
                "--xml",
                "--verbose",
                "--recursive",
                "--show-inherited-props",
                working_copy
                    .to_str()
                    .ok_or_else(|| refused("unsupported working directory"))?,
            ],
        )
        .await?;
    let mut properties = parse_refresh_properties(
        &xml,
        working_copy
            .to_str()
            .ok_or_else(|| refused("unsupported working directory"))?,
    )?;
    verify_refresh_svn_tree(
        git_path,
        &pins.parent_git_tree,
        &working_copy,
        &properties,
        Some(&mergeinfo),
        &lineage.child.parent_path,
    )?;
    expected_properties
        .entry(String::new())
        .or_default()
        .insert("svn:mergeinfo".into(), mergeinfo.clone());
    expected_properties.retain(|path, values| path.is_empty() || !values.is_empty());
    properties.retain(|path, values| path.is_empty() || !values.is_empty());
    if properties != expected_properties {
        return Err(refused(
            "SVN merge properties differ from pinned parent and approved mergeinfo",
        ));
    }
    Ok((
        PreparedCleanChildSvn {
            operation_id,
            pins: pins.clone(),
            working_copy,
            mergeinfo,
            result_git_tree: pins.parent_git_tree.clone(),
            result_properties: properties,
        },
        presence,
    ))
}
