use workspace::{
    app_support::{support_for_window, SupportLevel, KNOWN_APPS},
    model::WorkspaceSnapshot,
};

#[test]
fn common_apps_fixture_loads_and_matches_policy() {
    let snapshot: WorkspaceSnapshot =
        serde_json::from_str(include_str!("fixtures/common_apps.workspace.json")).unwrap();

    let supported: Vec<_> = snapshot
        .windows
        .iter()
        .filter(|window| support_for_window(window).level == SupportLevel::FullRestore)
        .collect();
    let supported_bundle_ids = supported
        .iter()
        .filter_map(|window| window.bundle_id.as_deref())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(supported_bundle_ids.len(), KNOWN_APPS.len());

    for app in KNOWN_APPS {
        assert!(
            snapshot
                .windows
                .iter()
                .any(|window| window.bundle_id.as_deref() == Some(app.bundle_id)
                    && support_for_window(window).level == SupportLevel::FullRestore),
            "fixture should include supported window for {}",
            app.bundle_id
        );
    }

    let chrome_windows: Vec<_> = snapshot
        .windows
        .iter()
        .filter(|window| window.bundle_id.as_deref() == Some("com.google.Chrome"))
        .collect();
    assert!(chrome_windows.len() >= 3);
    assert!(chrome_windows
        .iter()
        .all(|window| !window.browser_tabs.is_empty()));
    assert!(chrome_windows
        .iter()
        .flat_map(|window| &window.browser_tabs)
        .any(|tab| tab.url == "https://www.rust-lang.org/"));
}
