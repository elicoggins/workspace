use chrono::Utc;
use workspace::{
    execute::{execute_plan, ExecuteOptions, SimulatedExecutor},
    model::{
        DisplaySnapshot, Frame, HostInfo, RelativeFrame, WindowSnapshot, WorkspaceSnapshot,
        SNAPSHOT_VERSION,
    },
    plan::{LiveWindow, OperationKind, RestoreMode, WorldState},
    verify::verify,
    world::{plan_for_world, target_frame_for_window},
};

fn display(id: &str, numeric_id: u32, frame: Frame, primary: bool) -> DisplaySnapshot {
    DisplaySnapshot {
        id: id.to_string(),
        numeric_id,
        name: None,
        frame,
        scale_factor: 1.0,
        is_primary: primary,
    }
}

fn window(frame: Frame, display: &DisplaySnapshot) -> WindowSnapshot {
    WindowSnapshot {
        window_id: 9,
        app_name: "Terminal".to_string(),
        process_name: "Terminal".to_string(),
        bundle_id: Some("com.apple.Terminal".to_string()),
        pid: 42,
        title: Some("shell".to_string()),
        frame,
        display_id: Some(display.id.clone()),
        display_frame: Some(display.frame),
        display_relative_frame: Some(RelativeFrame {
            x: 0.1,
            y: 0.2,
            width: 0.4,
            height: 0.5,
        }),
        z_order: Some(0),
        fullscreen: false,
        minimized: false,
        enabled: true,
        browser_tabs: Vec::new(),
    }
}

#[test]
fn unchanged_monitor_keeps_exact_frame() {
    let saved_display = display(
        "cgdisplay-1",
        1,
        Frame {
            x: 0.0,
            y: 0.0,
            width: 2560.0,
            height: 1440.0,
        },
        true,
    );
    let saved_window = window(
        Frame {
            x: 0.0,
            y: 0.0,
            width: 1280.0,
            height: 900.0,
        },
        &saved_display,
    );

    let target = target_frame_for_window(
        &saved_window,
        std::slice::from_ref(&saved_display),
        std::slice::from_ref(&saved_display),
    );

    assert_eq!(target, saved_window.frame);
}

#[test]
fn removed_monitor_uses_relative_geometry_on_primary() {
    let saved_display = display(
        "external",
        2,
        Frame {
            x: 2560.0,
            y: 0.0,
            width: 1920.0,
            height: 1080.0,
        },
        false,
    );
    let current_display = display(
        "built-in",
        1,
        Frame {
            x: 0.0,
            y: 0.0,
            width: 1440.0,
            height: 900.0,
        },
        true,
    );
    let saved_window = window(
        Frame {
            x: 2752.0,
            y: 216.0,
            width: 768.0,
            height: 540.0,
        },
        &saved_display,
    );

    let target = target_frame_for_window(&saved_window, &[saved_display], &[current_display]);

    assert_eq!(target.width, 576.0);
    assert_eq!(target.height, 450.0);
    assert!(target.x >= 12.0);
    assert!(target.y >= 12.0);
}

#[test]
fn changed_resolution_keeps_the_same_monitor() {
    let saved = display(
        "external",
        2,
        Frame {
            x: 1920.0,
            y: 0.0,
            width: 1920.0,
            height: 1080.0,
        },
        false,
    );
    let saved_window = window(
        Frame {
            x: 2112.0,
            y: 216.0,
            width: 768.0,
            height: 540.0,
        },
        &saved,
    );
    let built_in = display(
        "built-in",
        1,
        Frame {
            x: 0.0,
            y: 0.0,
            width: 1920.0,
            height: 1080.0,
        },
        true,
    );
    let external_frame = Frame {
        x: 1920.0,
        y: -100.0,
        width: 1280.0,
        height: 720.0,
    };
    for (id, numeric_id) in [("external", 9), ("external-renamed", 2)] {
        let external = display(id, numeric_id, external_frame, false);
        let target = target_frame_for_window(
            &saved_window,
            std::slice::from_ref(&saved),
            &[built_in.clone(), external],
        );
        assert_eq!(
            target,
            Frame {
                x: 2048.0,
                y: 44.0,
                width: 512.0,
                height: 360.0
            }
        );
    }
}

#[test]
fn changed_titles_do_not_cause_repeated_creation() {
    let saved_display = display(
        "built-in",
        1,
        Frame {
            x: 0.0,
            y: 0.0,
            width: 1920.0,
            height: 1080.0,
        },
        true,
    );
    let saved = window(
        Frame {
            x: 100.0,
            y: 100.0,
            width: 800.0,
            height: 600.0,
        },
        &saved_display,
    );
    let snapshot = WorkspaceSnapshot {
        version: SNAPSHOT_VERSION,
        name: "changed-title".to_string(),
        created_at: Utc::now(),
        host: HostInfo {
            hostname: "host".to_string(),
            os: "macos".to_string(),
            arch: "aarch64".to_string(),
        },
        displays: vec![saved_display.clone()],
        windows: vec![saved],
    };
    let target = snapshot.windows[0].frame;
    for mode in [
        RestoreMode::Safe,
        RestoreMode::Reconcile,
        RestoreMode::Destructive,
    ] {
        let mut executor = SimulatedExecutor::new(WorldState {
            displays: vec![saved_display.clone()],
            ..WorldState::default()
        });
        executor.launch_creates_window = false;
        let first_plan = plan_for_world(&snapshot, &executor.world, mode, false);
        let journal = execute_plan(
            &snapshot,
            &first_plan,
            &mut executor,
            ExecuteOptions::default(),
        );
        assert!(journal.succeeded(&first_plan));
        assert_eq!(executor.world.windows.len(), 1);
        // App creation restores geometry, but cannot restore the old session title.
        executor.world.windows[0].title = Some("new document".to_string());
        let id = executor.world.windows[0].window_id;
        for _ in 0..3 {
            let report = verify(&snapshot, &executor.world, &[target]);
            assert!(report.converged, "{report:?}");
            let plan = plan_for_world(&snapshot, &executor.world, mode, false);
            assert_eq!(plan.operations.len(), 1, "{plan:?}");
            assert!(
                matches!(plan.operations[0].kind, OperationKind::Reposition { live_window_id, .. } if live_window_id == id)
            );
            let journal = execute_plan(&snapshot, &plan, &mut executor, ExecuteOptions::default());
            assert!(journal.succeeded(&plan));
            assert_eq!(executor.world.windows.len(), 1);
        }
    }
}

#[test]
fn titleless_remapped_windows_are_reused_across_restore_passes() {
    let saved_display = display(
        "external",
        2,
        Frame {
            x: 3000.0,
            y: 0.0,
            width: 2000.0,
            height: 1000.0,
        },
        false,
    );
    let current_display = display(
        "built-in",
        1,
        Frame {
            x: 0.0,
            y: 0.0,
            width: 1000.0,
            height: 800.0,
        },
        true,
    );
    let windows = [
        Frame {
            x: 3200.0,
            y: 100.0,
            width: 400.0,
            height: 200.0,
        },
        Frame {
            x: 4000.0,
            y: 550.0,
            width: 400.0,
            height: 200.0,
        },
    ]
    .into_iter()
    .enumerate()
    .map(|(index, frame)| {
        let mut saved = window(frame, &saved_display);
        saved.window_id = index as u32 + 10;
        saved.title = None;
        saved.display_relative_frame = Some(frame.relative_to(saved_display.frame));
        saved
    })
    .collect();
    let snapshot = WorkspaceSnapshot {
        version: SNAPSHOT_VERSION,
        name: "remapped".to_string(),
        created_at: Utc::now(),
        host: HostInfo {
            hostname: "host".to_string(),
            os: "macos".to_string(),
            arch: "aarch64".to_string(),
        },
        displays: vec![saved_display],
        windows,
    };
    let targets: Vec<_> = snapshot
        .windows
        .iter()
        .map(|saved| {
            target_frame_for_window(
                saved,
                &snapshot.displays,
                std::slice::from_ref(&current_display),
            )
        })
        .collect();

    for mode in [
        RestoreMode::Safe,
        RestoreMode::Reconcile,
        RestoreMode::Destructive,
    ] {
        for already_remapped in [false, true] {
            let live = snapshot
                .windows
                .iter()
                .zip(&targets)
                .map(|(saved, target)| LiveWindow {
                    bundle_id: saved.bundle_id.clone(),
                    app_name: saved.app_name.clone(),
                    pid: saved.pid,
                    window_id: saved.window_id,
                    title: None,
                    frame: if already_remapped {
                        *target
                    } else {
                        saved.frame
                    },
                    minimized: false,
                })
                .collect();
            let mut executor = SimulatedExecutor::new(WorldState {
                windows: live,
                displays: vec![current_display.clone()],
                running_pids: std::collections::HashMap::from([(
                    "com.apple.Terminal".to_string(),
                    vec![42],
                )]),
            });
            for _ in 0..3 {
                let plan = plan_for_world(&snapshot, &executor.world, mode, false);
                assert_eq!(plan.operations.len(), 2, "{plan:?}");
                for (index, op) in plan.operations.iter().enumerate() {
                    assert!(
                        matches!(op.kind, OperationKind::Reposition { live_window_id, .. }
                        if live_window_id == snapshot.windows[index].window_id),
                        "{plan:?}"
                    );
                }
                let journal =
                    execute_plan(&snapshot, &plan, &mut executor, ExecuteOptions::default());
                assert_eq!(journal.counts().success, 2, "{journal:?}");
                assert_eq!(executor.world.windows.len(), 2);
                let report = verify(&snapshot, &executor.world, &targets);
                assert_eq!(report.matched, 2, "{report:?}");
                assert_eq!(report.max_geometry_delta, 0.0);
                assert!(report.converged);
            }
        }
    }
}
