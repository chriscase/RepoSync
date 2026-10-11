#[path = "../../personal/tests/support/fresh_clone.rs"]
mod fresh_clone;

#[test]
fn cli_import_respects_fresh_clone_revocation() {
    fresh_clone::assert_fresh_clone_revocation(
        env!("CARGO_BIN_EXE_reposync"),
        &["personal", "--personal-config"],
        "import",
    );
}
