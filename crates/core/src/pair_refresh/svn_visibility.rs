//! Fresh subtree checkout presence evidence. Listing/export/status cannot
//! establish authz completeness. This grants no job or publication authority;
//! lineage, inherited properties, content and actual actor/credential binding
//! are separate requirements of the authenticated execution caller.

use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::db::opened_file::OpenedDatabaseFile;
use crate::errors::SvnError;
use crate::svn::{SvnClient, SvnInfo};

#[cfg(test)]
#[path = "svn_visibility_tests.rs"]
mod tests;

fn refused(detail: &str) -> SvnError {
    SvnError::LineageRefused {
        reason: "unproved_svn_subtree_visibility".into(),
        detail: detail.into(),
    }
}

/// Constructed only by a fresh isolated checkout and native metadata check.
/// It is not a client-deserializable claim or a resource adoption capability.
pub struct VisibleSvnCheckout {
    _directory: tempfile::TempDir,
    path: PathBuf,
    info: SvnInfo,
    node_count: usize,
    max_changed_revision: i64,
    presence_digest: String,
}

/// Safe facts from a particular fresh snapshot, not admission authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SvnPresencePin {
    pub profile: String,
    pub role: String,
    pub repository_uuid: String,
    pub repository_root_url: String,
    pub url: String,
    pub revision: i64,
    pub max_changed_revision: i64,
    pub node_count: usize,
    pub presence_digest: String,
}

pub(crate) fn presence_pin(
    role: &str,
    path: &Path,
    info: &SvnInfo,
) -> Result<SvnPresencePin, SvnError> {
    let (node_count, max_changed_revision, presence_digest) = inspect_fresh_checkout(path, info)?;
    Ok(SvnPresencePin {
        profile: "svn_wc31_full_subtree_presence_v1".into(),
        role: role.into(),
        repository_uuid: info.uuid.clone(),
        repository_root_url: info.root_url.clone(),
        url: info.url.clone(),
        revision: info.latest_rev,
        max_changed_revision,
        node_count,
        presence_digest,
    })
}

impl VisibleSvnCheckout {
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn info(&self) -> &SvnInfo {
        &self.info
    }
    pub fn node_count(&self) -> usize {
        self.node_count
    }
    pub fn require_unchanged_since(&self, checkpoint: i64) -> Result<(), SvnError> {
        if checkpoint < 1
            || checkpoint > self.info.latest_rev
            || self.max_changed_revision > checkpoint
        {
            return Err(refused(
                "visible subtree contains changes after its handled checkpoint",
            ));
        }
        Ok(())
    }
    pub fn presence_digest(&self) -> &str {
        &self.presence_digest
    }
}

/// Observe a pegged subtree using an already isolated registered-source client.
/// The private checkout remains owned by the returned observation. The complete
/// WC format is explicit: unsupported schema/state refuses, never list fallback.
pub async fn observe_subtree(
    client: &SvnClient,
    revision: i64,
) -> Result<VisibleSvnCheckout, SvnError> {
    if !client.has_refresh_isolation() || revision < 1 {
        return Err(refused(
            "isolated credentials and a positive peg revision are required",
        ));
    }
    let info = client.info_at_rev(revision).await?;
    if info.url != client.url() || info.latest_rev != revision {
        return Err(refused(
            "pegged source identity differs from the requested snapshot",
        ));
    }
    let directory = tempfile::Builder::new()
        .prefix("reposync-svn-visibility-")
        .tempdir()?;
    let path = directory.path().join("snapshot");
    client
        .checkout_full_pinned_without_externals(&path, revision)
        .await?;
    let (node_count, max_changed_revision, presence_digest) = inspect_fresh_checkout(&path, &info)?;
    let after = client.info_at_rev(revision).await?;
    if info.uuid != after.uuid
        || info.root_url != after.root_url
        || info.url != after.url
        || info.latest_rev != after.latest_rev
    {
        return Err(refused(
            "pegged source changed during visibility observation",
        ));
    }
    Ok(VisibleSvnCheckout {
        _directory: directory,
        path,
        info,
        node_count,
        max_changed_revision,
        presence_digest,
    })
}

#[derive(Serialize)]
struct Node {
    wc: i64,
    path: String,
    layer: i64,
    parent: Option<String>,
    repository: Option<i64>,
    repository_path: Option<String>,
    revision: Option<i64>,
    presence: String,
    kind: String,
    depth: Option<String>,
    file_external: Option<i64>,
    changed_revision: Option<i64>,
}

fn relative(path: &str, root: bool) -> bool {
    (root && path.is_empty())
        || (!path.is_empty()
            && path
                .split('/')
                .all(|s| !matches!(s, "" | "." | "..") && !s.contains(['\\', '\0'])))
}

fn node_url(root: &str, path: &str) -> Result<reqwest::Url, SvnError> {
    let mut url =
        reqwest::Url::parse(root).map_err(|_| refused("invalid metadata repository root"))?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !relative(path, true)
    {
        return Err(refused("unsupported metadata repository location"));
    }
    if !path.is_empty() {
        url.path_segments_mut()
            .map_err(|_| refused("unsupported metadata repository root"))?
            .pop_if_empty()
            .extend(path.split('/'));
    }
    Ok(url)
}

pub(crate) fn inspect_fresh_checkout(
    path: &Path,
    expected: &SvnInfo,
) -> Result<(usize, i64, String), SvnError> {
    read_metadata(path, |conn| inspect_rows(conn, expected))
}

pub(crate) fn has_proved_empty_root_properties(
    path: &Path,
    expected: &SvnInfo,
) -> Result<bool, SvnError> {
    read_metadata(path, |conn| {
        inspect_rows(conn, expected)?;
        let properties: Option<Vec<u8>> = conn
            .query_row(
                "SELECT properties FROM NODES WHERE local_relpath='' AND op_depth=0",
                [],
                |row| row.get(0),
            )
            .map_err(|_| refused("root properties are unavailable"))?;
        // Exact WC31 property skeleton. NULL or the empty list represents no
        // direct properties; arbitrary missing XML alone is never that proof.
        Ok(properties.as_deref().is_none_or(|value| value == b"()"))
    })
}

fn read_metadata<T>(
    path: &Path,
    inspect: impl FnOnce(&Connection) -> Result<T, SvnError>,
) -> Result<T, SvnError> {
    let metadata_path = path.join(".svn/wc.db");
    let metadata = std::fs::symlink_metadata(&metadata_path)?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.uid() != rustix::process::geteuid().as_raw()
    {
        return Err(refused("working-copy metadata file is unsupported"));
    }
    let prior = OpenedDatabaseFile::capture(&metadata_path);
    let conn = Connection::open_with_flags(&metadata_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|_| refused("working-copy metadata cannot be read"))?;
    let identity = OpenedDatabaseFile::attest(&conn, &metadata_path, prior)
        .ok_or_else(|| refused("working-copy metadata descriptor identity is unknown"))?;
    conn.execute_batch("BEGIN")
        .map_err(|_| refused("working-copy metadata snapshot unavailable"))?;
    let result = inspect(&conn);
    if OpenedDatabaseFile::attest(&conn, &metadata_path, Some(identity)).is_none() {
        return Err(refused("working-copy metadata identity changed"));
    }
    conn.execute_batch("ROLLBACK")
        .map_err(|_| refused("working-copy read snapshot could not finish"))?;
    result
}

fn inspect_rows(conn: &Connection, expected: &SvnInfo) -> Result<(usize, i64, String), SvnError> {
    inspect_rows_sql(conn, expected)
        .map_err(|_| refused("working-copy metadata is incomplete, excluded or unsupported"))
}

fn inspect_rows_sql(conn: &Connection, expected: &SvnInfo) -> anyhow::Result<(usize, i64, String)> {
    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    anyhow::ensure!(version == 31, "unqualified WC schema");
    let integrity: Vec<String> = conn
        .prepare("PRAGMA integrity_check")?
        .query_map([], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    anyhow::ensure!(integrity == ["ok"], "invalid WC database");
    for table in ["ACTUAL_NODE", "EXTERNALS", "WC_LOCK", "WORK_QUEUE"] {
        let count: i64 = conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
            row.get(0)
        })?;
        anyhow::ensure!(count == 0, "not a pristine completed checkout");
    }
    let repositories: Vec<(i64, String, String)> = conn
        .prepare("SELECT id,root,uuid FROM REPOSITORY ORDER BY id")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<Result<_, _>>()?;
    anyhow::ensure!(repositories.len() == 1, "ambiguous repository");
    let (repo_id, root, uuid) = &repositories[0];
    anyhow::ensure!(
        root == &expected.root_url && uuid == &expected.uuid,
        "repository identity mismatch"
    );
    let roots: Vec<(i64, Option<String>)> = conn
        .prepare("SELECT id,local_abspath FROM WCROOT ORDER BY id")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_, _>>()?;
    anyhow::ensure!(
        roots.len() == 1 && roots[0].1.is_none(),
        "ambiguous working-copy root"
    );
    let nodes: Vec<Node> = conn.prepare("SELECT wc_id,local_relpath,op_depth,parent_relpath,repos_id,repos_path,revision,presence,kind,depth,file_external,changed_revision FROM NODES ORDER BY local_relpath,op_depth")?
        .query_map([], |r| Ok(Node {wc:r.get(0)?,path:r.get(1)?,layer:r.get(2)?,parent:r.get(3)?,repository:r.get(4)?,repository_path:r.get(5)?,revision:r.get(6)?,presence:r.get(7)?,kind:r.get(8)?,depth:r.get(9)?,file_external:r.get(10)?,changed_revision:r.get(11)?}))?
        .collect::<Result<_, _>>()?;
    let mut by_path = BTreeMap::new();
    for node in &nodes {
        anyhow::ensure!(
            relative(&node.path, true) && by_path.insert(&node.path, node).is_none(),
            "ambiguous node path"
        );
        anyhow::ensure!(
            node.wc == roots[0].0
                && node.layer == 0
                && node.repository == Some(*repo_id)
                && node.revision == Some(expected.latest_rev)
                && node.presence == "normal"
                && node.file_external.is_none(),
            "excluded, layered, switched or incomplete node"
        );
        anyhow::ensure!(
            node.changed_revision
                .is_some_and(|r| r >= 0 && r <= expected.latest_rev),
            "missing or contradictory node change revision"
        );
        anyhow::ensure!(
            matches!(node.kind.as_str(), "file" | "dir"),
            "unsupported node kind"
        );
        anyhow::ensure!(
            if node.kind == "dir" {
                node.depth.as_deref() == Some("infinity")
            } else {
                node.depth.is_none()
            },
            "sparse or unsupported depth"
        );
    }
    let root_node = by_path
        .get(&String::new())
        .ok_or_else(|| anyhow::anyhow!("missing root"))?;
    let base = root_node
        .repository_path
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("missing root location"))?;
    anyhow::ensure!(
        root_node.kind == "dir" && root_node.parent.is_none(),
        "invalid root node"
    );
    anyhow::ensure!(
        node_url(root, base)? == reqwest::Url::parse(&expected.url)?,
        "subtree location mismatch"
    );
    for node in &nodes {
        let target = if node.path.is_empty() {
            base.to_owned()
        } else if base.is_empty() {
            node.path.clone()
        } else {
            format!("{base}/{}", node.path)
        };
        anyhow::ensure!(
            node.repository_path.as_deref() == Some(target.as_str()),
            "switched descendant"
        );
        if !node.path.is_empty() {
            let parent = node.path.rsplit_once('/').map_or("", |(p, _)| p);
            anyhow::ensure!(
                node.parent.as_deref() == Some(parent)
                    && by_path
                        .get(&parent.to_owned())
                        .is_some_and(|p| p.kind == "dir"),
                "missing or invalid parent"
            );
        }
    }
    let encoded = serde_json::to_vec(&("pair_refresh_svn_presence_v1", version, expected, &nodes))?;
    let max_changed = nodes
        .iter()
        .filter_map(|n| n.changed_revision)
        .max()
        .ok_or_else(|| anyhow::anyhow!("missing change revisions"))?;
    Ok((
        nodes.len(),
        max_changed,
        hex::encode(Sha256::digest(encoded)),
    ))
}
