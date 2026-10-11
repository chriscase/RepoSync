#[path = "support/fresh_clone.rs"]
mod fresh_clone;

#[test]
fn personal_import_respects_fresh_clone_revocation() {
    fresh_clone::assert_fresh_clone_revocation(
        env!("CARGO_BIN_EXE_reposync-personal"),
        &["--config"],
        "import",
    );
}

#[test]
fn personal_sync_respects_fresh_clone_revocation() {
    fresh_clone::assert_fresh_clone_revocation(
        env!("CARGO_BIN_EXE_reposync-personal"),
        &["--config"],
        "sync",
    );
}
