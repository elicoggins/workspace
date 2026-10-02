//! Restore policy and reservations for skipped windows.

use chrono::{TimeZone, Utc};
use workspace::{
    app_support::KNOWN_APPS,
    model::{
        DisplaySnapshot, Frame, HostInfo, RelativeFrame, WindowSnapshot, WorkspaceSnapshot,
        SNAPSHOT_VERSION,
    },
    plan::{plan_restore, LiveWindow, OperationKind, PlanOptions, RestoreMode, WorldState},
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
    assert_eq!(skip.rationale, "missing app bundle identifier");
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

fn live(window: &WindowSnapshot, window_id: u32) -> LiveWindow {
    LiveWindow {
        bundle_id: window.bundle_id.clone(),
        app_name: window.app_name.clone(),
        pid: window.pid,
        window_id,
        title: window.title.clone(),
        frame: window.frame,
        minimized: false,
    }
}

fn plan_in_world(
    snapshot: &WorkspaceSnapshot,
    windows: Vec<LiveWindow>,
    mode: RestoreMode,
    dev_mode: bool,
) -> (workspace::plan::RestorePlan, WorldState) {
    let mut world = WorldState::default();
    for window in &windows {
        if let Some(bundle) = &window.bundle_id {
            world
                .running_pids
                .entry(bundle.clone())
                .or_default()
                .push(window.pid);
        }
    }
    world.windows = windows;
    let frames: Vec<_> = snapshot.windows.iter().map(|window| window.frame).collect();
    let plan = plan_restore(snapshot, &world, PlanOptions { mode, dev_mode }, &frames);
    (plan, world)
}

#[test]
fn skipped_apps_never_receive_conflict_cleanup() {
    for mode in [RestoreMode::Reconcile, RestoreMode::Destructive] {
        let unsupported = window(Some("dev.example.Unknown"), "Unknown", 0);
        let mut fullscreen = window(Some("com.apple.Terminal"), "Terminal", 1);
        fullscreen.fullscreen = true;
        let mut disabled = window(Some("com.apple.finder"), "Finder", 2);
        disabled.enabled = false;
        let windows = vec![unsupported, fullscreen, disabled];
        let live: Vec<_> = windows
            .iter()
            .enumerate()
            .map(|(index, window)| live(window, index as u32 + 1))
            .collect();
        let (plan, _) = plan_in_world(&snapshot(windows), live, mode, false);
        assert_eq!(plan.destructive_ops, 0, "{plan:?}");
        assert!(plan
            .operations
            .iter()
            .all(|op| matches!(op.kind, OperationKind::Skip { .. })));
    }
}

#[test]
fn dev_mode_protects_editor_conflicts_while_restoring_their_windows() {
    for bundle in ["com.microsoft.VSCode", "com.todesktop.230313mzl4w4u92"] {
        for mode in [RestoreMode::Reconcile, RestoreMode::Destructive] {
            let main = window(Some(bundle), "editor", 0);
            let mut extra = main.clone();
            extra.title = Some("unrelated editor window".into());
            extra.frame.x += 1000.0;
            let (plan, _) = plan_in_world(
                &snapshot(vec![main.clone()]),
                vec![live(&main, 1), live(&extra, 2)],
                mode,
                true,
            );
            assert_eq!(plan.summary().reposition, 1, "{plan:?}");
            assert_eq!(plan.destructive_ops, 0, "{plan:?}");
            assert_eq!(plan.left_alone_conflicts, 1, "{plan:?}");
        }
    }
}

#[test]
fn skipped_windows_are_reserved_before_reuse_and_cleanup() {
    for fullscreen in [false, true] {
        for mode in [
            RestoreMode::Safe,
            RestoreMode::Reconcile,
            RestoreMode::Destructive,
        ] {
            // Both windows have the same title, so reservation must account
            // for distinct geometry rather than excluding a whole app/title.
            let active = window(Some("com.apple.Terminal"), "Terminal", 0);
            let mut protected = active.clone();
            protected.frame.x += 1000.0;
            protected.fullscreen = fullscreen;
            protected.enabled = fullscreen;
            let snap = snapshot(vec![active.clone(), protected.clone()]);
            let (plan, world) = plan_in_world(&snap, vec![live(&protected, 7)], mode, false);
            assert_eq!(plan.summary().create, 1, "{plan:?}");
            assert_eq!(plan.summary().reposition, 0, "{plan:?}");
            assert_eq!(plan.destructive_ops, 0, "{plan:?}");

            let frames: Vec<_> = snap.windows.iter().map(|window| window.frame).collect();
            let report = workspace::verify::verify(&snap, &world, &frames);
            assert_eq!(report.skipped, 1);
            assert_eq!(report.matched, 0, "{report:?}");
            assert_eq!(report.unmatched, 1, "{report:?}");
        }
    }
}

#[test]
fn equally_plausible_matches_protect_skipped_windows() {
    for fullscreen in [false, true] {
        let active = window(Some("com.apple.Terminal"), "Terminal", 0);
        let mut protected = active.clone();
        protected.fullscreen = fullscreen;
        protected.enabled = fullscreen;
        let snap = snapshot(vec![active, protected.clone()]);
        let (plan, world) = plan_in_world(
            &snap,
            vec![live(&protected, 7)],
            RestoreMode::Destructive,
            false,
        );
        assert_eq!(plan.summary().create, 1, "{plan:?}");
        assert_eq!(plan.summary().reposition, 0, "{plan:?}");
        assert_eq!(plan.destructive_ops, 0, "{plan:?}");
        let frames: Vec<_> = snap.windows.iter().map(|window| window.frame).collect();
        let report = workspace::verify::verify(&snap, &world, &frames);
        assert_eq!(report.matched, 0, "{report:?}");
        assert_eq!(report.skipped, 1);
    }
}

#[test]
fn title_and_position_changes_do_not_expose_skipped_windows_to_cleanup() {
    for fullscreen in [false, true] {
        for mode in [RestoreMode::Reconcile, RestoreMode::Destructive] {
            let active = window(Some("com.apple.Terminal"), "active", 0);
            let mut protected = window(Some("com.apple.Terminal"), "old document", 1);
            protected.frame.x += 1000.0;
            protected.enabled = fullscreen;
            protected.fullscreen = fullscreen;
            let snap = snapshot(vec![active.clone(), protected]);
            for title in [None, Some("different content")] {
                for offset in [0.0, 300.0] {
                    let mut renamed = live(&snap.windows[1], 2);
                    renamed.title = title.map(str::to_string);
                    renamed.frame.x += offset;
                    let (plan, world) =
                        plan_in_world(&snap, vec![live(&active, 1), renamed], mode, false);
                    assert_eq!(plan.summary().reposition, 1, "{plan:?}");
                    assert_eq!(plan.destructive_ops, 0, "{plan:?}");
                    let frames: Vec<_> = snap.windows.iter().map(|window| window.frame).collect();
                    assert!(workspace::verify::verify(&snap, &world, &frames).converged);
                }
            }
        }
    }
}

#[test]
fn cleanup_still_targets_genuine_extras_beside_skipped_windows() {
    for mode in [RestoreMode::Reconcile, RestoreMode::Destructive] {
        let active = window(Some("com.apple.Terminal"), "Terminal", 0);
        let mut protected = active.clone();
        protected.enabled = false;
        protected.frame.x += 1000.0;
        let mut extra = active.clone();
        extra.title = Some("extra".into());
        extra.frame.x += 2000.0;
        let snap = snapshot(vec![active.clone(), protected.clone()]);
        let (plan, world) = plan_in_world(
            &snap,
            vec![live(&active, 1), live(&protected, 2), live(&extra, 3)],
            mode,
            false,
        );
        let cleanup: Vec<_> = plan
            .operations
            .iter()
            .filter_map(|op| match op.kind {
                OperationKind::CloseConflict { live_window_id, .. }
                | OperationKind::MinimizeConflict { live_window_id, .. } => Some(live_window_id),
                _ => None,
            })
            .collect();
        assert_eq!(cleanup, vec![3], "{plan:?}");
        let frames: Vec<_> = snap.windows.iter().map(|window| window.frame).collect();
        let report = workspace::verify::verify(&snap, &world, &frames);
        assert_eq!(report.matched, 1);
        assert_eq!(report.accuracy, 1.0);
    }
}
