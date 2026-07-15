//! Restore-policy behavior through the planner: which saved windows are
//! planned for restore and which are skipped, and why.

use chrono::{TimeZone, Utc};
use workspace::{
    app_support::KNOWN_APPS,
    model::{
        DisplaySnapshot, Frame, HostInfo, RelativeFrame, WindowSnapshot, WorkspaceSnapshot,
        SNAPSHOT_VERSION,
    },
    plan::{plan_restore, OperationKind, PlanOptions, WorldState},
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

fn plan_for(snap: &WorkspaceSnapshot) -> workspace::plan::RestorePlan {
    let frames: Vec<Frame> = snap.windows.iter().map(|w| w.frame).collect();
    plan_restore(
        snap,
        &WorldState::default(),
        PlanOptions::default(),
        &frames,
    )
}

fn is_actionable(kind: &OperationKind) -> bool {
    !matches!(kind, OperationKind::Skip { .. })
}

#[test]
fn plans_supported_windows_and_skips_unknown_windows() {
    let snap = snapshot(vec![
        window(Some("com.microsoft.VSCode"), "Code", 0),
        window(Some("com.google.Chrome"), "Google Chrome", 1),
        window(Some("com.apple.Terminal"), "Terminal", 2),
        window(None, "Unknown", 3),
    ]);

    let plan = plan_for(&snap);

    // Every supported window gets an actionable op; the unknown window is
    // skipped with an explanatory reason.
    for bundle in [
        "com.microsoft.VSCode",
        "com.google.Chrome",
        "com.apple.Terminal",
    ] {
        assert!(
            plan.operations
                .iter()
                .any(|op| op.bundle_id.as_deref() == Some(bundle) && is_actionable(&op.kind)),
            "expected actionable op for {bundle}"
        );
    }
    let skip = plan
        .operations
        .iter()
        .find(|op| matches!(&op.kind, OperationKind::Skip { .. }) && op.bundle_id.is_none())
        .expect("unknown window should be skipped");
    assert!(skip.rationale.contains("without bundle identifiers"));
}

#[test]
fn plans_every_known_app() {
    let windows = KNOWN_APPS
        .iter()
        .enumerate()
        .map(|(index, app)| window(Some(app.bundle_id), app.name, index as u32))
        .collect();

    let plan = plan_for(&snapshot(windows));

    for app in KNOWN_APPS {
        assert!(
            plan.operations
                .iter()
                .any(|op| op.bundle_id.as_deref() == Some(app.bundle_id)
                    && is_actionable(&op.kind)),
            "{} should be planned for restore",
            app.bundle_id
        );
    }
    assert!(
        !plan
            .operations
            .iter()
            .any(|op| matches!(op.kind, OperationKind::Skip { .. })),
        "no known app should be skipped"
    );
}

#[test]
fn plans_multiple_windows_for_every_known_app() {
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
    let window_count = windows.len();

    let plan = plan_for(&snapshot(windows));

    // Every saved window is covered by an op carrying its index.
    let covered: std::collections::HashSet<usize> = plan
        .operations
        .iter()
        .filter(|op| is_actionable(&op.kind))
        .filter_map(|op| op.saved_window_index)
        .collect();
    assert_eq!(covered.len(), window_count);
}

#[test]
fn fullscreen_supported_windows_are_still_skipped() {
    let mut vscode = window(Some("com.microsoft.VSCode"), "Code", 0);
    vscode.fullscreen = true;

    let plan = plan_for(&snapshot(vec![vscode]));

    assert!(matches!(
        plan.operations[0].kind,
        OperationKind::Skip { .. }
    ));
    assert!(plan.operations[0].rationale.contains("fullscreen"));
}

#[test]
fn disabled_windows_are_skipped_before_restore() {
    let mut vscode = window(Some("com.microsoft.VSCode"), "Code", 0);
    vscode.enabled = false;

    let plan = plan_for(&snapshot(vec![vscode]));

    assert!(matches!(
        plan.operations[0].kind,
        OperationKind::Skip { .. }
    ));
    assert!(plan.operations[0].rationale.contains("disabled"));
}
