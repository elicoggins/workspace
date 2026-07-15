use chrono::{TimeZone, Utc};
use workspace::{
    app_support::KNOWN_APPS,
    model::{
        DisplaySnapshot, Frame, HostInfo, RelativeFrame, RestoreStatus, WindowSnapshot,
        WorkspaceSnapshot, SNAPSHOT_VERSION,
    },
    restore::{restore_workspace, RestoreOptions},
};

fn display() -> DisplaySnapshot {
    DisplaySnapshot {
        id: "cgdisplay-1".to_string(),
        numeric_id: 1,
        name: Some("Built-in Display".to_string()),
        frame: Frame {
            x: 0.0,
            y: 0.0,
            width: 1470.0,
            height: 956.0,
        },
        scale_factor: 2.0,
        is_primary: true,
    }
}

fn window(bundle_id: Option<&str>, app_name: &str, z_order: u32) -> WindowSnapshot {
    WindowSnapshot {
        window_id: z_order + 100,
        app_name: app_name.to_string(),
        process_name: app_name.to_string(),
        bundle_id: bundle_id.map(str::to_string),
        pid: z_order as i32 + 1000,
        title: Some(format!("{app_name} window")),
        frame: Frame {
            x: 0.0,
            y: 33.0,
            width: 1000.0,
            height: 700.0,
        },
        display_id: Some("cgdisplay-1".to_string()),
        display_frame: Some(display().frame),
        display_relative_frame: Some(RelativeFrame {
            x: 0.0,
            y: 33.0 / 956.0,
            width: 1000.0 / 1470.0,
            height: 700.0 / 956.0,
        }),
        z_order: Some(z_order),
        fullscreen: false,
        minimized: false,
        enabled: true,
        browser_tabs: Vec::new(),
    }
}

fn snapshot(windows: Vec<WindowSnapshot>) -> WorkspaceSnapshot {
    WorkspaceSnapshot {
        version: SNAPSHOT_VERSION,
        name: "policy".to_string(),
        created_at: Utc.with_ymd_and_hms(2026, 5, 12, 15, 30, 0).unwrap(),
        host: HostInfo {
            hostname: "macbook-pro".to_string(),
            os: "macos".to_string(),
            arch: "aarch64".to_string(),
        },
        displays: vec![display()],
        windows,
    }
}

#[test]
fn dry_run_plans_supported_windows_and_skips_unknown_windows() {
    let report = restore_workspace(
        &snapshot(vec![
            window(Some("com.microsoft.VSCode"), "Code", 0),
            window(Some("com.google.Chrome"), "Google Chrome", 1),
            window(Some("com.apple.Terminal"), "Terminal", 2),
            window(None, "Unknown", 3),
        ]),
        RestoreOptions {
            mode: Default::default(),
            dry_run: true,
            dev_mode: false,
        },
    )
    .unwrap();

    assert_eq!(report.restored, 0);
    assert_eq!(report.skipped, 1);
    assert_eq!(report.failed, 0);
    assert_eq!(report.actions.len(), 4);
    assert_eq!(report.actions[0].status, RestoreStatus::Planned);
    assert_eq!(
        report.actions[0].bundle_id.as_deref(),
        Some("com.microsoft.VSCode")
    );
    assert_eq!(report.actions[1].status, RestoreStatus::Planned);
    assert_eq!(report.actions[2].status, RestoreStatus::Planned);
    assert_eq!(report.actions[3].status, RestoreStatus::Skipped);
    assert!(report.actions[3]
        .message
        .as_deref()
        .unwrap_or_default()
        .contains("without bundle identifiers"));
}

#[test]
fn dry_run_plans_every_known_app() {
    let windows = KNOWN_APPS
        .iter()
        .enumerate()
        .map(|(index, app)| window(Some(app.bundle_id), app.name, index as u32))
        .collect();

    let report = restore_workspace(
        &snapshot(windows),
        RestoreOptions {
            mode: Default::default(),
            dry_run: true,
            dev_mode: false,
        },
    )
    .unwrap();

    assert_eq!(report.restored, 0);
    assert_eq!(report.skipped, 0);
    assert_eq!(report.failed, 0);
    assert_eq!(report.actions.len(), KNOWN_APPS.len());
    for (action, app) in report.actions.iter().zip(KNOWN_APPS) {
        assert_eq!(action.status, RestoreStatus::Planned, "{}", app.bundle_id);
        assert_eq!(action.bundle_id.as_deref(), Some(app.bundle_id));
    }
}

#[test]
fn dry_run_plans_multiple_windows_for_every_known_app() {
    let windows = KNOWN_APPS
        .iter()
        .enumerate()
        .flat_map(|(index, app)| {
            [
                window(Some(app.bundle_id), app.name, (index * 2) as u32),
                window(Some(app.bundle_id), app.name, (index * 2 + 1) as u32),
            ]
        })
        .collect::<Vec<_>>();
    let expected_count = windows.len();

    let report = restore_workspace(
        &snapshot(windows),
        RestoreOptions {
            mode: Default::default(),
            dry_run: true,
            dev_mode: false,
        },
    )
    .unwrap();

    assert_eq!(report.restored, 0);
    assert_eq!(report.skipped, 0);
    assert_eq!(report.failed, 0);
    assert_eq!(report.actions.len(), expected_count);
    assert!(report
        .actions
        .iter()
        .all(|action| action.status == RestoreStatus::Planned));
}

#[test]
fn fullscreen_supported_windows_are_still_skipped() {
    let mut vscode = window(Some("com.microsoft.VSCode"), "Code", 0);
    vscode.fullscreen = true;

    let report = restore_workspace(
        &snapshot(vec![vscode]),
        RestoreOptions {
            mode: Default::default(),
            dry_run: true,
            dev_mode: false,
        },
    )
    .unwrap();

    assert_eq!(report.actions[0].status, RestoreStatus::Skipped);
    assert!(report.actions[0]
        .message
        .as_deref()
        .unwrap_or_default()
        .contains("fullscreen"));
}

#[test]
fn disabled_windows_are_skipped_before_restore() {
    let mut vscode = window(Some("com.microsoft.VSCode"), "Code", 0);
    vscode.enabled = false;

    let report = restore_workspace(
        &snapshot(vec![vscode]),
        RestoreOptions {
            mode: Default::default(),
            dry_run: true,
            dev_mode: false,
        },
    )
    .unwrap();

    assert_eq!(report.actions[0].status, RestoreStatus::Skipped);
    assert!(report.actions[0]
        .message
        .as_deref()
        .unwrap_or_default()
        .contains("disabled"));
}
