//! Execution engine for restore plans.
//!
//! The planner produces a [`RestorePlan`]; this module *executes* it against
//! a pluggable [`Executor`]. A real-world [`MacOsExecutor`] talks to the
//! system, and a [`SimulatedExecutor`] mutates an in-memory [`WorldState`]
//! for deterministic testing.
//!
//! Every operation is recorded in an [`ExecutionJournal`] with status,
//! timing, and a human-readable message — these journals are the audit trail
//! for both production debugging and test assertions.

use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    error::Result,
    model::{Frame, WindowSnapshot, WorkspaceSnapshot},
    plan::{LiveWindow, OperationKind, PlannedOperation, RestorePlan, WorldState},
};

// ---------------------------------------------------------------------------
// Journal
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalStatus {
    /// Op executed and produced the desired observable change.
    Success,
    /// Op executed but the post-condition could not be observed.
    PartialSuccess,
    /// Op was intentionally skipped (gate-skip or planner Skip).
    Skipped,
    /// Op was attempted but failed.
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalEntry {
    pub op_index: usize,
    pub op: String,
    pub app_name: String,
    pub bundle_id: Option<String>,
    pub saved_window_index: Option<usize>,
    pub status: JournalStatus,
    pub started_at: DateTime<Utc>,
    pub duration_ms: u64,
    pub message: String,
    pub attempts: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionJournal {
    pub snapshot: String,
    pub started_at: DateTime<Utc>,
    pub duration_ms: u64,
    pub entries: Vec<JournalEntry>,
}

impl ExecutionJournal {
    pub fn counts(&self) -> JournalCounts {
        let mut counts = JournalCounts::default();
        for entry in &self.entries {
            match entry.status {
                JournalStatus::Success => counts.success += 1,
                JournalStatus::PartialSuccess => counts.partial += 1,
                JournalStatus::Skipped => counts.skipped += 1,
                JournalStatus::Failed => counts.failed += 1,
            }
        }
        counts
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalCounts {
    pub success: usize,
    pub partial: usize,
    pub skipped: usize,
    pub failed: usize,
}

// ---------------------------------------------------------------------------
// Executor trait
// ---------------------------------------------------------------------------

/// Outcome of a single executor call. `attempts` is used by the executor to
/// surface its internal retry budget into the journal.
#[derive(Debug, Clone)]
pub struct OpOutcome {
    pub status: JournalStatus,
    pub message: String,
    pub attempts: u32,
}

impl OpOutcome {
    pub fn success(msg: impl Into<String>) -> Self {
        Self {
            status: JournalStatus::Success,
            message: msg.into(),
            attempts: 1,
        }
    }

    pub fn partial(msg: impl Into<String>) -> Self {
        Self {
            status: JournalStatus::PartialSuccess,
            message: msg.into(),
            attempts: 1,
        }
    }

    pub fn skipped(msg: impl Into<String>) -> Self {
        Self {
            status: JournalStatus::Skipped,
            message: msg.into(),
            attempts: 0,
        }
    }

    pub fn failed(msg: impl Into<String>) -> Self {
        Self {
            status: JournalStatus::Failed,
            message: msg.into(),
            attempts: 1,
        }
    }

    pub fn with_attempts(mut self, attempts: u32) -> Self {
        self.attempts = attempts;
        self
    }
}

pub trait Executor {
    fn launch_app(&mut self, bundle_id: &str) -> Result<OpOutcome>;
    fn create_window(
        &mut self,
        bundle_id: &str,
        saved: &WindowSnapshot,
        target: Frame,
    ) -> Result<OpOutcome>;
    fn reposition(
        &mut self,
        pid: i32,
        window_id: u32,
        saved: &WindowSnapshot,
        target: Frame,
    ) -> Result<OpOutcome>;
    fn restore_chrome_tabs(
        &mut self,
        bundle_id: &str,
        windows: &[(&WindowSnapshot, Frame)],
    ) -> Result<OpOutcome>;
    fn minimize_window(&mut self, pid: i32, window_id: u32) -> Result<OpOutcome>;
    fn close_window(&mut self, pid: i32, window_id: u32) -> Result<OpOutcome>;

    /// Re-observe the world after a batch of mutations. Default: no-op for
    /// executors that already keep their state authoritative.
    fn observe(&mut self) -> Result<Option<WorldState>> {
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// execute_plan
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default)]
pub struct ExecuteOptions {
    pub dry_run: bool,
}

pub fn execute_plan<E: Executor>(
    snapshot: &WorkspaceSnapshot,
    plan: &RestorePlan,
    executor: &mut E,
    options: ExecuteOptions,
) -> ExecutionJournal {
    let started_at = Utc::now();
    let start = Instant::now();
    let mut entries = Vec::with_capacity(plan.operations.len());

    for (index, op) in plan.operations.iter().enumerate() {
        let op_start = Instant::now();
        let op_started_at = Utc::now();
        let outcome = if options.dry_run {
            OpOutcome {
                status: JournalStatus::Skipped,
                message: format!("dry-run: would {}", describe(op)),
                attempts: 0,
            }
        } else {
            run_op(executor, snapshot, op)
        };

        entries.push(JournalEntry {
            op_index: index,
            op: op.kind.short_name().to_string(),
            app_name: op.app_name.clone(),
            bundle_id: op.bundle_id.clone(),
            saved_window_index: op.saved_window_index,
            status: outcome.status,
            started_at: op_started_at,
            duration_ms: duration_ms(op_start.elapsed()),
            message: outcome.message,
            attempts: outcome.attempts,
        });
    }

    ExecutionJournal {
        snapshot: snapshot.name.clone(),
        started_at,
        duration_ms: duration_ms(start.elapsed()),
        entries,
    }
}

fn duration_ms(d: Duration) -> u64 {
    d.as_millis().min(u128::from(u64::MAX)) as u64
}

fn describe(op: &PlannedOperation) -> String {
    match &op.kind {
        OperationKind::Reposition { target_frame, .. } => format!(
            "reposition {} to ({:.0},{:.0} {:.0}x{:.0})",
            op.app_name, target_frame.x, target_frame.y, target_frame.width, target_frame.height
        ),
        OperationKind::LaunchApp { bundle_id } => format!("launch {bundle_id}"),
        OperationKind::CreateWindow { bundle_id, .. } => format!("create window for {bundle_id}"),
        OperationKind::RestoreChromeTabs { .. } => "restore Chrome tabs".to_string(),
        OperationKind::MinimizeConflict { .. } => "minimize conflicting window".to_string(),
        OperationKind::CloseConflict { .. } => "close conflicting window".to_string(),
        OperationKind::Skip { reason } => format!("skip: {reason}"),
    }
}

fn saved_for_op<'a>(
    snapshot: &'a WorkspaceSnapshot,
    op: &PlannedOperation,
) -> std::result::Result<&'a WindowSnapshot, OpOutcome> {
    let saved_index = op.saved_window_index.ok_or_else(|| {
        OpOutcome::failed(format!("{} op missing saved index", op.kind.short_name()))
    })?;
    snapshot.windows.get(saved_index).ok_or_else(|| {
        OpOutcome::failed(format!(
            "{} op references invalid saved index {saved_index}",
            op.kind.short_name()
        ))
    })
}

fn run_op<E: Executor>(
    executor: &mut E,
    snapshot: &WorkspaceSnapshot,
    op: &PlannedOperation,
) -> OpOutcome {
    match &op.kind {
        OperationKind::Skip { reason } => OpOutcome::skipped(reason.clone()),
        OperationKind::LaunchApp { bundle_id } => match executor.launch_app(bundle_id) {
            Ok(o) => o,
            Err(e) => OpOutcome::failed(format!("launch failed: {e}")),
        },
        OperationKind::CreateWindow {
            bundle_id,
            target_frame,
        } => {
            let saved = match saved_for_op(snapshot, op) {
                Ok(saved) => saved,
                Err(outcome) => return outcome,
            };
            match executor.create_window(bundle_id, saved, *target_frame) {
                Ok(o) => o,
                Err(e) => OpOutcome::failed(format!("create_window failed: {e}")),
            }
        }
        OperationKind::Reposition {
            live_pid,
            live_window_id,
            target_frame,
        } => {
            let saved = match saved_for_op(snapshot, op) {
                Ok(saved) => saved,
                Err(outcome) => return outcome,
            };
            match executor.reposition(*live_pid, *live_window_id, saved, *target_frame) {
                Ok(o) => o,
                Err(e) => OpOutcome::failed(format!("reposition failed: {e}")),
            }
        }
        OperationKind::RestoreChromeTabs {
            bundle_id,
            target_frame,
        } => {
            // One op restores exactly one saved Chrome window at its
            // display-remapped target frame. Restoring the whole bundle here
            // would rewrite tabs of live windows other ops already matched.
            let saved = match saved_for_op(snapshot, op) {
                Ok(saved) => saved,
                Err(outcome) => return outcome,
            };
            match executor.restore_chrome_tabs(bundle_id, &[(saved, *target_frame)]) {
                Ok(o) => o,
                Err(e) => OpOutcome::failed(format!("chrome restore failed: {e}")),
            }
        }
        OperationKind::MinimizeConflict {
            live_pid,
            live_window_id,
        } => match executor.minimize_window(*live_pid, *live_window_id) {
            Ok(o) => o,
            Err(e) => OpOutcome::failed(format!("minimize failed: {e}")),
        },
        OperationKind::CloseConflict {
            live_pid,
            live_window_id,
        } => match executor.close_window(*live_pid, *live_window_id) {
            Ok(o) => o,
            Err(e) => OpOutcome::failed(format!("close failed: {e}")),
        },
    }
}

// ---------------------------------------------------------------------------
// SimulatedExecutor — pure, in-memory, for tests
// ---------------------------------------------------------------------------

/// In-memory executor that mutates a [`WorldState`] in place. Used to drive
/// the engine in tests without touching any OS APIs.
pub struct SimulatedExecutor {
    pub world: WorldState,
    pub next_window_id: u32,
    pub next_pid: i32,
    pub allow_launch: bool,
    pub reposition_drift: f64,
    pub launch_creates_window: bool,
    pub created_initial_frame: Frame,
}

impl SimulatedExecutor {
    pub fn new(world: WorldState) -> Self {
        let next_window_id = world.windows.iter().map(|w| w.window_id).max().unwrap_or(0) + 1;
        let next_pid = world.windows.iter().map(|w| w.pid).max().unwrap_or(100) + 1;
        Self {
            world,
            next_window_id,
            next_pid,
            allow_launch: true,
            reposition_drift: 0.0,
            launch_creates_window: true,
            created_initial_frame: Frame {
                x: 0.0,
                y: 0.0,
                width: 800.0,
                height: 600.0,
            },
        }
    }

    fn alloc_pid(&mut self) -> i32 {
        let pid = self.next_pid;
        self.next_pid += 1;
        pid
    }

    fn alloc_window_id(&mut self) -> u32 {
        let id = self.next_window_id;
        self.next_window_id += 1;
        id
    }
}

impl Executor for SimulatedExecutor {
    fn launch_app(&mut self, bundle_id: &str) -> Result<OpOutcome> {
        if !self.allow_launch {
            return Ok(OpOutcome::failed(format!(
                "simulated launch disabled for {bundle_id}"
            )));
        }
        let pid = self.alloc_pid();
        self.world
            .running_pids
            .entry(bundle_id.to_string())
            .or_default()
            .push(pid);
        if self.launch_creates_window {
            let window_id = self.alloc_window_id();
            self.world.windows.push(LiveWindow {
                bundle_id: Some(bundle_id.to_string()),
                app_name: bundle_id.to_string(),
                pid,
                window_id,
                title: Some("launched".to_string()),
                frame: self.created_initial_frame,
                minimized: false,
            });
        }
        Ok(OpOutcome::success(format!(
            "launched {bundle_id} pid={pid}"
        )))
    }

    fn create_window(
        &mut self,
        bundle_id: &str,
        saved: &WindowSnapshot,
        target: Frame,
    ) -> Result<OpOutcome> {
        let pid = self
            .world
            .pids_for(bundle_id)
            .first()
            .copied()
            .unwrap_or_else(|| {
                let pid = self.alloc_pid();
                self.world
                    .running_pids
                    .entry(bundle_id.to_string())
                    .or_default()
                    .push(pid);
                pid
            });
        let window_id = self.alloc_window_id();
        self.world.windows.push(LiveWindow {
            bundle_id: Some(bundle_id.to_string()),
            app_name: bundle_id.to_string(),
            pid,
            window_id,
            title: saved.title.clone().or_else(|| Some("created".to_string())),
            frame: target,
            minimized: false,
        });
        Ok(OpOutcome::success(format!(
            "created window {window_id} for {bundle_id}"
        )))
    }

    fn reposition(
        &mut self,
        pid: i32,
        window_id: u32,
        _saved: &WindowSnapshot,
        target: Frame,
    ) -> Result<OpOutcome> {
        for live in &mut self.world.windows {
            if live.pid == pid && live.window_id == window_id {
                let drift = self.reposition_drift;
                live.frame = Frame {
                    x: target.x + drift,
                    y: target.y + drift,
                    width: target.width,
                    height: target.height,
                };
                live.minimized = false;
                return Ok(OpOutcome::success(format!(
                    "moved window {window_id} to ({:.0},{:.0})",
                    live.frame.x, live.frame.y
                )));
            }
        }
        Ok(OpOutcome::failed(format!(
            "no live window pid={pid} id={window_id} to reposition"
        )))
    }

    fn restore_chrome_tabs(
        &mut self,
        bundle_id: &str,
        windows: &[(&WindowSnapshot, Frame)],
    ) -> Result<OpOutcome> {
        // Mirror the real semantics: create new windows at their target
        // frames without touching any existing live windows.
        let pid = self
            .world
            .pids_for(bundle_id)
            .first()
            .copied()
            .unwrap_or_else(|| {
                let pid = self.alloc_pid();
                self.world
                    .running_pids
                    .entry(bundle_id.to_string())
                    .or_default()
                    .push(pid);
                pid
            });
        for (saved, target_frame) in windows {
            let window_id = self.alloc_window_id();
            self.world.windows.push(LiveWindow {
                bundle_id: Some(bundle_id.to_string()),
                app_name: bundle_id.to_string(),
                pid,
                window_id,
                title: saved.title.clone(),
                frame: *target_frame,
                minimized: false,
            });
        }
        Ok(OpOutcome::success(format!(
            "rebuilt {} chrome window(s)",
            windows.len()
        )))
    }

    fn minimize_window(&mut self, pid: i32, window_id: u32) -> Result<OpOutcome> {
        for live in &mut self.world.windows {
            if live.pid == pid && live.window_id == window_id {
                live.minimized = true;
                return Ok(OpOutcome::success(format!("minimized {window_id}")));
            }
        }
        Ok(OpOutcome::failed("no such window"))
    }

    fn close_window(&mut self, pid: i32, window_id: u32) -> Result<OpOutcome> {
        let before = self.world.windows.len();
        self.world
            .windows
            .retain(|w| !(w.pid == pid && w.window_id == window_id));
        if self.world.windows.len() < before {
            Ok(OpOutcome::success(format!("closed {window_id}")))
        } else {
            Ok(OpOutcome::failed("no such window"))
        }
    }

    fn observe(&mut self) -> Result<Option<WorldState>> {
        Ok(Some(self.world.clone()))
    }
}

// ---------------------------------------------------------------------------
// MacOsExecutor (real-world)
// ---------------------------------------------------------------------------

/// A real-world [`Executor`] that drives the macOS Accessibility / NSWorkspace
/// stack. It needs an up-to-date [`WorldState`] (typically the planner's
/// `observed_world`) so it can map `(pid, window_id)` pairs back to the
/// AX-visible title and frame required for window matching.
#[cfg(target_os = "macos")]
pub struct MacOsExecutor {
    world: WorldState,
    /// Windows that appeared when this run launched an app and have not yet
    /// been claimed by a CreateWindow op. Launching an app usually opens one
    /// or more windows on its own (VS Code restores whole workspaces);
    /// CreateWindow ops adopt these instead of stacking extra windows on top.
    adoptable_windows: std::collections::HashMap<String, Vec<u32>>,
}

#[cfg(target_os = "macos")]
const LAUNCH_WAIT_ATTEMPTS: usize = 40;
#[cfg(target_os = "macos")]
const LAUNCH_WAIT_INTERVAL: Duration = Duration::from_millis(100);
#[cfg(target_os = "macos")]
const WINDOW_WAIT_ATTEMPTS: usize = 40;
#[cfg(target_os = "macos")]
const WINDOW_WAIT_INTERVAL: Duration = Duration::from_millis(100);

#[cfg(target_os = "macos")]
impl MacOsExecutor {
    pub fn new(world: WorldState) -> Self {
        Self {
            world,
            adoptable_windows: std::collections::HashMap::new(),
        }
    }

    /// System Events names processes by their displayed app name ("Code",
    /// "Google Chrome"), not the executable name captured in snapshots
    /// ("Electron"). Resolve it from the running app; fall back to the saved
    /// process name.
    fn system_events_process_name(bundle_id: &str, fallback: &str) -> String {
        crate::macos::app::running_pids_for_bundle(bundle_id)
            .first()
            .and_then(|pid| crate::macos::app::application_for_pid(*pid))
            .and_then(|info| info.localized_name)
            .unwrap_or_else(|| fallback.to_string())
    }

    fn find_live(&self, pid: i32, window_id: u32) -> Option<&LiveWindow> {
        self.world
            .windows
            .iter()
            .find(|w| w.pid == pid && w.window_id == window_id)
    }

    /// Build a synthetic `WindowSnapshot` whose title + frame fields match a
    /// live window. Only those two fields are consulted by the AX matcher.
    fn synthetic_snapshot(live: &LiveWindow) -> WindowSnapshot {
        WindowSnapshot {
            window_id: live.window_id,
            app_name: live.app_name.clone(),
            process_name: live.app_name.clone(),
            bundle_id: live.bundle_id.clone(),
            pid: live.pid,
            title: live.title.clone(),
            frame: live.frame,
            display_id: None,
            display_frame: None,
            display_relative_frame: None,
            z_order: None,
            fullscreen: false,
            minimized: live.minimized,
            enabled: true,
            browser_tabs: vec![],
        }
    }

    /// Current CG windows belonging to a bundle, front-to-back, junk filtered.
    fn cg_windows_for_bundle(bundle_id: &str) -> Vec<crate::macos::window::RawWindow> {
        let pids: std::collections::HashSet<i32> =
            crate::macos::app::running_pids_for_bundle(bundle_id)
                .into_iter()
                .collect();
        if pids.is_empty() {
            return Vec::new();
        }
        crate::macos::window::enumerate_windows()
            .unwrap_or_default()
            .into_iter()
            .filter(|raw| pids.contains(&raw.owner_pid))
            .filter(crate::filter::should_capture_window)
            .collect()
    }

    fn wait_for_any_window(bundle_id: &str) -> Option<crate::macos::window::RawWindow> {
        for attempt in 0..WINDOW_WAIT_ATTEMPTS {
            let mut windows = Self::cg_windows_for_bundle(bundle_id);
            if !windows.is_empty() {
                return Some(windows.remove(0));
            }
            if attempt + 1 < WINDOW_WAIT_ATTEMPTS {
                std::thread::sleep(WINDOW_WAIT_INTERVAL);
            }
        }
        None
    }

    /// Move a specific CG window (identified before/after Cmd+N) to `target`
    /// by matching its live title + frame through the AX matcher.
    fn position_raw_window(
        bundle_id: &str,
        raw: &crate::macos::window::RawWindow,
        target: Frame,
        verb: &str,
    ) -> OpOutcome {
        let live = LiveWindow {
            bundle_id: Some(bundle_id.to_string()),
            app_name: raw.owner_name.clone(),
            pid: raw.owner_pid,
            window_id: raw.window_id,
            title: raw.window_title.clone(),
            frame: raw.frame,
            minimized: false,
        };
        let synthetic = Self::synthetic_snapshot(&live);
        match crate::macos::accessibility::set_window_frame(raw.owner_pid, &synthetic, target) {
            Ok(true) => OpOutcome::success(format!("{verb} and positioned")),
            Ok(false) => OpOutcome::partial(format!("{verb}; AX could not match it to position")),
            Err(e) => OpOutcome::partial(format!("{verb}; positioning failed: {e}")),
        }
    }
}

#[cfg(target_os = "macos")]
impl Executor for MacOsExecutor {
    fn launch_app(&mut self, bundle_id: &str) -> Result<OpOutcome> {
        match crate::macos::app::launch_bundle(bundle_id) {
            Ok(true) => {}
            Ok(false) => return Ok(OpOutcome::failed(format!("launch refused for {bundle_id}"))),
            Err(e) => return Ok(OpOutcome::failed(e.to_string())),
        }

        // Wait for the process to register with NSWorkspace so the ops that
        // follow (create/reposition) have something to act on.
        let mut attempts = 1u32;
        let mut running = false;
        for attempt in 0..LAUNCH_WAIT_ATTEMPTS {
            if !crate::macos::app::running_pids_for_bundle(bundle_id).is_empty() {
                running = true;
                break;
            }
            if attempt + 1 < LAUNCH_WAIT_ATTEMPTS {
                attempts += 1;
                std::thread::sleep(LAUNCH_WAIT_INTERVAL);
            }
        }
        if !running {
            return Ok(OpOutcome::partial(format!(
                "launched {bundle_id} but it did not register as running in time"
            ))
            .with_attempts(attempts));
        }

        // Best-effort: wait for its first window, then record every window
        // the launch produced so CreateWindow ops can adopt them.
        let has_window = Self::wait_for_any_window(bundle_id).is_some();
        let adoptable: Vec<u32> = Self::cg_windows_for_bundle(bundle_id)
            .iter()
            .map(|raw| raw.window_id)
            .collect();
        self.adoptable_windows
            .insert(bundle_id.to_string(), adoptable);
        if has_window {
            Ok(OpOutcome::success(format!("launched {bundle_id}")).with_attempts(attempts))
        } else {
            Ok(
                OpOutcome::partial(format!("launched {bundle_id}; no window appeared yet"))
                    .with_attempts(attempts),
            )
        }
    }

    fn create_window(
        &mut self,
        bundle_id: &str,
        saved: &WindowSnapshot,
        target: Frame,
    ) -> Result<OpOutcome> {
        // Adopt a window the app opened at launch instead of opening another
        // one on top of it; only Cmd+N once the launch pool is exhausted.
        if let Some(pool) = self.adoptable_windows.get_mut(bundle_id) {
            while let Some(window_id) = pool.pop() {
                if let Some(raw) = Self::cg_windows_for_bundle(bundle_id)
                    .into_iter()
                    .find(|raw| raw.window_id == window_id)
                {
                    return Ok(Self::position_raw_window(
                        bundle_id,
                        &raw,
                        target,
                        "adopted launch window",
                    ));
                }
            }
        }

        let before: std::collections::HashSet<u32> = Self::cg_windows_for_bundle(bundle_id)
            .iter()
            .map(|raw| raw.window_id)
            .collect();

        let process_name = Self::system_events_process_name(bundle_id, &saved.process_name);
        match crate::macos::app::create_new_window(bundle_id, &process_name) {
            Ok(true) => {}
            Ok(false) => {
                return Ok(OpOutcome::failed(format!(
                    "create_window refused for {bundle_id}"
                )))
            }
            Err(e) => return Ok(OpOutcome::failed(e.to_string())),
        }

        // Identify the new window by diffing CG window ids, then position it.
        let mut attempts = 1u32;
        let mut created = None;
        for attempt in 0..WINDOW_WAIT_ATTEMPTS {
            if let Some(raw) = Self::cg_windows_for_bundle(bundle_id)
                .into_iter()
                .find(|raw| !before.contains(&raw.window_id))
            {
                created = Some(raw);
                break;
            }
            if attempt + 1 < WINDOW_WAIT_ATTEMPTS {
                attempts += 1;
                std::thread::sleep(WINDOW_WAIT_INTERVAL);
            }
        }

        match created {
            Some(raw) => Ok(
                Self::position_raw_window(bundle_id, &raw, target, "created window")
                    .with_attempts(attempts),
            ),
            None => Ok(OpOutcome::partial(format!(
                "asked {bundle_id} (process {process_name}) for a new window but none appeared"
            ))
            .with_attempts(attempts)),
        }
    }

    fn reposition(
        &mut self,
        pid: i32,
        window_id: u32,
        saved: &WindowSnapshot,
        target: Frame,
    ) -> Result<OpOutcome> {
        // A minimized window can be AX-matched and "moved", but stays in the
        // Dock. Un-minimize it first so the reposition is actually visible.
        let live_frame = self.find_live(pid, window_id).map(|live| {
            if live.minimized {
                let synthetic = Self::synthetic_snapshot(live);
                let _ = crate::macos::accessibility::unminimize_window(pid, &synthetic);
            }
            live.frame
        });

        let tab_capable = crate::app_support::is_tab_capable(saved.bundle_id.as_deref());
        let bundle_id = saved.bundle_id.as_deref().unwrap_or_default();

        // Chromium's AX implementation intermittently rejects AXSize writes
        // (kAXErrorFailure -25200); scripting `bounds` is deterministic, so
        // prefer it for tab-capable browsers and fall back to AX.
        let positioned = if tab_capable {
            let from = live_frame.unwrap_or(saved.frame);
            match crate::macos::chrome::set_window_bounds(bundle_id, from, target) {
                Ok(true) => Ok(true),
                Ok(false) | Err(_) => {
                    crate::macos::accessibility::set_window_frame(pid, saved, target)
                }
            }
        } else {
            crate::macos::accessibility::set_window_frame(pid, saved, target)
        };

        match positioned {
            Ok(true) => {
                // A matched browser window keeps its identity, but the user
                // may have closed some of its saved tabs — re-open the missing
                // ones (add-only; nothing the user has open is touched).
                if tab_capable && !saved.browser_tabs.is_empty() {
                    return Ok(
                        match crate::macos::chrome::reconcile_window_tabs(bundle_id, saved, target)
                        {
                            Ok(Some(0)) => OpOutcome::success("repositioned".to_string()),
                            Ok(Some(n)) => OpOutcome::success(format!(
                                "repositioned; reopened {n} missing tab(s)"
                            )),
                            Ok(None) => OpOutcome::partial(
                                "repositioned; could not locate window to reconcile tabs"
                                    .to_string(),
                            ),
                            Err(e) => OpOutcome::partial(format!(
                                "repositioned; tab reconcile failed: {e}"
                            )),
                        },
                    );
                }
                Ok(OpOutcome::success("repositioned".to_string()))
            }
            Ok(false) => Ok(OpOutcome::partial("AX match failed".to_string())),
            Err(e) => Ok(OpOutcome::failed(e.to_string())),
        }
    }

    fn restore_chrome_tabs(
        &mut self,
        bundle_id: &str,
        windows: &[(&WindowSnapshot, Frame)],
    ) -> Result<OpOutcome> {
        match crate::macos::chrome::restore_windows(bundle_id, windows) {
            Ok(true) => Ok(OpOutcome::success(format!(
                "restored {} browser window(s)",
                windows.len()
            ))),
            Ok(false) => Ok(OpOutcome::partial(
                "chrome restore completed with per-window errors".to_string(),
            )),
            Err(e) => Ok(OpOutcome::failed(e.to_string())),
        }
    }

    fn minimize_window(&mut self, pid: i32, window_id: u32) -> Result<OpOutcome> {
        let Some(live) = self.find_live(pid, window_id) else {
            return Ok(OpOutcome::failed(
                "no cached live window for (pid, window_id)",
            ));
        };
        let saved = Self::synthetic_snapshot(live);
        match crate::macos::accessibility::minimize_window(pid, &saved) {
            Ok(true) => Ok(OpOutcome::success("minimized".to_string())),
            Ok(false) => Ok(OpOutcome::partial("AX match failed".to_string())),
            Err(e) => Ok(OpOutcome::failed(e.to_string())),
        }
    }

    fn close_window(&mut self, pid: i32, window_id: u32) -> Result<OpOutcome> {
        let Some(live) = self.find_live(pid, window_id) else {
            return Ok(OpOutcome::failed(
                "no cached live window for (pid, window_id)",
            ));
        };
        let saved = Self::synthetic_snapshot(live);
        match crate::macos::accessibility::close_window(pid, &saved) {
            Ok(true) => Ok(OpOutcome::success("closed".to_string())),
            Ok(false) => Ok(OpOutcome::partial("AX match failed".to_string())),
            Err(e) => Ok(OpOutcome::failed(e.to_string())),
        }
    }

    fn observe(&mut self) -> Result<Option<WorldState>> {
        // The caller should re-run `plan::observe_world` to refresh between
        // convergence iterations; we don't re-enumerate here.
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use chrono::Utc;

    use super::*;
    use crate::model::{HostInfo, SNAPSHOT_VERSION};
    use crate::plan::{plan_restore, PlanOptions, RestoreMode};

    fn frame(x: f64, y: f64) -> Frame {
        Frame {
            x,
            y,
            width: 800.0,
            height: 600.0,
        }
    }

    fn snapshot(windows: Vec<WindowSnapshot>) -> WorkspaceSnapshot {
        WorkspaceSnapshot {
            version: SNAPSHOT_VERSION,
            name: "exec".to_string(),
            created_at: Utc::now(),
            host: HostInfo {
                hostname: "h".to_string(),
                os: "macos".to_string(),
                arch: "aarch64".to_string(),
            },
            displays: Vec::new(),
            windows,
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

    fn empty_world() -> WorldState {
        WorldState {
            displays: Vec::new(),
            windows: Vec::new(),
            running_pids: HashMap::new(),
        }
    }

    #[test]
    fn simulated_executor_launches_and_repositions_to_match_snapshot() {
        let bundle = "com.apple.Terminal";
        let snap = snapshot(vec![saved(bundle, "main", frame(100.0, 200.0))]);
        let mut exec = SimulatedExecutor::new(empty_world());
        // Disable "launch creates window" so the planner emits a CreateWindow.
        exec.launch_creates_window = false;

        let plan = plan_restore(
            &snap,
            &exec.world,
            PlanOptions {
                mode: RestoreMode::Safe,
                dev_mode: false,
            },
            &[frame(100.0, 200.0)],
        );

        let journal = execute_plan(&snap, &plan, &mut exec, ExecuteOptions::default());

        let counts = journal.counts();
        assert_eq!(counts.failed, 0, "no ops should fail: {journal:?}");
        // The world should now contain exactly one Terminal window at the target.
        let live: Vec<_> = exec
            .world
            .windows
            .iter()
            .filter(|w| w.bundle_id.as_deref() == Some(bundle))
            .collect();
        assert_eq!(live.len(), 1);
        assert!((live[0].frame.x - 100.0).abs() < 1.0);
        assert!((live[0].frame.y - 200.0).abs() < 1.0);
    }

    #[test]
    fn replanning_after_executor_drift_converges() {
        // Reposition with a small drift; running plan+execute repeatedly
        // should still converge (the second iteration picks up the moved
        // window and re-issues a reposition to the exact target).
        let bundle = "com.apple.Terminal";
        let snap = snapshot(vec![saved(bundle, "main", frame(0.0, 0.0))]);
        let mut world = empty_world();
        world.windows.push(LiveWindow {
            bundle_id: Some(bundle.to_string()),
            app_name: bundle.to_string(),
            pid: 999,
            window_id: 1,
            title: Some("main".to_string()),
            frame: frame(500.0, 500.0),
            minimized: false,
        });
        world.running_pids.insert(bundle.to_string(), vec![999]);
        let mut exec = SimulatedExecutor::new(world);
        exec.reposition_drift = 0.0;

        for _ in 0..3 {
            let plan = plan_restore(
                &snap,
                &exec.world,
                PlanOptions {
                    mode: RestoreMode::Safe,
                    dev_mode: false,
                },
                &[frame(0.0, 0.0)],
            );
            execute_plan(&snap, &plan, &mut exec, ExecuteOptions::default());
        }

        let live = &exec.world.windows[0];
        assert!((live.frame.x - 0.0).abs() < 0.5);
        assert!((live.frame.y - 0.0).abs() < 0.5);
    }

    #[test]
    fn dry_run_does_not_mutate_world() {
        let bundle = "com.apple.Terminal";
        let snap = snapshot(vec![saved(bundle, "main", frame(0.0, 0.0))]);
        let mut exec = SimulatedExecutor::new(empty_world());

        let plan = plan_restore(
            &snap,
            &exec.world,
            PlanOptions {
                mode: RestoreMode::Safe,
                dev_mode: false,
            },
            &[frame(0.0, 0.0)],
        );
        let journal = execute_plan(&snap, &plan, &mut exec, ExecuteOptions { dry_run: true });

        assert!(
            exec.world.windows.is_empty(),
            "dry-run must not mutate world"
        );
        assert!(
            journal
                .entries
                .iter()
                .all(|e| e.status == JournalStatus::Skipped),
            "all dry-run entries must be Skipped"
        );
    }

    #[test]
    fn multiple_chrome_windows_restore_without_touching_matched_ones() {
        let bundle = "com.google.Chrome";
        let tab = |url: &str| crate::model::BrowserTab {
            title: None,
            url: url.to_string(),
            active: true,
        };
        let mut first = saved(bundle, "Docs", frame(0.0, 0.0));
        first.browser_tabs = vec![tab("https://example.com/docs")];
        let mut second = saved(bundle, "Search", frame(900.0, 0.0));
        second.browser_tabs = vec![tab("https://example.com/search")];
        let mut third = saved(bundle, "Mail", frame(0.0, 700.0));
        third.browser_tabs = vec![tab("https://example.com/mail")];
        let snap = snapshot(vec![first, second, third]);

        // One live Chrome window already matches "Docs"; the other two saved
        // windows have no live counterpart.
        let mut world = empty_world();
        world.windows.push(LiveWindow {
            bundle_id: Some(bundle.to_string()),
            app_name: bundle.to_string(),
            pid: 500,
            window_id: 7,
            title: Some("Docs".to_string()),
            frame: frame(10.0, 10.0),
            minimized: false,
        });
        world.running_pids.insert(bundle.to_string(), vec![500]);

        let plan = plan_restore(
            &snap,
            &world,
            PlanOptions {
                mode: RestoreMode::Safe,
                dev_mode: false,
            },
            &[frame(0.0, 0.0), frame(900.0, 0.0), frame(0.0, 700.0)],
        );

        let chrome_ops = plan
            .operations
            .iter()
            .filter(|op| matches!(op.kind, OperationKind::RestoreChromeTabs { .. }))
            .count();
        assert_eq!(chrome_ops, 2, "unmatched Chrome windows restore via tabs");

        let mut exec = SimulatedExecutor::new(world);
        let journal = execute_plan(&snap, &plan, &mut exec, ExecuteOptions::default());
        assert_eq!(journal.counts().failed, 0, "{journal:?}");

        // The matched live window survives (same id) and exactly two new
        // windows appear — nothing gets rebuilt or clobbered.
        let chrome_windows: Vec<_> = exec
            .world
            .windows
            .iter()
            .filter(|w| w.bundle_id.as_deref() == Some(bundle))
            .collect();
        assert_eq!(chrome_windows.len(), 3, "{chrome_windows:?}");
        assert!(
            chrome_windows.iter().any(|w| w.window_id == 7),
            "matched window must not be destroyed"
        );
    }

    #[test]
    fn destructive_mode_executes_close_through_executor() {
        let bundle = "com.apple.Terminal";
        let snap = snapshot(vec![saved(bundle, "main", frame(0.0, 0.0))]);
        let mut world = empty_world();
        world.windows.push(LiveWindow {
            bundle_id: Some(bundle.to_string()),
            app_name: bundle.to_string(),
            pid: 999,
            window_id: 1,
            title: Some("main".to_string()),
            frame: frame(0.0, 0.0),
            minimized: false,
        });
        world.windows.push(LiveWindow {
            bundle_id: Some(bundle.to_string()),
            app_name: bundle.to_string(),
            pid: 999,
            window_id: 2,
            title: Some("extra".to_string()),
            frame: frame(500.0, 500.0),
            minimized: false,
        });
        world.running_pids.insert(bundle.to_string(), vec![999]);
        let mut exec = SimulatedExecutor::new(world);

        let plan = plan_restore(
            &snap,
            &exec.world,
            PlanOptions {
                mode: RestoreMode::Destructive,
                dev_mode: false,
            },
            &[frame(0.0, 0.0)],
        );
        execute_plan(&snap, &plan, &mut exec, ExecuteOptions::default());

        // After destructive execution only the matched window remains.
        let terminal_windows: Vec<_> = exec
            .world
            .windows
            .iter()
            .filter(|w| w.bundle_id.as_deref() == Some(bundle))
            .collect();
        assert_eq!(terminal_windows.len(), 1);
        assert_eq!(terminal_windows[0].window_id, 1);
    }
}
