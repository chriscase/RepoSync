// Installs a repository hook for a fixture.
//
// The isolated qualification image mounts the writable fixture noexec, so
// neither Git nor Subversion can run a hook written there. The image instead
// carries a read-only copy of each reviewed script (scripts/reliability-hooks)
// and names that directory in REPOSYNC_FIXTURE_HOOKS. In that lane the hook
// path inside the fixture repository is a symbolic link to the image's copy,
// after its bytes are compared with the script the test names. Anything the
// hook reads or writes stays in the fixture. Outside that lane the script is
// written into the repository as before.
#![allow(dead_code)]

use std::path::Path;

/// Installs a script as `<repository>/hooks/<hook>`. The script is its
/// directory name under scripts/reliability-hooks and its exact bytes.
pub fn install(repository: &Path, hook: &str, (name, body): (&str, &str)) {
    use std::os::unix::fs::PermissionsExt;
    let path = repository.join("hooks").join(hook);
    match std::fs::symlink_metadata(&path) {
        Ok(_) => std::fs::remove_file(&path).unwrap(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => panic!("could not inspect {}: {error}", path.display()),
    }
    match std::env::var_os("REPOSYNC_FIXTURE_HOOKS") {
        Some(root) => {
            let image = Path::new(&root).join(name).join(hook);
            let bytes = std::fs::read(&image)
                .unwrap_or_else(|error| panic!("image-owned {name}/{hook} is missing: {error}"));
            assert_eq!(
                bytes,
                body.as_bytes(),
                "image-owned {name}/{hook} differs from the test's script"
            );
            std::os::unix::fs::symlink(&image, &path).unwrap();
        }
        None => {
            std::fs::write(&path, body).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
}

/// The reviewed scripts. Each constant is `(directory under
/// scripts/reliability-hooks, script bytes)`; the image's copy must match.
pub mod script {
    pub const COUNT_REJECT: (&str, &str) = (
        "count-reject",
        r#"#!/bin/sh
printf x >> attempts
exit 1
"#,
    );
    pub const COUNT_HOLD: (&str, &str) = (
        "count-hold",
        r#"#!/bin/sh
printf x >> attempts
: > ready
count=0
while [ ! -f release ]; do
  count=$((count + 1))
  [ "$count" -lt 1000 ] || exit 1
  sleep 0.01
done
exit 0
"#,
    );
    pub const REF_MOVE: (&str, &str) = (
        "ref-move",
        r#"#!/bin/sh
env -u GIT_QUARANTINE_PATH git update-ref refs/heads/child "$(cat hooks/ref-move-target)"
"#,
    );
    pub const REJECT: (&str, &str) = (
        "reject",
        r#"#!/bin/sh
exit 1
"#,
    );
    pub const WITNESS_REJECT: (&str, &str) = (
        "witness-reject",
        r#"#!/bin/sh
printf 'ran\n' > hooks/witness
exit 1
"#,
    );
    pub const PROTECTED_REF: (&str, &str) = (
        "protected-ref",
        r#"#!/bin/sh
printf '%s\n' 'disposable protected ref refusal' >&2
exit 1
"#,
    );
    pub const SVN_PERMISSION_REFUSAL: (&str, &str) = (
        "svn-permission-refusal",
        r#"#!/bin/sh
printf '%s\n' 'disposable SVN permission refusal' >&2
exit 1
"#,
    );
    pub const SVN_REJECT_AFTER_GIT: (&str, &str) = (
        "svn-reject-after-git",
        r#"#!/bin/sh
echo 'synthetic rejection after Git success' >&2
exit 1
"#,
    );
    pub const SVN_HOLD_HTTP_DISCONNECT: (&str, &str) = (
        "svn-hold-http-disconnect",
        r#"#!/bin/sh
# Subversion passes the repository path; the control files sit beside it.
root=$(dirname "$1")
printf ready > "$root/http-disconnect-ready"
attempt=0
while [ "$attempt" -lt 600 ]; do
    [ -f "$root/http-disconnect-release" ] && exit 0
    sleep 0.05
    attempt=$((attempt + 1))
done
exit 1
"#,
    );
    pub const SVN_HOLD_SHUTDOWN: (&str, &str) = (
        "svn-hold-shutdown",
        r#"#!/bin/sh
# Subversion passes the repository path; the control files sit beside it.
root=$(dirname "$1")
printf ready > "$root/shutdown-ready"
attempt=0
while [ "$attempt" -lt 600 ]; do
    [ -f "$root/shutdown-release" ] && exit 0
    sleep 0.05
    attempt=$((attempt + 1))
done
exit 1
"#,
    );
    // Git invokes pre-receive in its bare repository before changing the refs.
    // Publish the complete offer atomically and keep the push open until the
    // test releases it. The hook also exits if the test dies.
    pub const R17_HOLD: (&str, &str) = (
        "r17-hold",
        r#"#!/bin/sh
set -eu
cat > offer.tmp
mv offer.tmp offer
attempt=0
while [ ! -f release ]; do
    [ -f hooks/pre-receive ] || exit 1
    if [ "$attempt" -ge 600 ]; then
        echo 'timed out waiting for both R17 pushes' >&2
        exit 1
    fi
    sleep 0.05
    attempt=$((attempt + 1))
done
"#,
    );
    // The same hold, then a rejection that names the repository. The name comes
    // from `hooks/r17-reject-id` so the script itself stays fixed.
    pub const R17_HOLD_REJECT: (&str, &str) = (
        "r17-hold-reject",
        r#"#!/bin/sh
set -eu
cat > offer.tmp
mv offer.tmp offer
attempt=0
while [ ! -f release ]; do
    [ -f hooks/pre-receive ] || exit 1
    if [ "$attempt" -ge 600 ]; then
        echo 'timed out waiting for both R17 pushes' >&2
        exit 1
    fi
    sleep 0.05
    attempt=$((attempt + 1))
done
echo "rejected by $(cat hooks/r17-reject-id)" >&2
exit 1
"#,
    );
}
