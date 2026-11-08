//! Consumes one sealed 54e18348 partial-import artifact. Default builds keep
//! this file feature-gated so the historical case is absent until the named
//! reliability-fixture profile runs. A missing artifact fails.

#![cfg(feature = "reliability-fixture")]

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use reposync_core::db::import_operations::import_target_fingerprint;
use reposync_core::db::lineage_checkpoints::{
    independently_established_svn_uuid, independently_established_svn_uuid_kv_key,
};
use reposync_core::file_policy::FilePolicy;
use reposync_core::git::GitClient;
use reposync_core::identity::IdentityMapper;
use reposync_core::import::{
    apply_import_reconciliation, ImportConfig, ImportOutcome, ImportProgress, ImportRunState,
};
use reposync_core::svn::SvnClient;
use reposync_core::{config::IdentityConfig, Database};

const ATTEMPT_ROOT_ENV: &str = "REPOSYNC_RS63_ATTEMPT_ROOT";
const ATTEMPT_ID_ENV: &str = "REPOSYNC_RS63_ATTEMPT_ID";
const MANIFEST_PATH_ENV: &str = "REPOSYNC_RS63_MANIFEST_PATH";
const MANIFEST_SHA_ENV: &str = "REPOSYNC_RS63_MANIFEST_SHA256";
const PRESERVED_FAILURE_ROOTS: &[&str] = &[
    "/tmp/rs63-predecessor-partial-import-hold",
    "/private/tmp/rs63-predecessor-partial-import-hold",
    "/tmp/.tmpRJUzYe",
    "/private/tmp/.tmpRJUzYe",
];

#[derive(Debug)]
struct Manifest {
    raw: serde_json::Value,
    path: PathBuf,
}

impl Manifest {
    fn load_admitted() -> Self {
        let path = std::env::var(MANIFEST_PATH_ENV)
            .unwrap_or_else(|_| panic!("missing {MANIFEST_PATH_ENV}"));
        let sha = std::env::var(MANIFEST_SHA_ENV)
            .unwrap_or_else(|_| panic!("missing {MANIFEST_SHA_ENV}"));
        let attempt_id =
            std::env::var(ATTEMPT_ID_ENV).unwrap_or_else(|_| panic!("missing {ATTEMPT_ID_ENV}"));
        let attempt_root = std::env::var(ATTEMPT_ROOT_ENV)
            .unwrap_or_else(|_| panic!("missing {ATTEMPT_ROOT_ENV}"));
        admit_manifest(
            Path::new(&path),
            &sha,
            &attempt_id,
            Path::new(&attempt_root),
        )
        .unwrap_or_else(|error| panic!("admitted manifest refused: {error}"))
    }

    fn str<'a>(&'a self, key: &str) -> &'a str {
        self.raw[key]
            .as_str()
            .unwrap_or_else(|| panic!("manifest {key}"))
    }

    fn path(&self, key: &str) -> PathBuf {
        PathBuf::from(self.str(key))
    }

    fn u64(&self, key: &str) -> u64 {
        self.raw[key]
            .as_u64()
            .unwrap_or_else(|| panic!("manifest {key}"))
    }
}

fn sha256_file(path: &Path) -> (u64, String) {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(path).unwrap();
    let mut hasher = Sha256::new();
    let mut buf = [0_u8; 64 * 1024];
    let mut bytes = 0_u64;
    loop {
        let read = file.read(&mut buf).unwrap();
        if read == 0 {
            break;
        }
        bytes += read as u64;
        hasher.update(&buf[..read]);
    }
    (bytes, hex::encode(hasher.finalize()))
}

fn unix_metadata_mode(meta: &std::fs::Metadata) -> Result<u32, String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(meta.mode())
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        Err("historical member modes require Unix metadata".into())
    }
}

fn relative_member_is_safe(rel: &str) -> bool {
    !rel.is_empty()
        && !rel.starts_with('/')
        && !rel.contains('\\')
        && !rel.contains('\0')
        && Path::new(rel)
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
}

fn verify_sealed_tree(
    root: &Path,
    members: &[serde_json::Value],
    label: &str,
) -> Result<(), String> {
    if !root.is_absolute() {
        return Err(format!("{label} is not an absolute sealed path"));
    }
    let root_meta =
        std::fs::symlink_metadata(root).map_err(|error| format!("{label} root: {error}"))?;
    if !root_meta.is_dir() || root_meta.file_type().is_symlink() {
        return Err(format!("{label} root is not a real directory"));
    }
    let mut expected = std::collections::BTreeMap::new();
    for member in members {
        let rel = member["path"]
            .as_str()
            .ok_or_else(|| format!("{label} member path"))?;
        if !relative_member_is_safe(rel) {
            return Err(format!("{label} unsafe member {rel}"));
        }
        if expected.insert(rel, member).is_some() {
            return Err(format!("{label} duplicate member {rel}"));
        }
    }
    if expected.is_empty() {
        return Err(format!("{label} has no sealed members"));
    }
    let mut seen = std::collections::BTreeSet::new();
    fn visit(
        root: &Path,
        dir: &Path,
        expected: &std::collections::BTreeMap<&str, &serde_json::Value>,
        seen: &mut std::collections::BTreeSet<String>,
        label: &str,
    ) -> Result<(), String> {
        let mut entries = std::fs::read_dir(dir)
            .map_err(|error| format!("{label} read {dir:?}: {error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("{label} entry: {error}"))?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let rel = path
                .strip_prefix(root)
                .map_err(|_| format!("{label} path escaped {}", path.display()))?
                .to_string_lossy()
                .replace('\\', "/");
            if !relative_member_is_safe(&rel) {
                return Err(format!("{label} unsafe path {rel}"));
            }
            let member = match expected.get(rel.as_str()) {
                Some(member) => *member,
                None => return Err(format!("{label} unexpected extra entry {rel}")),
            };
            let meta = std::fs::symlink_metadata(&path)
                .map_err(|error| format!("{label} {rel}: {error}"))?;
            let file_type = meta.file_type();
            let actual = if file_type.is_symlink() {
                "symlink"
            } else if file_type.is_dir() {
                "dir"
            } else if file_type.is_file() {
                "file"
            } else {
                "other"
            };
            let kind = member["kind"]
                .as_str()
                .ok_or_else(|| format!("{label} kind {rel}"))?;
            if actual != kind {
                return Err(format!("{label} changed type {rel}"));
            }
            let mode = unix_metadata_mode(&meta)?;
            let sealed_mode = member["mode"]
                .as_u64()
                .ok_or_else(|| format!("{label} mode {rel}"))? as u32;
            if mode != sealed_mode {
                return Err(format!("{label} changed mode {rel}"));
            }
            match kind {
                "dir" => visit(root, &path, expected, seen, label)?,
                "file" => {
                    let (bytes, sha) = sha256_file(&path);
                    let sealed_bytes = member["bytes"]
                        .as_u64()
                        .ok_or_else(|| format!("{label} bytes {rel}"))?;
                    let sealed_sha = member["sha256"]
                        .as_str()
                        .ok_or_else(|| format!("{label} sha256 {rel}"))?;
                    if bytes != sealed_bytes || sha != sealed_sha {
                        return Err(format!("{label} changed content {rel}"));
                    }
                }
                "symlink" => {
                    let target = std::fs::read_link(&path)
                        .map_err(|error| format!("{label} link {rel}: {error}"))?;
                    let sealed = member["link"]
                        .as_str()
                        .ok_or_else(|| format!("{label} link text {rel}"))?;
                    if target.to_string_lossy() != sealed {
                        return Err(format!("{label} changed content {rel}"));
                    }
                }
                other => return Err(format!("{label} unknown member kind {other}")),
            }
            seen.insert(rel);
        }
        Ok(())
    }
    visit(root, root, &expected, &mut seen, label)?;
    for rel in expected.keys() {
        if !seen.contains(*rel) {
            return Err(format!("{label} missing sealed member {rel}"));
        }
    }
    Ok(())
}

fn clear_owned_children(root: &Path) -> Result<(), String> {
    let root_meta =
        std::fs::symlink_metadata(root).map_err(|error| format!("owned root: {error}"))?;
    if !root_meta.is_dir() || root_meta.file_type().is_symlink() {
        return Err("owned root is not a real directory".into());
    }
    let canonical_root = root
        .canonicalize()
        .map_err(|error| format!("owned root: {error}"))?;
    for entry in std::fs::read_dir(root).map_err(|error| format!("owned root: {error}"))? {
        let path = entry
            .map_err(|error| format!("owned child: {error}"))?
            .path();
        if !path.starts_with(root) {
            return Err(format!(
                "refusing path outside owned root {}",
                path.display()
            ));
        }
        let meta = std::fs::symlink_metadata(&path)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if meta.file_type().is_symlink() {
            std::fs::remove_file(&path)
                .map_err(|error| format!("remove symlink {}: {error}", path.display()))?;
            continue;
        }
        let canonical = path
            .canonicalize()
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if !canonical.starts_with(&canonical_root) {
            return Err(format!(
                "refusing deletion outside owned root {}",
                path.display()
            ));
        }
        if meta.is_dir() {
            std::fs::remove_dir_all(&path)
                .map_err(|error| format!("remove dir {}: {error}", path.display()))?;
        } else {
            std::fs::remove_file(&path)
                .map_err(|error| format!("remove file {}: {error}", path.display()))?;
        }
    }
    Ok(())
}

fn restore_exact_root(
    root: &Path,
    master: &Path,
    members: &[serde_json::Value],
) -> Result<(), String> {
    if root == master {
        return Err("working root and sealed master are the same path".into());
    }
    verify_sealed_tree(master, members, "master")?;
    clear_owned_children(root)?;
    let destination = root.to_str().ok_or("working root path is not Unicode")?;
    let status = Command::new("cp")
        .args(["-a", &format!("{}/.", master.display()), destination])
        .status()
        .map_err(|error| format!("restore copy: {error}"))?;
    if !status.success() {
        return Err("restore from immutable master failed".into());
    }
    verify_sealed_tree(root, members, "working root")
}

fn sealed_members(manifest: &Manifest) -> Result<&[serde_json::Value], String> {
    manifest.raw["members"]
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| "manifest members".into())
}

fn same_or_nested(left: &Path, right: &Path) -> bool {
    left == right || left.starts_with(right) || right.starts_with(left)
}

fn touches_preserved(path: &Path) -> Result<(), String> {
    for raw in PRESERVED_FAILURE_ROOTS {
        let preserved = Path::new(raw);
        if same_or_nested(path, preserved) {
            return Err(format!("path touches preserved failure {raw}"));
        }
        if let Ok(meta) = std::fs::symlink_metadata(preserved) {
            if meta.is_dir() && !meta.file_type().is_symlink() {
                if let Ok(canonical) = preserved.canonicalize() {
                    if same_or_nested(path, &canonical) {
                        return Err(format!(
                            "path touches preserved failure {}",
                            canonical.display()
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

fn canonical_real_dir(path: &Path) -> Result<PathBuf, String> {
    let meta =
        std::fs::symlink_metadata(path).map_err(|error| format!("{}: {error}", path.display()))?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(format!("{} is not a real directory", path.display()));
    }
    path.canonicalize()
        .map_err(|error| format!("{}: {error}", path.display()))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

fn admit_manifest(
    path: &Path,
    expected_sha: &str,
    attempt_id: &str,
    attempt_root: &Path,
) -> Result<Manifest, String> {
    let bytes = std::fs::read(path).map_err(|error| format!("manifest read: {error}"))?;
    let actual_sha = sha256_bytes(&bytes);
    if actual_sha != expected_sha {
        return Err("manifest sha256 does not match the admitted value".into());
    }
    let raw: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|error| format!("manifest json: {error}"))?;
    let required = |key: &str| -> Result<String, String> {
        raw.get(key)
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .ok_or_else(|| format!("manifest {key}"))
    };
    let recorded_attempt_id = required("attempt_id")?;
    let producer_commit = required("producer_commit")?;
    if recorded_attempt_id != attempt_id {
        return Err("manifest attempt id does not match the admitted attempt".into());
    }
    if producer_commit != "54e183484961bd9ad12bd03505b3b00610de7cd9" {
        return Err("manifest producer commit is not the old source".into());
    }
    let recorded_attempt_root = required("attempt_root")?;
    let recorded_live = required("root")?;
    let recorded_master = required("master")?;
    let recorded_snapshot = required("snapshot")?;
    let admitted_root = canonical_real_dir(attempt_root)?;
    let recorded_root = canonical_real_dir(Path::new(&recorded_attempt_root))?;
    if admitted_root != recorded_root {
        return Err("manifest attempt root does not match the admitted root".into());
    }
    let live = canonical_real_dir(Path::new(&recorded_live))?;
    let master = canonical_real_dir(Path::new(&recorded_master))?;
    let snapshot = canonical_real_dir(Path::new(&recorded_snapshot))?;
    if live != PathBuf::from(&recorded_live) {
        return Err("working root path is not the recorded absolute path".into());
    }
    if !live.starts_with(&admitted_root) || master == live || snapshot == live {
        return Err("foreign or aliased working root".into());
    }
    if !master.starts_with(&admitted_root) || !snapshot.starts_with(&admitted_root) {
        return Err("sealed source is outside the admitted attempt".into());
    }
    if master.starts_with(&live) || snapshot.starts_with(&live) || master == snapshot {
        return Err("sealed source aliases the working root".into());
    }
    for source in [&live, &master, &snapshot, &admitted_root] {
        touches_preserved(source)?;
    }
    drop(required);
    let manifest = Manifest {
        raw,
        path: path.to_path_buf(),
    };
    let members = sealed_members(&manifest)?;
    verify_sealed_tree(&master, members, "master")?;
    verify_sealed_tree(&snapshot, members, "snapshot")?;
    assert_distinct_file_inodes(&master, &snapshot)?;
    if manifest.path != path {
        return Err("manifest path changed during admission".into());
    }
    Ok(manifest)
}

fn assert_distinct_file_inodes(left: &Path, right: &Path) -> Result<(), String> {
    #[cfg(not(unix))]
    {
        let _ = (left, right);
        return Err("inode independence requires Unix metadata".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        fn visit(base: &Path, dir: &Path, other_root: &Path) -> Result<(), String> {
            let mut entries = std::fs::read_dir(dir)
                .map_err(|error| format!("inode walk {}: {error}", dir.display()))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| format!("inode entry: {error}"))?;
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                let path = entry.path();
                let meta = std::fs::symlink_metadata(&path)
                    .map_err(|error| format!("{}: {error}", path.display()))?;
                let rel = path
                    .strip_prefix(base)
                    .map_err(|_| format!("inode path escaped {}", path.display()))?;
                if meta.file_type().is_symlink() {
                    continue;
                }
                if meta.is_dir() {
                    visit(base, &path, other_root)?;
                    continue;
                }
                if !meta.is_file() {
                    continue;
                }
                let other = other_root.join(rel);
                let other_meta = match std::fs::symlink_metadata(&other) {
                    Ok(meta) => meta,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(format!("{}: {error}", other.display())),
                };
                if other_meta.is_file()
                    && meta.dev() == other_meta.dev()
                    && meta.ino() == other_meta.ino()
                {
                    return Err(format!("inode alias {}", rel.display()));
                }
            }
            Ok(())
        }
        visit(left, left, right)
    }
}

fn copy_permissions(from: &Path, to: &Path) -> Result<(), String> {
    let meta = std::fs::symlink_metadata(from)
        .map_err(|error| format!("mode source {}: {error}", from.display()))?;
    std::fs::set_permissions(to, meta.permissions())
        .map_err(|error| format!("mode copy {}: {error}", to.display()))
}

fn copy_tree_independent(src: &Path, dst: &Path) -> Result<(), String> {
    if std::fs::symlink_metadata(dst).is_ok() {
        return Err(format!("copy destination already exists {}", dst.display()));
    }
    std::fs::create_dir(dst).map_err(|error| format!("copy create {}: {error}", dst.display()))?;
    copy_permissions(src, dst)?;
    copy_children_independent(src, dst)?;
    assert_distinct_file_inodes(src, dst)?;
    Ok(())
}

fn copy_children_independent(src: &Path, dst: &Path) -> Result<(), String> {
    let mut entries = std::fs::read_dir(src)
        .map_err(|error| format!("read {}: {error}", src.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("entry: {error}"))?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let from = entry.path();
        let into = dst.join(entry.file_name());
        let meta = std::fs::symlink_metadata(&from)
            .map_err(|error| format!("{}: {error}", from.display()))?;
        if meta.file_type().is_symlink() {
            let target = std::fs::read_link(&from)
                .map_err(|error| format!("link {}: {error}", from.display()))?;
            #[cfg(unix)]
            {
                std::os::unix::fs::symlink(&target, &into)
                    .map_err(|error| format!("symlink {}: {error}", into.display()))?;
            }
            #[cfg(not(unix))]
            {
                let _ = target;
                return Err("symlink copies require Unix".into());
            }
        } else if meta.is_dir() {
            std::fs::create_dir(&into)
                .map_err(|error| format!("dir {}: {error}", into.display()))?;
            copy_permissions(&from, &into)?;
            copy_children_independent(&from, &into)?;
        } else if meta.is_file() {
            std::fs::copy(&from, &into)
                .map_err(|error| format!("file {}: {error}", into.display()))?;
        } else {
            return Err(format!("unsupported member {}", from.display()));
        }
    }
    Ok(())
}

fn require_raw_seals(manifest: &Manifest, label: &str) -> Result<(), String> {
    let members = sealed_members(manifest)?;
    verify_sealed_tree(
        &manifest.path("master"),
        members,
        &format!("{label} master"),
    )?;
    verify_sealed_tree(
        &manifest.path("snapshot"),
        members,
        &format!("{label} snapshot"),
    )?;
    assert_distinct_file_inodes(&manifest.path("master"), &manifest.path("snapshot"))?;
    let live = manifest.path("root");
    if std::fs::symlink_metadata(&live).is_ok() {
        assert_distinct_file_inodes(&manifest.path("master"), &live)?;
    }
    Ok(())
}

fn park_and_restore(manifest: &Manifest, label: &str) -> Result<(), String> {
    let root_text = manifest.str("root").to_string();
    let fingerprint = manifest.str("fingerprint").to_string();
    require_raw_seals(manifest, label)?;
    let live = manifest.path("root");
    if live != PathBuf::from(&root_text) || !live.is_absolute() {
        return Err("working root is not the sealed absolute path".into());
    }
    let slot = manifest.path("attempt_root").join("parked").join(label);
    if !slot.starts_with(&manifest.path("attempt_root")) || slot.starts_with(&live) {
        return Err("phase slot escaped the attempt envelope".into());
    }
    if std::fs::symlink_metadata(&slot).is_ok() {
        return Err(format!("phase slot already exists {}", slot.display()));
    }
    std::fs::rename(&live, &slot).map_err(|error| format!("park {label}: {error}"))?;
    if std::fs::symlink_metadata(&live).is_ok() {
        return Err("live path still present after park".into());
    }
    copy_tree_independent(&manifest.path("master"), &live)?;
    verify_sealed_tree(&live, sealed_members(manifest)?, "working root")?;
    require_raw_seals(manifest, &format!("{label} restored"))?;
    if manifest.str("root") != root_text || manifest.str("fingerprint") != fingerprint {
        return Err("sealed path or fingerprint changed".into());
    }
    if manifest.path("root") != live {
        return Err("working root path changed".into());
    }
    Ok(())
}

fn run_ok(cmd: &mut Command) -> std::process::Output {
    let output = cmd
        .output()
        .unwrap_or_else(|error| panic!("{cmd:?}: {error}"));
    assert!(
        output.status.success(),
        "{cmd:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn git_ref(manifest: &Manifest) -> (String, String) {
    let head =
        run_ok(Command::new("git").args(["-C", manifest.str("workdir"), "rev-parse", "HEAD"]));
    let bare = run_ok(Command::new("git").args([
        "--git-dir",
        manifest.str("bare"),
        "rev-parse",
        "refs/heads/main",
    ]));
    (
        String::from_utf8_lossy(&head.stdout).trim().to_string(),
        String::from_utf8_lossy(&bare.stdout).trim().to_string(),
    )
}

fn replace_svn_same_url(manifest: &Manifest) {
    let svn_repo = manifest.path("svn_repo");
    std::fs::remove_dir_all(&svn_repo).unwrap();
    run_ok(Command::new("svnadmin").args(["create", svn_repo.to_str().unwrap()]));
    let seed = manifest.path("root").join("replacement-seed");
    std::fs::create_dir_all(seed.join("trunk")).unwrap();
    std::fs::write(seed.join("trunk").join("history.txt"), "revision-1\n").unwrap();
    let svn_url = manifest.str("svn_url");
    run_ok(Command::new("svn").args([
        "import",
        seed.join("trunk").to_str().unwrap(),
        &format!("{svn_url}/trunk"),
        "-m",
        "revision 1",
        "--username",
        "fixture",
        "--non-interactive",
    ]));
    let checkout = manifest.path("root").join("replacement-wc");
    run_ok(Command::new("svn").args([
        "checkout",
        &format!("{svn_url}/trunk"),
        checkout.to_str().unwrap(),
        "--username",
        "fixture",
        "--non-interactive",
    ]));
    for revision in 2..=51 {
        std::fs::write(
            checkout.join("history.txt"),
            format!("revision-{revision}\n"),
        )
        .unwrap();
        run_ok(
            Command::new("svn")
                .args([
                    "commit",
                    "-m",
                    &format!("revision {revision}"),
                    "--username",
                    "fixture",
                    "--non-interactive",
                ])
                .current_dir(&checkout),
        );
    }
    let youngest = run_ok(Command::new("svnlook").args(["youngest", svn_repo.to_str().unwrap()]));
    assert_eq!(String::from_utf8_lossy(&youngest.stdout).trim(), "51");
    let uuid = String::from_utf8(
        run_ok(Command::new("svnlook").args(["uuid", svn_repo.to_str().unwrap()])).stdout,
    )
    .unwrap();
    assert_ne!(uuid.trim(), manifest.str("observed_svn_uuid"));
    let body = run_ok(Command::new("svnlook").args([
        "cat",
        "-r",
        "50",
        svn_repo.to_str().unwrap(),
        "trunk/history.txt",
    ]));
    assert_eq!(body.stdout, b"revision-50\n");
}

struct Effects {
    maps: i64,
    max_rev: i64,
    applied: i64,
    watermark_rev: i64,
    watermark_sha: String,
    git_head: String,
    bare_head: String,
}

fn effects(db: &Database, manifest: &Manifest) -> Effects {
    let conn = db.conn();
    let (maps, max_rev): (i64, i64) = conn
        .query_row(
            "SELECT COUNT(*), COALESCE(MAX(svn_rev), 0) FROM commit_map",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let applied = conn
        .query_row(
            "SELECT COUNT(*) FROM canonical_mapping_outcomes WHERE outcome = 'applied_and_verified'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0_i64);
    drop(conn);
    let (watermark_rev, watermark_sha) = db.get_repo_watermark(manifest.str("repo_id")).unwrap();
    let (git_head, bare_head) = git_ref(manifest);
    Effects {
        maps,
        max_rev,
        applied,
        watermark_rev,
        watermark_sha,
        git_head,
        bare_head,
    }
}

async fn refuse_resume(db: &Database, manifest: &Manifest, label: &str) {
    let repo = db
        .get_repository(manifest.str("repo_id"))
        .unwrap()
        .expect("repository");
    let workdir = manifest.path("workdir");
    let svn_url = format!(
        "{}/{}",
        repo.svn_url.trim_end_matches('/'),
        repo.svn_branch.trim_start_matches('/')
    );
    let svn = SvnClient::new(svn_url, repo.svn_username.clone(), "");
    let git = Arc::new(Mutex::new(GitClient::new(&workdir).unwrap()));
    let mapper = IdentityMapper::new(&IdentityConfig::default()).unwrap();
    let policy = FilePolicy::with_lfs(0, Vec::new(), 0, &[]);
    let config = ImportConfig {
        committer_name: "RepoSync".into(),
        committer_email: "reposync@localhost".into(),
        remote_name: "origin".into(),
        branch: repo.git_branch.clone(),
        push_token: None,
        message_prefix: None,
        trunk_path: repo.svn_branch.clone(),
    };
    let outcome = reposync_core::import::run_full_import(
        &svn,
        &git,
        &mapper,
        db,
        &policy,
        &config,
        ImportRunState {
            progress: Arc::new(tokio::sync::RwLock::new(ImportProgress::default())),
            ws_broadcast: None,
            repo_id: Some(manifest.str("repo_id").to_string()),
            operation_id: Some(manifest.str("operation_id").to_string()),
            cancel_signal: None,
        },
    )
    .await
    .unwrap_or_else(|error| panic!("{label} resume import failed: {error:#}"));
    match outcome {
        ImportOutcome::ReconciliationRequired { commits, reason } => {
            assert_eq!(commits, 0, "{label}");
            assert!(
                reason.contains("svn_identity_unproved"),
                "{label} held for a different reason: {reason}"
            );
        }
        other => panic!("{label} produced {other:?}"),
    }
}

async fn phase(manifest: &Manifest, label: &str) {
    require_raw_seals(manifest, &format!("{label} before"))
        .unwrap_or_else(|error| panic!("{label} seal: {error}"));
    let db = Database::new(manifest.path("db")).unwrap();
    db.initialize().unwrap();
    let repo = db.get_repository(manifest.str("repo_id")).unwrap().unwrap();
    let workdir = manifest.path("workdir");
    let fingerprint = import_target_fingerprint(&repo, &workdir);
    assert_eq!(
        fingerprint,
        manifest.str("fingerprint"),
        "{label} fingerprint"
    );
    let before = effects(&db, manifest);
    assert_eq!(before.git_head, manifest.str("intended_sha"));
    assert_eq!(before.bare_head, manifest.str("intended_sha"));
    assert_eq!(before.watermark_rev, 0);
    assert!(before.watermark_sha.is_empty());
    assert!(before.max_rev <= 50);
    let reconciled =
        apply_import_reconciliation(&db, &repo, manifest.str("operation_id"), &workdir)
            .await
            .unwrap_or_else(|error| panic!("{label} reconciliation failed: {error}"));
    assert!(!reconciled.finalized, "{label} completed the baseline");
    assert!(
        reconciled.resume_authorized,
        "{label} did not authorize resume"
    );
    assert!(reconciled.publication_recorded, "{label}");
    assert_eq!(
        reconciled.observed_sha.as_deref(),
        Some(manifest.str("intended_sha"))
    );
    assert_eq!(reconciled.operation.processed_revisions, 50);
    assert_eq!(reconciled.operation.total_revisions, Some(51));
    let (rev, sha) = db.get_repo_watermark(manifest.str("repo_id")).unwrap();
    assert_eq!(rev, 0, "{label} advanced the repository checkpoint");
    assert!(sha.is_empty());
    db.resume_import_operation(manifest.str("repo_id"), manifest.str("operation_id"))
        .unwrap_or_else(|error| panic!("{label} authorized resume was refused: {error}"));
    refuse_resume(&db, manifest, label).await;
    let after = effects(&db, manifest);
    assert_eq!(after.git_head, before.git_head, "{label}");
    assert_eq!(after.bare_head, before.bare_head, "{label}");
    assert_eq!(after.maps, before.maps, "{label}");
    assert_eq!(after.max_rev, before.max_rev, "{label}");
    assert_eq!(after.applied, before.applied, "{label}");
    assert_eq!(after.watermark_rev, 0);
    assert!(
        independently_established_svn_uuid(&db.conn(), manifest.str("repo_id"))
            .unwrap()
            .is_none(),
        "{label} recorded an SVN UUID"
    );
    drop(db);
    require_raw_seals(manifest, &format!("{label} before reopen"))
        .unwrap_or_else(|error| panic!("{label} seal before reopen: {error}"));

    let db = Database::new(manifest.path("db")).unwrap();
    db.initialize().unwrap();
    let reopened = db
        .get_import_operation(manifest.str("repo_id"), manifest.str("operation_id"))
        .unwrap()
        .unwrap();
    assert!(
        reopened
            .outcome_detail
            .unwrap_or_default()
            .contains("svn_identity_unproved"),
        "{label} reopen lost the hold"
    );
    let second_resume =
        db.resume_import_operation(manifest.str("repo_id"), manifest.str("operation_id"));
    assert!(
        second_resume.is_err(),
        "{label} reopen authorized another resume"
    );
    refuse_resume(&db, manifest, &format!("{label} reopen")).await;
    let reopened_effects = effects(&db, manifest);
    assert_eq!(reopened_effects.git_head, before.git_head);
    assert_eq!(reopened_effects.bare_head, before.bare_head);
    assert_eq!(reopened_effects.maps, before.maps);
    assert_eq!(reopened_effects.max_rev, before.max_rev);
    assert_eq!(reopened_effects.applied, before.applied);
    assert!(
        independently_established_svn_uuid(&db.conn(), manifest.str("repo_id"))
            .unwrap()
            .is_none()
    );
    let key = independently_established_svn_uuid_kv_key(manifest.str("repo_id"));
    let stored: Option<String> = db
        .conn()
        .query_row("SELECT value FROM kv_state WHERE key=?1", [key], |row| {
            row.get(0)
        })
        .ok();
    assert!(stored.is_none());
    drop(db);
    require_raw_seals(manifest, &format!("{label} after reopen"))
        .unwrap_or_else(|error| panic!("{label} seal after reopen: {error}"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rs63_authentic_predecessor_partial_identity_hold() {
    let manifest = Manifest::load_admitted();
    assert_eq!(manifest.u64("processed_revisions"), 50);
    assert_eq!(manifest.u64("total_revisions"), 51);
    assert_eq!(manifest.u64("local_commits"), 50);
    assert_eq!(
        manifest.str("producer_commit"),
        "54e183484961bd9ad12bd03505b3b00610de7cd9"
    );
    let sealed_root = manifest.str("root").to_string();
    let sealed_fingerprint = manifest.str("fingerprint").to_string();
    park_and_restore(&manifest, "before-unchanged")
        .unwrap_or_else(|error| panic!("direct restore: {error}"));
    assert_eq!(manifest.str("root"), sealed_root);
    assert_eq!(manifest.str("fingerprint"), sealed_fingerprint);
    phase(&manifest, "unchanged").await;
    park_and_restore(&manifest, "before-replacement")
        .unwrap_or_else(|error| panic!("reused restore: {error}"));
    assert_eq!(manifest.str("root"), sealed_root);
    assert_eq!(manifest.str("fingerprint"), sealed_fingerprint);
    replace_svn_same_url(&manifest);
    phase(&manifest, "replacement").await;
    let (head, bare) = git_ref(&manifest);
    assert_eq!(head, manifest.str("intended_sha"));
    assert_eq!(bare, manifest.str("intended_sha"));
    let receipt = serde_json::json!({
        "receipt": "RS63_PREDECESSOR_PARTIAL_IMPORT_HOLD",
        "producer_commit": manifest.str("producer_commit"),
        "repo_id": manifest.str("repo_id"),
        "operation_id": manifest.str("operation_id"),
        "root": manifest.str("root"),
        "fingerprint": manifest.str("fingerprint"),
        "intended_sha": manifest.str("intended_sha"),
        "processed_revisions": 50,
        "total_revisions": 51,
        "local_commits": 50,
        "unchanged_hold": "svn_identity_unproved",
        "replacement_hold": "svn_identity_unproved",
        "reopen": "both phases",
        "new_publication": false,
        "new_uuid_authority": false,
        "new_applied_checkpoint": false,
        "working_root_restored_before_each_phase": true,
        "attempt_id": manifest.str("attempt_id"),
        "sealed_sources_raw_only": true,
    });
    let receipt_path = manifest
        .path("attempt_root")
        .join("RS63_PREDECESSOR_PARTIAL_IMPORT_HOLD.json");
    let mut receipt_file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&receipt_path)
        .unwrap_or_else(|error| panic!("exclusive receipt: {error}"));
    std::io::Write::write_all(
        &mut receipt_file,
        &serde_json::to_vec_pretty(&receipt).unwrap(),
    )
    .unwrap();
}

#[cfg(unix)]
#[test]
fn rs63_restore_verifier_rejects_modified_and_extra_members() {
    use std::os::unix::fs::{symlink, PermissionsExt};

    let scratch = std::env::temp_dir().join(format!(
        "rs63-restore-verifier-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&scratch).unwrap();
    let outside = scratch.join("outside");
    let master = scratch.join("master");
    let root = scratch.join("root");
    std::fs::create_dir_all(outside.join("nested")).unwrap();
    std::fs::write(outside.join("nested").join("sentinel"), b"outside").unwrap();
    std::fs::create_dir_all(&master).unwrap();
    std::fs::write(master.join("keep.txt"), b"sealed\n").unwrap();
    let (bytes, sha) = sha256_file(&master.join("keep.txt"));
    let mode =
        unix_metadata_mode(&std::fs::symlink_metadata(master.join("keep.txt")).unwrap()).unwrap();
    let members = vec![serde_json::json!({
        "path": "keep.txt",
        "kind": "file",
        "bytes": bytes,
        "mode": mode,
        "sha256": sha,
    })];
    std::fs::create_dir_all(&root).unwrap();
    restore_exact_root(&root, &master, &members).unwrap();
    assert_eq!(std::fs::read(root.join("keep.txt")).unwrap(), b"sealed\n");

    std::fs::write(root.join("keep.txt"), b"mutated\n").unwrap();
    let modified = verify_sealed_tree(&root, &members, "working root").unwrap_err();
    assert!(modified.contains("changed content"), "{modified}");

    restore_exact_root(&root, &master, &members).unwrap();
    assert_eq!(std::fs::read(root.join("keep.txt")).unwrap(), b"sealed\n");
    std::fs::write(root.join("extra.txt"), b"extra\n").unwrap();
    let extra = verify_sealed_tree(&root, &members, "working root").unwrap_err();
    assert!(extra.contains("unexpected extra entry"), "{extra}");

    restore_exact_root(&root, &master, &members).unwrap();
    let keep = root.join("keep.txt");
    assert!(keep.starts_with(&scratch), "{}", keep.display());
    let recorded_mode = unix_metadata_mode(&std::fs::symlink_metadata(&keep).unwrap()).unwrap();
    assert_eq!(recorded_mode, mode);
    // Owner execute is independent of the creating umask. umask 077 already yields 0600.
    let distinct_mode = recorded_mode ^ 0o100;
    assert_eq!(distinct_mode ^ recorded_mode, 0o100);
    let mut permissions = std::fs::metadata(&keep).unwrap().permissions();
    permissions.set_mode(distinct_mode);
    std::fs::set_permissions(&keep, permissions).unwrap();
    let actual_mode = unix_metadata_mode(&std::fs::symlink_metadata(&keep).unwrap()).unwrap();
    assert_ne!(actual_mode, recorded_mode);
    assert_eq!(actual_mode ^ recorded_mode, 0o100);
    let mode_error = verify_sealed_tree(&root, &members, "working root").unwrap_err();
    assert!(mode_error.contains("changed mode"), "{mode_error}");
    let mut restored_permissions = std::fs::metadata(&keep).unwrap().permissions();
    restored_permissions.set_mode(recorded_mode);
    std::fs::set_permissions(&keep, restored_permissions).unwrap();
    assert_eq!(
        unix_metadata_mode(&std::fs::symlink_metadata(&keep).unwrap()).unwrap(),
        recorded_mode
    );

    restore_exact_root(&root, &master, &members).unwrap();
    std::fs::remove_file(root.join("keep.txt")).unwrap();
    std::fs::create_dir(root.join("keep.txt")).unwrap();
    let type_error = verify_sealed_tree(&root, &members, "working root").unwrap_err();
    assert!(type_error.contains("changed type"), "{type_error}");

    restore_exact_root(&root, &master, &members).unwrap();
    symlink(outside.join("nested"), root.join("escape")).unwrap();
    clear_owned_children(&root).unwrap();
    assert!(!root.join("escape").exists());
    assert!(!root.join("keep.txt").exists());
    assert_eq!(
        std::fs::read(outside.join("nested").join("sentinel")).unwrap(),
        b"outside"
    );
    std::fs::remove_dir_all(&scratch).unwrap();
}

fn canary_dir(label: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "rs63-canary-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&path).unwrap();
    path
}

fn write_manifest(path: &Path, value: &serde_json::Value) -> String {
    let bytes = serde_json::to_vec_pretty(value).unwrap();
    std::fs::write(path, &bytes).unwrap();
    sha256_bytes(&bytes)
}

#[test]
fn rs63_manifest_refuses_wrong_sha256() {
    let scratch = canary_dir("sha");
    let manifest_path = scratch.join("manifest.json");
    std::fs::write(&manifest_path, b"{}\n").unwrap();
    let error = admit_manifest(
        &manifest_path,
        "0000000000000000000000000000000000000000000000000000000000000000",
        "fresh",
        &scratch,
    )
    .unwrap_err();
    assert!(error.contains("sha256"), "{error}");
    std::fs::remove_dir_all(&scratch).unwrap();
}

#[test]
fn rs63_manifest_refuses_wrong_attempt_identity() {
    let scratch = canary_dir("identity");
    let manifest_path = scratch.join("manifest.json");
    let sha = write_manifest(
        &manifest_path,
        &serde_json::json!({
            "attempt_id": "other-attempt",
            "producer_commit": "54e183484961bd9ad12bd03505b3b00610de7cd9"
        }),
    );
    let error = admit_manifest(&manifest_path, &sha, "fresh-attempt", &scratch).unwrap_err();
    assert!(error.contains("attempt id"), "{error}");
    std::fs::remove_dir_all(&scratch).unwrap();
}

#[test]
fn rs63_manifest_refuses_foreign_root() {
    let scratch = canary_dir("foreign");
    let attempt = scratch.join("attempt");
    let outside = scratch.join("outside");
    let master = attempt.join("master");
    let snapshot = attempt.join("snapshot");
    for dir in [&attempt, &outside, &master, &snapshot] {
        std::fs::create_dir(dir).unwrap();
    }
    std::fs::write(outside.join("keep.txt"), b"sealed\n").unwrap();
    let (bytes, sha) = sha256_file(&outside.join("keep.txt"));
    let mode =
        unix_metadata_mode(&std::fs::symlink_metadata(outside.join("keep.txt")).unwrap()).unwrap();
    let manifest_path = scratch.join("manifest.json");
    let manifest_sha = write_manifest(
        &manifest_path,
        &serde_json::json!({
            "attempt_id": "fresh-attempt",
            "producer_commit": "54e183484961bd9ad12bd03505b3b00610de7cd9",
            "attempt_root": attempt,
            "root": outside,
            "master": master,
            "snapshot": snapshot,
            "members": [{
                "path": "keep.txt",
                "kind": "file",
                "bytes": bytes,
                "mode": mode,
                "sha256": sha
            }]
        }),
    );
    let error =
        admit_manifest(&manifest_path, &manifest_sha, "fresh-attempt", &attempt).unwrap_err();
    assert!(error.contains("foreign"), "{error}");
    std::fs::remove_dir_all(&scratch).unwrap();
}

#[test]
fn rs63_manifest_refuses_corrupt_sealed_member() {
    let scratch = canary_dir("corrupt");
    let attempt = scratch.join("attempt");
    let live = attempt.join("live");
    let master = attempt.join("master");
    let snapshot = attempt.join("snapshot");
    std::fs::create_dir_all(&live).unwrap();
    std::fs::write(live.join("keep.txt"), b"sealed\n").unwrap();
    copy_tree_independent(&live, &master).unwrap();
    copy_tree_independent(&live, &snapshot).unwrap();
    let mode =
        unix_metadata_mode(&std::fs::symlink_metadata(master.join("keep.txt")).unwrap()).unwrap();
    let manifest_path = scratch.join("manifest.json");
    let manifest_sha = write_manifest(
        &manifest_path,
        &serde_json::json!({
            "attempt_id": "fresh-attempt",
            "producer_commit": "54e183484961bd9ad12bd03505b3b00610de7cd9",
            "attempt_root": attempt,
            "root": live,
            "master": master,
            "snapshot": snapshot,
            "members": [{
                "path": "keep.txt",
                "kind": "file",
                "bytes": 7,
                "mode": mode,
                "sha256": "0000000000000000000000000000000000000000000000000000000000000000"
            }]
        }),
    );
    let error =
        admit_manifest(&manifest_path, &manifest_sha, "fresh-attempt", &attempt).unwrap_err();
    assert!(error.contains("changed content"), "{error}");
    assert_eq!(std::fs::read(master.join("keep.txt")).unwrap(), b"sealed\n");
    std::fs::remove_dir_all(&scratch).unwrap();
}

#[test]
fn rs63_admission_refuses_preserved_failure_root() {
    let error =
        touches_preserved(Path::new("/tmp/rs63-predecessor-partial-import-hold")).unwrap_err();
    assert!(error.contains("preserved failure"), "{error}");
    let error = touches_preserved(Path::new("/private/tmp/.tmpRJUzYe")).unwrap_err();
    assert!(error.contains("preserved failure"), "{error}");
}

#[cfg(unix)]
#[test]
fn rs63_copy_refuses_shared_inode_alias() {
    let scratch = canary_dir("alias");
    let source = scratch.join("source");
    let copy = scratch.join("copy");
    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir(&copy).unwrap();
    let original = source.join("keep.txt");
    std::fs::write(&original, b"sealed\n").unwrap();
    std::fs::hard_link(&original, copy.join("keep.txt")).unwrap();
    let error = assert_distinct_file_inodes(&source, &copy).unwrap_err();
    assert!(error.contains("inode alias"), "{error}");
    std::fs::remove_dir_all(&scratch).unwrap();
}

#[cfg(unix)]
#[test]
fn rs63_independent_copy_semantic_read_closes_and_preserves_source() {
    let scratch = canary_dir("semantic");
    let source = scratch.join("sealed");
    std::fs::create_dir(&source).unwrap();
    let db = source.join("sample.db");
    let created = Command::new("sqlite3")
        .arg(&db)
        .arg("PRAGMA journal_mode=WAL; CREATE TABLE sample(v INTEGER); INSERT INTO sample VALUES (7);")
        .status()
        .unwrap();
    assert!(created.success());
    let before = std::fs::read_dir(&source)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    let mut before_hashes = std::collections::BTreeMap::new();
    for path in &before {
        if path.is_file() {
            before_hashes.insert(path.clone(), sha256_file(path));
        }
    }
    let copy = scratch.join("diagnostic");
    copy_tree_independent(&source, &copy).unwrap();
    assert_distinct_file_inodes(&source, &copy).unwrap();
    let copy_db = copy.join("sample.db");
    assert!(copy_db.starts_with(&copy));
    assert!(!copy_db.starts_with("/tmp/rs63-predecessor-partial-import-hold"));
    let queried = Command::new("sqlite3")
        .arg(&copy_db)
        .arg("SELECT v FROM sample;")
        .output()
        .unwrap();
    assert!(
        queried.status.success(),
        "{}",
        String::from_utf8_lossy(&queried.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&queried.stdout).trim(), "7");
    for (path, prior) in before_hashes {
        assert_eq!(sha256_file(&path), prior, "{}", path.display());
    }
    std::fs::remove_dir_all(&scratch).unwrap();
}
