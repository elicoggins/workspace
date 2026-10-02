use std::collections::HashMap;

use chrono::Utc;
use workspace::{
    error::{Result, WorkspaceError},
    execute::{
        execute_plan, ExecuteOptions, Executor, JournalStatus, OpOutcome, SimulatedExecutor,
    },
    model::{BrowserTab, Frame, HostInfo, WindowSnapshot, WorkspaceSnapshot, SNAPSHOT_VERSION},
    plan::{plan_restore, LiveWindow, PlanOptions, RestoreMode, WorldState},
};

fn frame(x: f64) -> Frame {
    Frame {
        x,
        y: 0.0,
        width: 800.0,
        height: 600.0,
    }
}

fn saved(bundle: &str, title: Option<&str>) -> WindowSnapshot {
    WindowSnapshot {
        window_id: 100,
        app_name: bundle.into(),
        process_name: bundle.into(),
        bundle_id: Some(bundle.into()),
        pid: 1,
        title: title.map(str::to_owned),
        frame: frame(0.0),
        display_id: None,
        display_frame: None,
        display_relative_frame: None,
        z_order: Some(0),
        fullscreen: false,
        minimized: false,
        enabled: true,
        browser_tabs: vec![],
    }
}

fn snapshot(windows: Vec<WindowSnapshot>) -> WorkspaceSnapshot {
    WorkspaceSnapshot {
        version: SNAPSHOT_VERSION,
        name: "dependencies".into(),
        created_at: Utc::now(),
        host: HostInfo {
            hostname: "test".into(),
            os: "macos".into(),
            arch: "aarch64".into(),
        },
        displays: vec![],
        windows,
    }
}

fn live(bundle: &str, title: Option<&str>, pid: i32, window_id: u32, x: f64) -> LiveWindow {
    LiveWindow {
        bundle_id: Some(bundle.into()),
        app_name: bundle.into(),
        pid,
        window_id,
        title: title.map(str::to_owned),
        frame: frame(x),
        minimized: false,
    }
}

struct RecordingExecutor {
    fail_bundle: String,
    fail_op: &'static str,
    fail_status: JournalStatus,
    return_error: bool,
    calls: Vec<String>,
}

impl RecordingExecutor {
    fn call(&mut self, op: &str, bundle: &str) -> Result<OpOutcome> {
        self.calls.push(format!("{op}:{bundle}"));
        if op == self.fail_op && bundle == self.fail_bundle {
            if self.return_error {
                return Err(WorkspaceError::MacOs("refused".into()));
            }
            Ok(OpOutcome {
                status: self.fail_status,
                message: "unconfirmed".into(),
                attempts: 1,
            })
        } else {
            Ok(OpOutcome::success("confirmed"))
        }
    }
}

impl Executor for RecordingExecutor {
    fn launch_app(&mut self, bundle: &str) -> Result<OpOutcome> {
        self.call("launch", bundle)
    }

    fn create_window(&mut self, bundle: &str, _: &WindowSnapshot, _: Frame) -> Result<OpOutcome> {
        self.call("create", bundle)
    }

    fn reposition(
        &mut self,
        _: i32,
        _: u32,
        saved: &WindowSnapshot,
        _: Frame,
    ) -> Result<OpOutcome> {
        self.call("reposition", saved.bundle_id.as_deref().unwrap())
    }

    fn restore_chrome_tabs(
        &mut self,
        bundle: &str,
        _: &[(&WindowSnapshot, Frame)],
    ) -> Result<OpOutcome> {
        self.call("chrome_tabs", bundle)
    }

    fn minimize_window(&mut self, pid: i32, id: u32) -> Result<OpOutcome> {
        self.calls.push(format!("minimize:{pid}:{id}"));
        Ok(OpOutcome::success("minimized"))
    }

    fn close_window(&mut self, pid: i32, id: u32) -> Result<OpOutcome> {
        self.calls.push(format!("close:{pid}:{id}"));
        Ok(OpOutcome::success("closed"))
    }
}

#[test]
fn unconfirmed_restoration_blocks_only_that_apps_cleanup() {
    for op in ["create", "reposition", "chrome_tabs"] {
        for status in [
            JournalStatus::Success,
            JournalStatus::Failed,
            JournalStatus::PartialSuccess,
            JournalStatus::Skipped,
        ] {
            for mode in [RestoreMode::Reconcile, RestoreMode::Destructive] {
                let bundle = if op == "chrome_tabs" {
                    "com.google.Chrome"
                } else {
                    "com.apple.Terminal"
                };
                let mut wanted = saved(bundle, (op == "reposition").then_some("wanted"));
                if op == "chrome_tabs" {
                    wanted.browser_tabs.push(BrowserTab {
                        title: None,
                        url: "https://example.com/".into(),
                        active: true,
                    });
                }
                let survivor = saved(bundle, Some("survivor"));
                let other = saved("com.apple.Notes", Some("note"));
                let snap = snapshot(vec![wanted, survivor, other]);
                let mut world = WorldState {
                    displays: vec![],
                    windows: vec![
                        live(bundle, None, 1, 2, 5000.0),
                        live(bundle, Some("survivor"), 1, 5, 0.0),
                        live("com.apple.Notes", Some("note"), 2, 3, 0.0),
                        live("com.apple.Notes", Some("extra"), 2, 4, 5000.0),
                    ],
                    running_pids: HashMap::from([
                        (bundle.into(), vec![1]),
                        ("com.apple.Notes".into(), vec![2]),
                    ]),
                };
                if op == "reposition" {
                    world.windows.push(live(bundle, Some("wanted"), 1, 1, 0.0));
                }
                let frames: Vec<_> = snap.windows.iter().map(|window| window.frame).collect();
                let plan = plan_restore(
                    &snap,
                    &world,
                    PlanOptions {
                        mode,
                        dev_mode: false,
                    },
                    &frames,
                );
                let mut exec = RecordingExecutor {
                    fail_bundle: bundle.into(),
                    fail_op: op,
                    fail_status: status,
                    return_error: false,
                    calls: vec![],
                };
                let journal = execute_plan(&snap, &plan, &mut exec, ExecuteOptions::default());
                assert_eq!(
                    journal.succeeded(&plan),
                    status == JournalStatus::Success,
                    "{journal:?}"
                );
                let cleanup = if mode == RestoreMode::Destructive {
                    "close"
                } else {
                    "minimize"
                };
                let blocked = journal
                    .entries
                    .iter()
                    .find(|entry| entry.op == cleanup && entry.bundle_id.as_deref() == Some(bundle))
                    .unwrap();
                if status == JournalStatus::Success {
                    assert_eq!(blocked.status, JournalStatus::Success, "{journal:?}");
                    assert!(exec.calls.contains(&format!("{cleanup}:1:2")));
                } else {
                    assert_eq!(blocked.status, JournalStatus::Skipped, "{journal:?}");
                    assert_eq!(blocked.attempts, 0);
                    assert!(blocked.message.contains("prerequisite"));
                    assert!(!exec.calls.contains(&format!("{cleanup}:1:2")));
                }
                // A window failure blocks cleanup but still permits restoring
                // the app's remaining windows and completing other apps.
                assert!(exec.calls.contains(&format!("reposition:{bundle}")));
                assert!(
                    exec.calls.contains(&format!("{cleanup}:2:4")),
                    "{:?}",
                    exec.calls
                );
            }
        }
    }
}

#[test]
fn policy_skips_do_not_prevent_successful_restore_completion() {
    let bundle = "com.apple.Terminal";
    let mut disabled = saved(bundle, Some("protected"));
    disabled.enabled = false;
    let snap = snapshot(vec![saved(bundle, Some("main")), disabled]);
    let world = WorldState {
        windows: vec![live(bundle, Some("main"), 1, 1, 0.0)],
        running_pids: HashMap::from([(bundle.into(), vec![1])]),
        ..WorldState::default()
    };
    let plan = plan_restore(&snap, &world, PlanOptions::default(), &[frame(0.0); 2]);
    let mut executor = SimulatedExecutor::new(world);
    let journal = execute_plan(&snap, &plan, &mut executor, ExecuteOptions::default());
    assert_eq!(journal.counts().skipped, 1);
    assert_eq!(journal.counts().success, 1);
    assert!(journal.succeeded(&plan));
    assert!(workspace::verify::verify(&snap, &executor.world, &[frame(0.0); 2]).converged);
}

#[test]
fn failed_or_partial_launch_cancels_dependent_window_creation() {
    let bundle = "com.google.Chrome";
    let mut browser = saved(bundle, Some("browser"));
    browser.browser_tabs.push(BrowserTab {
        title: None,
        url: "https://example.com/".into(),
        active: true,
    });
    let snap = snapshot(vec![saved(bundle, Some("main")), browser]);
    let plan = plan_restore(
        &snap,
        &WorldState::default(),
        PlanOptions::default(),
        &[frame(0.0); 2],
    );
    for status in [
        JournalStatus::Failed,
        JournalStatus::PartialSuccess,
        JournalStatus::Skipped,
    ] {
        for return_error in [false, true] {
            let mut exec = RecordingExecutor {
                fail_bundle: bundle.into(),
                fail_op: "launch",
                fail_status: status,
                return_error,
                calls: vec![],
            };
            let journal = execute_plan(&snap, &plan, &mut exec, ExecuteOptions::default());
            assert!(!journal.succeeded(&plan));
            assert_eq!(exec.calls, vec![format!("launch:{bundle}")]);
            for entry in &journal.entries[1..] {
                assert_eq!(entry.status, JournalStatus::Skipped);
                assert_eq!(entry.attempts, 0);
                assert!(entry.message.contains("launch"));
            }
        }
    }
}

#[test]
fn dry_run_never_calls_executor_even_for_failed_launches() {
    let bundle = "com.apple.Terminal";
    let snap = snapshot(vec![saved(bundle, Some("main"))]);
    let plan = plan_restore(
        &snap,
        &WorldState::default(),
        PlanOptions::default(),
        &[frame(0.0)],
    );
    let mut exec = RecordingExecutor {
        fail_bundle: bundle.into(),
        fail_op: "launch",
        fail_status: JournalStatus::Failed,
        return_error: true,
        calls: vec![],
    };
    let journal = execute_plan(&snap, &plan, &mut exec, ExecuteOptions { dry_run: true });
    assert!(exec.calls.is_empty());
    assert!(!journal.succeeded(&plan));
    assert!(journal
        .entries
        .iter()
        .all(|entry| entry.message.starts_with("dry-run: would ")));
}

#[test]
fn simulation_allocates_distinct_ids_with_minimized_windows() {
    let bundle = "com.apple.Terminal";
    let snap = snapshot(vec![saved(bundle, Some("main"))]);
    let mut minimized = live(bundle, Some("minimized"), 1, u32::MAX, 5000.0);
    minimized.minimized = true;
    let mut exec = SimulatedExecutor::new(WorldState {
        displays: vec![],
        windows: vec![minimized, live(bundle, Some("existing"), 1, 1, 4000.0)],
        running_pids: HashMap::from([(bundle.into(), vec![1])]),
    });
    exec.create_window(bundle, &snap.windows[0], frame(0.0))
        .unwrap();
    exec.create_window(bundle, &snap.windows[0], frame(0.0))
        .unwrap();
    let ids: std::collections::HashSet<_> = exec
        .world
        .windows
        .iter()
        .map(|window| window.window_id)
        .collect();
    assert_eq!(ids.len(), exec.world.windows.len());
}

#[test]
fn cleanup_requires_completed_restore_prerequisites() {
    let bundle = "com.apple.Terminal";
    let snap = snapshot(vec![saved(bundle, Some("main"))]);
    let world = WorldState {
        displays: vec![],
        windows: vec![
            live(bundle, Some("main"), 1, 1, 0.0),
            live(bundle, Some("extra"), 1, 2, 5000.0),
        ],
        running_pids: HashMap::from([(bundle.into(), vec![1])]),
    };
    for mode in [RestoreMode::Reconcile, RestoreMode::Destructive] {
        let plan = plan_restore(
            &snap,
            &world,
            PlanOptions {
                mode,
                dev_mode: false,
            },
            &[frame(0.0)],
        );
        for cleanup_only in [false, true] {
            let mut invalid_plan = plan.clone();
            if cleanup_only {
                invalid_plan.operations.remove(0);
            } else {
                invalid_plan.operations.reverse();
            }
            let mut exec = SimulatedExecutor::new(world.clone());
            let journal = execute_plan(&snap, &invalid_plan, &mut exec, ExecuteOptions::default());
            assert_eq!(journal.entries[0].status, JournalStatus::Skipped);
            assert_eq!(journal.entries[0].attempts, 0);
            assert_eq!(exec.world.windows.len(), 2);
            assert!(exec.world.windows.iter().all(|window| !window.minimized));
        }
    }
}

#[test]
fn simulation_allocates_distinct_positive_pids_after_overflow() {
    let mut exec = SimulatedExecutor::new(WorldState {
        displays: vec![],
        windows: vec![],
        running_pids: HashMap::from([("existing".into(), vec![1, i32::MAX])]),
    });
    exec.launch_app("first").unwrap();
    exec.launch_app("second").unwrap();
    let pids: Vec<_> = exec
        .world
        .running_pids
        .values()
        .flatten()
        .copied()
        .collect();
    let unique: std::collections::HashSet<_> = pids.iter().copied().collect();
    assert_eq!(pids.len(), unique.len());
    assert!(pids.iter().all(|pid| *pid > 0));
}
