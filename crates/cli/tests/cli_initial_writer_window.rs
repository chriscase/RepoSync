#[path = "../../personal/tests/support/initial_writer_window.rs"]
mod initial_writer_window;

#[test]
fn cli_import_holds_exclusion_before_clone_and_existing_checkout_authentication() {
    initial_writer_window::assert_startup_window(
        env!("CARGO_BIN_EXE_reposync"),
        &["personal", "--personal-config"],
        &[&["import", "--full"], &["import", "--snapshot"]],
    );
}
