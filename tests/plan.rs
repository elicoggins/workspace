use chrono::Utc;
use workspace::model::{Frame, HostInfo, WindowSnapshot, WorkspaceSnapshot, SNAPSHOT_VERSION};
use workspace::plan::{
    plan_restore, LiveWindow, OperationKind, PlanOptions, RestoreMode, WorldState,
};

fn frame(x: f64, y: f64) -> Frame {
    Frame {
        x,
        y,
        width: 800.0,
        height: 600.0,
    }
}

fn saved(bundle: &str, title: &str, frame_: Frame) -> WindowSnapshot {
    WindowSnapshot {
        window_id: 1,
        app_name: bundle.to_string(),
        process_name: bundle.to_string(),
        bundle_id: Some(bundle.to_string()),
        pid: 1,
        title: Some(title.to_string()),
        frame: frame_,
        display_id: None,
        display_frame: None,
        display_relative_frame: None,
        z_order: Some(0),
        fullscreen: false,
        minimized: false,
        enabled: true,
        browser_tabs: Vec::new(),
    }
}

fn snapshot(windows: Vec<WindowSnapshot>) -> WorkspaceSnapshot {
    WorkspaceSnapshot {
        version: SNAPSHOT_VERSION,
        name: "plan-test".to_string(),
        created_at: Utc::now(),
        host: HostInfo {
            hostname: "host".to_string(),
            os: "macos".to_string(),
            arch: "aarch64".to_string(),
        },
        displays: Vec::new(),
        windows,
    }
}

#[test]
fn safe_mode_never_emits_close_operations_for_user_apps() {
    let bundle = "com.apple.Terminal";
    let snap = snapshot(vec![saved(bundle, "main", frame(0.0, 0.0))]);
    let world = WorldState {
        displays: Vec::new(),
        windows: vec![
            LiveWindow {
                bundle_id: Some(bundle.to_string()),
                app_name: bundle.to_string(),
                pid: 99,
                window_id: 10,
                title: Some("main".to_string()),
                frame: frame(50.0, 50.0),
                minimized: false,
            },
            LiveWindow {
                bundle_id: Some(bundle.to_string()),
                app_name: bundle.to_string(),
                pid: 99,
                window_id: 11,
                title: Some("scratch".to_string()),
                frame: frame(100.0, 100.0),
                minimized: false,
            },
            // unrelated app — must NEVER be touched
            LiveWindow {
                bundle_id: Some("com.apple.Notes".to_string()),
                app_name: "Notes".to_string(),
                pid: 42,
                window_id: 12,
                title: Some("inbox".to_string()),
                frame: frame(200.0, 200.0),
                minimized: false,
            },
        ],
        running_pids: std::collections::HashMap::from([(bundle.to_string(), vec![99])]),
    };

    let plan = plan_restore(
        &snap,
        &world,
        PlanOptions {
            mode: RestoreMode::Safe,
            dev_mode: false,
        },
        &[frame(0.0, 0.0)],
    );

    for op in &plan.operations {
        assert!(
            !matches!(
                op.kind,
                OperationKind::CloseConflict { .. } | OperationKind::MinimizeConflict { .. }
            ),
            "safe mode emitted destructive op: {op:?}"
        );
        // No operation should ever target Notes (we don't own it).
        assert_ne!(op.bundle_id.as_deref(), Some("com.apple.Notes"));
    }
}

#[test]
fn destructive_mode_only_closes_owned_bundles() {
    let bundle = "com.apple.Terminal";
    let snap = snapshot(vec![saved(bundle, "main", frame(0.0, 0.0))]);
    let world = WorldState {
        displays: Vec::new(),
        windows: vec![
            LiveWindow {
                bundle_id: Some(bundle.to_string()),
                app_name: bundle.to_string(),
                pid: 99,
                window_id: 10,
                title: Some("main".to_string()),
                frame: frame(50.0, 50.0),
                minimized: false,
            },
            LiveWindow {
                bundle_id: Some(bundle.to_string()),
                app_name: bundle.to_string(),
                pid: 99,
                window_id: 11,
                title: Some("scratch".to_string()),
                frame: frame(100.0, 100.0),
                minimized: false,
            },
            LiveWindow {
                bundle_id: Some("com.apple.Notes".to_string()),
                app_name: "Notes".to_string(),
                pid: 42,
                window_id: 12,
                title: Some("inbox".to_string()),
                frame: frame(200.0, 200.0),
                minimized: false,
            },
        ],
        running_pids: std::collections::HashMap::from([
            (bundle.to_string(), vec![99]),
            ("com.apple.Notes".to_string(), vec![42]),
        ]),
    };

    let plan = plan_restore(
        &snap,
        &world,
        PlanOptions {
            mode: RestoreMode::Destructive,
            dev_mode: false,
        },
        &[frame(0.0, 0.0)],
    );

    let close_ops: Vec<_> = plan
        .operations
        .iter()
        .filter(|op| matches!(op.kind, OperationKind::CloseConflict { .. }))
        .collect();
    assert_eq!(
        close_ops.len(),
        1,
        "should close one conflicting Terminal window"
    );
    assert_eq!(close_ops[0].bundle_id.as_deref(), Some(bundle));

    // Notes must not appear in any operation.
    for op in &plan.operations {
        assert_ne!(op.bundle_id.as_deref(), Some("com.apple.Notes"));
    }
}
