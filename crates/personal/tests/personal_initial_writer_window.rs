#[path = "support/initial_writer_window.rs"]
mod initial_writer_window;

#[test]
fn personal_import_holds_exclusion_before_clone_and_existing_checkout_authentication() {
    initial_writer_window::assert_startup_window(
        env!("CARGO_BIN_EXE_reposync-personal"),
        &["--config"],
        &[&["import", "--full"], &["import", "--snapshot"]],
    );
}

#[test]
fn personal_sync_holds_exclusion_before_clone_and_existing_checkout_authentication() {
    initial_writer_window::assert_startup_window(
        env!("CARGO_BIN_EXE_reposync-personal"),
        &["--config"],
        &[&["sync"]],
    );
}
