//! Static UI contract checks for #65 RS-16 disable vs remove vs remote deletion.

#[test]
fn repo_detail_ui_distinguishes_pause_disable_and_managed_remove() {
    let src = include_str!("../../../web-ui/src/pages/RepoDetail.tsx");
    assert!(
        src.contains("data-testid=\"pause-disable-repo\""),
        "Pause/disable control must be explicit"
    );
    assert!(
        src.contains("data-testid=\"remove-from-reposync\""),
        "Managed removal must be separate from disable"
    );
    assert!(
        src.contains("api.disableRepo"),
        "UI must call explicit disable API"
    );
    assert!(
        src.contains("api.removeManagedRepo"),
        "UI must call managed removal API"
    );
    assert!(
        !src.contains("Permanently remove this repository configuration"),
        "Root delete copy must not promise permanent removal while disabling"
    );
    assert!(
        src.contains("delete_git: false, delete_svn: false"),
        "Branch remote deletion must default to false in UI"
    );
    assert!(
        src.contains("ManagedRemovalPanel"),
        "Managed removal states must render in the detail view"
    );
    let panel = include_str!("../../../web-ui/src/components/ManagedRemovalPanel.tsx");
    assert!(
        panel.contains("data-testid=\"managed-removal-panel\""),
        "Managed removal panel must expose state for tests"
    );
    assert!(
        panel.contains("Retry only retries owned local cleanup"),
        "Retry copy must not imply remote deletion"
    );
}
