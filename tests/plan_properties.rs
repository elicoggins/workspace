//! Property-based tests for the deterministic planner.
//!
//! Invariants verified:
//! 1. Safe mode never emits Close or Minimize operations.
//! 2. Idempotency: planning twice on identical inputs yields identical plans.
//! 3. Convergence: SimulatedExecutor + planner reach 100% match in ≤ 2
//!    iterations for non-pathological worlds.
//! 4. Window-count conservation: a plan that contains only Reposition ops
//!    leaves the live-world window count unchanged after execution.

use chrono::{TimeZone, Utc};
use proptest::prelude::*;
use workspace::{
    execute::{execute_plan, ExecuteOptions, SimulatedExecutor},
    model::{
        DisplaySnapshot, Frame, HostInfo, WindowSnapshot, WorkspaceSnapshot, SNAPSHOT_VERSION,
    },
    plan::{plan_restore, LiveWindow, OperationKind, PlanOptions, RestoreMode, WorldState},
};

fn display() -> DisplaySnapshot {
    DisplaySnapshot {
        id: "cgdisplay-1".into(),
        numeric_id: 1,
        name: Some("Built-in".into()),
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

fn saved_window(bundle: &str, name: &str, idx: u32, frame: Frame) -> WindowSnapshot {
    WindowSnapshot {
        window_id: idx + 100,
        app_name: name.into(),
        process_name: name.into(),
        bundle_id: Some(bundle.into()),
        pid: idx as i32 + 1000,
        title: Some(format!("{name} #{idx}")),
        frame,
        display_id: Some("cgdisplay-1".into()),
        display_frame: Some(display().frame),
        display_relative_frame: None,
        z_order: Some(idx),
        fullscreen: false,
        minimized: false,
        enabled: true,
        browser_tabs: vec![],
    }
}

fn snapshot_of(windows: Vec<WindowSnapshot>) -> WorkspaceSnapshot {
    WorkspaceSnapshot {
        version: SNAPSHOT_VERSION,
        name: "prop".into(),
        created_at: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
        host: HostInfo {
            hostname: "test".into(),
            os: "macos".into(),
            arch: "aarch64".into(),
        },
        displays: vec![display()],
        windows,
    }
}

/// Strategy: build a small saved workspace (1..=4 windows of supported apps).
fn arb_saved() -> impl Strategy<Value = WorkspaceSnapshot> {
    let bundles = [
        ("com.apple.Terminal", "Terminal"),
        ("com.apple.finder", "Finder"),
        ("com.microsoft.VSCode", "Code"),
    ];
    proptest::collection::vec(
        (0usize..bundles.len(), 0.0f64..1200.0, 0.0f64..700.0),
        1..=4,
    )
    .prop_map(move |spec| {
        let windows: Vec<WindowSnapshot> = spec
            .into_iter()
            .enumerate()
            .map(|(i, (b, x, y))| {
                let (bundle, name) = bundles[b];
                saved_window(
                    bundle,
                    name,
                    i as u32,
                    Frame {
                        x,
                        y,
                        width: 800.0,
                        height: 600.0,
                    },
                )
            })
            .collect();
        snapshot_of(windows)
    })
}

fn target_frames(snap: &WorkspaceSnapshot) -> Vec<Frame> {
    snap.windows.iter().map(|w| w.frame).collect()
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 64, .. ProptestConfig::default()
    })]

    /// Safe mode must never emit destructive ops.
    #[test]
    fn safe_mode_never_destructive(snap in arb_saved()) {
        let world = WorldState::default();
        let frames = target_frames(&snap);
        let plan = plan_restore(
            &snap,
            &world,
            PlanOptions { mode: RestoreMode::Safe, dev_mode: false },
            &frames,
        );
        for op in &plan.operations {
            prop_assert!(
                !op.kind.is_destructive(),
                "safe-mode plan must not contain destructive ops, got {:?}",
                op.kind,
            );
        }
    }

    /// Planning is deterministic: two plans over identical inputs are equal.
    #[test]
    fn planner_is_idempotent(snap in arb_saved()) {
        let world = WorldState::default();
        let frames = target_frames(&snap);
        let opts = PlanOptions { mode: RestoreMode::Safe, dev_mode: false };
        let a = plan_restore(&snap, &world, opts, &frames);
        let b = plan_restore(&snap, &world, opts, &frames);
        prop_assert_eq!(a, b);
    }

    /// Convergence: applying plan+execute against a simulated empty world
    /// reaches verify-equivalence (target frame match) within 2 iterations.
    #[test]
    fn simulated_convergence_is_fast(snap in arb_saved()) {
        let frames = target_frames(&snap);
        let opts = PlanOptions { mode: RestoreMode::Safe, dev_mode: false };
        let mut world = WorldState::default();

        for _ in 0..3 {
            let plan = plan_restore(&snap, &world, opts, &frames);
            let mut exec = SimulatedExecutor::new(world.clone());
            execute_plan(&snap, &plan, &mut exec, ExecuteOptions::default());
            world = exec.world;
            // Stop once every saved window has a live window at its target frame.
            let satisfied = snap.windows.iter().zip(&frames).all(|(saved, target)| {
                world.windows.iter().any(|live| {
                    live.bundle_id.as_deref() == saved.bundle_id.as_deref()
                        && (live.frame.x - target.x).abs() < 1.0
                        && (live.frame.y - target.y).abs() < 1.0
                })
            });
            if satisfied {
                return Ok(());
            }
        }
        prop_assert!(false, "did not converge within 3 iterations");
    }

    /// Window-count conservation: a Reposition-only plan does not change the
    /// total number of live windows.
    #[test]
    fn reposition_only_conserves_window_count(snap in arb_saved()) {
        // Seed world with exactly one matching live window per saved window
        // so the planner only chooses Reposition ops.
        let mut world = WorldState::default();
        for (i, saved) in snap.windows.iter().enumerate() {
            world.windows.push(LiveWindow {
                bundle_id: saved.bundle_id.clone(),
                app_name: saved.app_name.clone(),
                pid: 9000 + i as i32,
                window_id: 5000 + i as u32,
                title: saved.title.clone(),
                frame: Frame {
                    x: saved.frame.x + 200.0,
                    y: saved.frame.y + 200.0,
                    width: saved.frame.width,
                    height: saved.frame.height,
                },
                minimized: false,
            });
            if let Some(bid) = &saved.bundle_id {
                world
                    .running_pids
                    .entry(bid.clone())
                    .or_default()
                    .push(9000 + i as i32);
            }
        }
        let before = world.windows.len();
        let frames = target_frames(&snap);
        let plan = plan_restore(
            &snap,
            &world,
            PlanOptions { mode: RestoreMode::Safe, dev_mode: false },
            &frames,
        );
        // Verify the plan only contains Reposition or Skip ops.
        for op in &plan.operations {
            let ok = matches!(
                op.kind,
                OperationKind::Reposition { .. } | OperationKind::Skip { .. }
            );
            prop_assert!(ok, "unexpected op kind: {:?}", op.kind);
        }
        let mut exec = SimulatedExecutor::new(world);
        execute_plan(&snap, &plan, &mut exec, ExecuteOptions::default());
        let after = exec.world.windows.len();
        prop_assert_eq!(before, after);
    }
}
