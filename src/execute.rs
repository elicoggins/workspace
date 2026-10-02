//! Run restore plans and record each operation in an execution journal.
//!
//! The macOS executor uses retained window handles; the simulator updates an
//! in-memory world for tests.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

#[cfg(target_os = "macos")]
use std::collections::HashSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    error::Result,
    model::{Frame, WindowSnapshot, WorkspaceSnapshot},
    plan::{LiveWindow, OperationKind, PlannedOperation, RestorePlan, WorldState},
};

#[cfg(target_os = "macos")]
use crate::macos::{
    accessibility::{self, WindowHandle},
    app, chrome,
    window::{self, RawWindow},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalStatus {
    /// The executor reported success.
    Success,
    /// Attempted, but the result was not fully confirmed.
    PartialSuccess,
    /// Not attempted; see the journal message for the reason.
    Skipped,
    /// The operation failed.
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
    /// All planned actions succeeded; intentional planner skips are allowed.
    pub fn succeeded(&self, plan: &RestorePlan) -> bool {
        self.entries.len() == plan.operations.len()
            && plan
                .operations
                .iter()
                .zip(&self.entries)
                .all(|(op, entry)| {
                    matches!(op.kind, OperationKind::Skip { .. })
                        || entry.status == JournalStatus::Success
                })
    }

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

/// Executor result and attempt count, copied into the journal.
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

    /// Return updated observations, if the executor maintains them.
    fn observe(&mut self) -> Result<Option<WorldState>> {
        Ok(None)
    }
}

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
    if options.dry_run {
        return preview_plan(snapshot, plan);
    }

    // Cleanup is allowed only after every restore prerequisite for that app
    // has succeeded. Count ahead so premature cleanup also fails closed.
    let mut progress: HashMap<&str, AppRestoreProgress> = HashMap::new();
    for op in &plan.operations {
        if is_restore_operation(&op.kind) {
            if let Some(bundle) = operation_bundle(op) {
                progress.entry(bundle).or_default().pending += 1;
            }
        }
    }

    journal_plan(snapshot, plan, |op| {
        let bundle = operation_bundle(op);
        let app = bundle.and_then(|bundle| progress.get(bundle));
        let cleanup = matches!(
            op.kind,
            OperationKind::MinimizeConflict { .. } | OperationKind::CloseConflict { .. }
        );
        let outcome = if cleanup && !app.is_some_and(|app| app.pending == 0 && !app.unconfirmed) {
            OpOutcome::skipped("cleanup skipped: restore prerequisites are not confirmed")
        } else if is_restore_operation(&op.kind) && app.is_some_and(|app| app.launch_unconfirmed) {
            OpOutcome::skipped("restore skipped: app launch was not confirmed")
        } else {
            run_op(executor, snapshot, op)
        };

        if is_restore_operation(&op.kind) {
            if let Some(app) = bundle.and_then(|bundle| progress.get_mut(bundle)) {
                app.pending -= 1;
                if outcome.status != JournalStatus::Success {
                    app.unconfirmed = true;
                    if matches!(op.kind, OperationKind::LaunchApp { .. }) {
                        app.launch_unconfirmed = true;
                    }
                }
            }
        }
        outcome
    })
}

#[derive(Default)]
struct AppRestoreProgress {
    pending: usize,
    unconfirmed: bool,
    launch_unconfirmed: bool,
}

fn is_restore_operation(kind: &OperationKind) -> bool {
    matches!(
        kind,
        OperationKind::LaunchApp { .. }
            | OperationKind::CreateWindow { .. }
            | OperationKind::Reposition { .. }
            | OperationKind::RestoreChromeTabs { .. }
    )
}

fn operation_bundle(op: &PlannedOperation) -> Option<&str> {
    match &op.kind {
        OperationKind::LaunchApp { bundle_id }
        | OperationKind::CreateWindow { bundle_id, .. }
        | OperationKind::RestoreChromeTabs { bundle_id, .. } => Some(bundle_id),
        _ => op.bundle_id.as_deref(),
    }
}

/// Describe a plan without constructing or calling an executor.
pub fn preview_plan(snapshot: &WorkspaceSnapshot, plan: &RestorePlan) -> ExecutionJournal {
    journal_plan(snapshot, plan, |op| {
        OpOutcome::skipped(format!("dry-run: would {}", describe(op)))
    })
}

fn journal_plan(
    snapshot: &WorkspaceSnapshot,
    plan: &RestorePlan,
    mut outcome_for: impl FnMut(&PlannedOperation) -> OpOutcome,
) -> ExecutionJournal {
    let started_at = Utc::now();
    let start = Instant::now();
    let mut entries = Vec::with_capacity(plan.operations.len());

    for (index, op) in plan.operations.iter().enumerate() {
        let op_start = Instant::now();
        let op_started_at = Utc::now();
        let outcome = outcome_for(op);

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
            // Keep browser restoration scoped to this saved window; other
            // operations may already have claimed windows from the same app.
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

/// Test executor backed by an in-memory world.
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
        let next_window_id = world
            .windows
            .iter()
            .map(|w| w.window_id)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .unwrap_or(1);
        let next_pid = world
            .windows
            .iter()
            .map(|w| w.pid)
            .chain(world.running_pids.values().flatten().copied())
            .max()
            .unwrap_or(100)
            .checked_add(1)
            .unwrap_or(1);
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
        loop {
            let pid = self.next_pid.max(1);
            self.next_pid = pid.checked_add(1).unwrap_or(1);
            if !self.world.windows.iter().any(|window| window.pid == pid)
                && !self
                    .world
                    .running_pids
                    .values()
                    .flatten()
                    .any(|p| *p == pid)
            {
                return pid;
            }
        }
    }

    fn alloc_window_id(&mut self) -> u32 {
        loop {
            let id = self.next_window_id.max(1);
            self.next_window_id = id.checked_add(1).unwrap_or(1);
            if !self
                .world
                .windows
                .iter()
                .any(|window| window.window_id == id)
            {
                return id;
            }
        }
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

/// macOS executor. Window identities are resolved before mutation and retained
/// for movement, cleanup, and stacking order.
#[cfg(target_os = "macos")]
pub struct MacOsExecutor {
    world: WorldState,
    /// Unclaimed windows opened during launch. Creating a window consumes
    /// these first, since apps often open a window on their own.
    adoptable_windows: HashMap<String, Vec<u32>>,
    window_handles: HashMap<(i32, u32), WindowHandle>,
    known_handles: Vec<WindowHandle>,
    browser_window_ids: HashMap<(i32, u32), i64>,
    restored_windows: HashMap<u32, (String, WindowHandle)>,
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
        let mut executor = Self {
            world,
            adoptable_windows: HashMap::new(),
            window_handles: HashMap::new(),
            known_handles: Vec::new(),
            browser_window_ids: HashMap::new(),
            restored_windows: HashMap::new(),
        };
        executor.bind_observed_windows();
        executor
    }

    fn bind_observed_windows(&mut self) {
        let mut by_pid: HashMap<i32, Vec<&LiveWindow>> = HashMap::new();
        let mut by_browser: HashMap<&str, Vec<&LiveWindow>> = HashMap::new();
        for live in &self.world.windows {
            let observed = Self::synthetic_snapshot(live);
            if crate::plan::restore_skip_reason(&observed).is_some() {
                continue;
            }
            by_pid.entry(live.pid).or_default().push(live);
            if crate::app_support::is_tab_capable(live.bundle_id.as_deref()) {
                if let Some(bundle) = live.bundle_id.as_deref() {
                    by_browser.entry(bundle).or_default().push(live);
                }
            }
        }
        for (pid, live) in by_pid {
            if let Ok(handles) = accessibility::window_handles(pid) {
                self.known_handles.extend(handles);
            }
            let observed: Vec<_> = live
                .iter()
                .map(|window| Self::synthetic_snapshot(window))
                .collect();
            match accessibility::resolve_windows(pid, &observed) {
                Ok(handles) => {
                    for (window, handle) in live.into_iter().zip(handles) {
                        if let Some(handle) = handle {
                            self.window_handles.insert((pid, window.window_id), handle);
                        }
                    }
                }
                Err(error) => tracing::debug!(pid, %error, "AX window identity lookup failed"),
            }
        }
        for (bundle, live) in by_browser {
            let observed: Vec<_> = live
                .iter()
                .map(|window| Self::synthetic_snapshot(window))
                .collect();
            match chrome::resolve_window_ids(bundle, &observed) {
                Ok(ids) => {
                    for (window, id) in live.into_iter().zip(ids) {
                        if let Some(id) = id {
                            self.browser_window_ids
                                .insert((window.pid, window.window_id), id);
                        }
                    }
                }
                Err(error) => {
                    tracing::debug!(bundle, %error, "browser window identity lookup failed")
                }
            }
        }
    }

    fn remember_restored(&mut self, saved: &WindowSnapshot, handle: WindowHandle) {
        if let Some(bundle) = &saved.bundle_id {
            self.restored_windows
                .insert(saved.window_id, (bundle.clone(), handle));
        }
    }

    /// Raise only windows this executor actually restored, using the same
    /// retained handles. Disappeared windows never select a replacement.
    pub fn replay_z_order(&self, snapshot: &WorkspaceSnapshot) {
        let mut windows: Vec<_> = snapshot.windows.iter().collect();
        windows.sort_by_key(|window| std::cmp::Reverse(window.z_order.unwrap_or(u32::MAX)));
        for saved in windows {
            if crate::plan::restore_skip_reason(saved).is_some()
                || snapshot
                    .windows
                    .iter()
                    .filter(|window| window.window_id == saved.window_id)
                    .count()
                    != 1
            {
                continue;
            }
            if let Some((bundle, handle)) = self.restored_windows.get(&saved.window_id) {
                let _ = app::activate_bundle(bundle);
                if let Err(error) = handle.raise() {
                    tracing::debug!(%error, "raising restored window failed");
                }
            }
        }
    }

    /// System Events uses display names such as "Code", while captured
    /// process names can be "Electron".
    fn system_events_process_name(bundle_id: &str, fallback: &str) -> String {
        app::running_pids_for_bundle(bundle_id)
            .first()
            .and_then(|pid| app::application_for_pid(*pid))
            .and_then(|info| info.localized_name)
            .unwrap_or_else(|| fallback.to_string())
    }

    fn find_live(&self, pid: i32, window_id: u32) -> Option<&LiveWindow> {
        self.world
            .windows
            .iter()
            .find(|w| w.pid == pid && w.window_id == window_id)
    }

    /// Adapt live observations to the AX resolver's snapshot input.
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

    /// Capturable CG windows for this app, in front-to-back order.
    fn cg_windows_for_bundle(bundle_id: &str) -> Vec<RawWindow> {
        let pids: HashSet<i32> = app::running_pids_for_bundle(bundle_id)
            .into_iter()
            .collect();
        if pids.is_empty() {
            return Vec::new();
        }
        window::enumerate_windows()
            .unwrap_or_default()
            .into_iter()
            .filter(|raw| pids.contains(&raw.owner_pid))
            .filter(crate::filter::should_capture_window)
            .collect()
    }

    fn wait_for_any_window(bundle_id: &str) -> Option<RawWindow> {
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
    /// by resolving its observed live state once and retaining its handle.
    fn position_raw_window(
        &mut self,
        bundle_id: &str,
        raw: &RawWindow,
        saved: &WindowSnapshot,
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
        let handle = match accessibility::resolve_window(raw.owner_pid, &synthetic) {
            Ok(Some(handle)) => handle,
            Ok(None) => {
                return OpOutcome::partial(format!("{verb}; AX identity is missing or ambiguous"))
            }
            Err(error) => return OpOutcome::partial(format!("{verb}; AX lookup failed: {error}")),
        };
        if self.handle_is_known(&handle) {
            return OpOutcome::partial(format!(
                "{verb}; AX identity belongs to an existing window"
            ));
        }
        match handle.set_frame(target) {
            Ok(true) => {
                self.remember_restored(saved, handle);
                OpOutcome::success(format!("{verb} and positioned"))
            }
            Ok(false) => OpOutcome::partial(format!("{verb}; position could not be verified")),
            Err(e) => OpOutcome::partial(format!("{verb}; positioning failed: {e}")),
        }
    }

    fn handle_is_known(&self, handle: &WindowHandle) -> bool {
        self.known_handles
            .iter()
            .any(|known| known.same_window(handle))
            || self
                .window_handles
                .values()
                .any(|known| known.same_window(handle))
            || self
                .restored_windows
                .values()
                .any(|(_, known)| known.same_window(handle))
    }

    fn remember_existing_handles(&mut self, bundle_id: &str) -> Result<()> {
        for pid in app::running_pids_for_bundle(bundle_id) {
            self.known_handles
                .extend(accessibility::window_handles(pid)?);
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
impl Executor for MacOsExecutor {
    fn launch_app(&mut self, bundle_id: &str) -> Result<OpOutcome> {
        self.remember_existing_handles(bundle_id)?;
        let before: HashSet<_> = Self::cg_windows_for_bundle(bundle_id)
            .into_iter()
            .map(|window| window.window_id)
            .collect();
        match app::launch_bundle(bundle_id) {
            Ok(true) => {}
            Ok(false) => return Ok(OpOutcome::failed(format!("launch refused for {bundle_id}"))),
            Err(e) => return Ok(OpOutcome::failed(e.to_string())),
        }

        // NSWorkspace can register the process after the launch call returns.
        let mut attempts = 1u32;
        let mut running = false;
        for attempt in 0..LAUNCH_WAIT_ATTEMPTS {
            if !app::running_pids_for_bundle(bundle_id).is_empty() {
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

        // Keep launch-created windows available for subsequent create ops.
        let has_window = Self::wait_for_any_window(bundle_id).is_some();
        let adoptable: Vec<u32> = Self::cg_windows_for_bundle(bundle_id)
            .iter()
            .filter(|raw| !before.contains(&raw.window_id))
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
        while let Some(window_id) = self.adoptable_windows.get_mut(bundle_id).and_then(Vec::pop) {
            if let Some(raw) = Self::cg_windows_for_bundle(bundle_id)
                .into_iter()
                .find(|raw| raw.window_id == window_id)
            {
                return Ok(self.position_raw_window(
                    bundle_id,
                    &raw,
                    saved,
                    target,
                    "adopted launch window",
                ));
            }
        }

        let before: HashSet<u32> = Self::cg_windows_for_bundle(bundle_id)
            .iter()
            .map(|raw| raw.window_id)
            .collect();

        self.remember_existing_handles(bundle_id)?;

        let process_name = Self::system_events_process_name(bundle_id, &saved.process_name);
        match app::create_new_window(bundle_id, &process_name) {
            Ok(true) => {}
            Ok(false) => {
                return Ok(OpOutcome::failed(format!(
                    "create_window refused for {bundle_id}"
                )))
            }
            Err(e) => return Ok(OpOutcome::failed(e.to_string())),
        }

        let mut attempts = 1u32;
        let mut created = None;
        for attempt in 0..WINDOW_WAIT_ATTEMPTS {
            let mut added: Vec<_> = Self::cg_windows_for_bundle(bundle_id)
                .into_iter()
                .filter(|raw| !before.contains(&raw.window_id))
                .collect();
            if added.len() > 1 {
                return Ok(OpOutcome::partial(
                    "multiple new windows appeared; creation ownership is ambiguous",
                )
                .with_attempts(attempts));
            }
            if let Some(raw) = added.pop() {
                created = Some(raw);
                break;
            }
            if attempt + 1 < WINDOW_WAIT_ATTEMPTS {
                attempts += 1;
                std::thread::sleep(WINDOW_WAIT_INTERVAL);
            }
        }

        match created {
            Some(raw) => Ok(self
                .position_raw_window(bundle_id, &raw, saved, target, "created window")
                .with_attempts(attempts)),
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
        let Some(live) = self.find_live(pid, window_id).cloned() else {
            return Ok(OpOutcome::failed("planned live window is missing"));
        };
        if live.bundle_id != saved.bundle_id {
            return Ok(OpOutcome::failed(
                "planned live window belongs to a different app",
            ));
        }
        let Some(handle) = self.window_handles.get(&(pid, window_id)).cloned() else {
            return Ok(OpOutcome::skipped(
                "planned AX identity is missing or ambiguous",
            ));
        };
        if handle.is_fullscreen() {
            return Ok(OpOutcome::skipped(
                "planned window entered fullscreen; leaving it alone",
            ));
        }
        let tab_capable = crate::app_support::is_tab_capable(saved.bundle_id.as_deref());
        let bundle_id = saved.bundle_id.as_deref().unwrap_or_default();
        let browser_id = self.browser_window_ids.get(&(pid, window_id)).copied();
        if tab_capable && browser_id.is_none() {
            return Ok(OpOutcome::skipped(
                "planned browser identity is missing or ambiguous",
            ));
        }
        if live.minimized {
            match handle.set_minimized(false) {
                Ok(true) => {}
                Ok(false) => return Ok(OpOutcome::partial("unminimizing could not be verified")),
                Err(error) => {
                    return Ok(OpOutcome::failed(format!("unminimizing failed: {error}")))
                }
            }
        }

        // Chromium can reject AXSize writes with -25200. Prefer scripting
        // bounds, with AX as a fallback on the same retained window.
        let positioned = if let Some(browser_id) = browser_id {
            match chrome::set_window_bounds(bundle_id, browser_id, target) {
                Ok(true) => Ok(true),
                Ok(false) | Err(_) => handle.set_frame(target),
            }
        } else {
            handle.set_frame(target)
        };

        match positioned {
            Ok(true) => {
                self.remember_restored(saved, handle);
                if let Some(browser_id) = browser_id.filter(|_| !saved.browser_tabs.is_empty()) {
                    return Ok(
                        match chrome::reconcile_window_tabs(bundle_id, browser_id, saved) {
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
            Ok(false) => Ok(OpOutcome::partial(
                "position could not be verified".to_string(),
            )),
            Err(e) => Ok(OpOutcome::failed(e.to_string())),
        }
    }

    fn restore_chrome_tabs(
        &mut self,
        bundle_id: &str,
        windows: &[(&WindowSnapshot, Frame)],
    ) -> Result<OpOutcome> {
        self.remember_existing_handles(bundle_id)?;
        match chrome::restore_windows(bundle_id, windows) {
            Ok(result) => {
                for ((saved, _), restored) in windows.iter().zip(&result.windows) {
                    let Some(restored) = restored else {
                        continue;
                    };
                    let mut observed = (*saved).clone();
                    observed.title = restored.title.clone();
                    observed.frame = restored.frame;
                    observed.minimized = false;
                    let handles: Vec<_> = app::running_pids_for_bundle(bundle_id)
                        .into_iter()
                        .filter_map(|pid| {
                            accessibility::resolve_window(pid, &observed).ok().flatten()
                        })
                        .collect();
                    if handles.len() == 1 {
                        let handle = handles.into_iter().next().unwrap();
                        if !self.handle_is_known(&handle) {
                            self.remember_restored(saved, handle);
                        }
                    }
                }
                if result.errors.is_empty() && result.windows.iter().all(Option::is_some) {
                    Ok(OpOutcome::success(format!(
                        "restored {} browser window(s)",
                        windows.len()
                    )))
                } else {
                    Ok(OpOutcome::partial(format!(
                        "browser restore incomplete: {}",
                        result.errors.join("; ")
                    )))
                }
            }
            Err(e) => Ok(OpOutcome::failed(e.to_string())),
        }
    }

    fn minimize_window(&mut self, pid: i32, window_id: u32) -> Result<OpOutcome> {
        let Some(handle) = self.window_handles.get(&(pid, window_id)) else {
            return Ok(OpOutcome::skipped(
                "conflict AX identity is missing or ambiguous",
            ));
        };
        match handle.set_minimized(true) {
            Ok(true) => Ok(OpOutcome::success("minimized".to_string())),
            Ok(false) => Ok(OpOutcome::partial("AX match failed".to_string())),
            Err(e) => Ok(OpOutcome::failed(e.to_string())),
        }
    }

    fn close_window(&mut self, pid: i32, window_id: u32) -> Result<OpOutcome> {
        let Some(handle) = self.window_handles.get(&(pid, window_id)) else {
            return Ok(OpOutcome::skipped(
                "conflict AX identity is missing or ambiguous",
            ));
        };
        match handle.close() {
            Ok(true) => Ok(OpOutcome::success("closed".to_string())),
            Ok(false) => Ok(OpOutcome::partial("AX match failed".to_string())),
            Err(e) => Ok(OpOutcome::failed(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
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

    #[cfg(target_os = "macos")]
    fn unbound_native_executor(world: WorldState) -> MacOsExecutor {
        // No OS queries or native handles: test the real executor's gates.
        MacOsExecutor {
            world,
            adoptable_windows: HashMap::new(),
            window_handles: HashMap::new(),
            known_handles: Vec::new(),
            browser_window_ids: HashMap::new(),
            restored_windows: HashMap::new(),
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_executor_never_reselects_a_missing_planned_identity() {
        let saved = saved("com.google.Chrome", "tab", frame(0.0, 0.0));
        let live = LiveWindow {
            bundle_id: saved.bundle_id.clone(),
            app_name: saved.app_name.clone(),
            pid: 1234,
            window_id: 2,
            title: saved.title.clone(),
            frame: saved.frame,
            minimized: false,
        };
        let mut exec = unbound_native_executor(WorldState {
            windows: vec![live],
            ..empty_world()
        });
        let missing = exec
            .reposition(1234, 999, &saved, frame(40.0, 0.0))
            .unwrap();
        assert_eq!(missing.status, JournalStatus::Failed);
        let ambiguous = exec.reposition(1234, 2, &saved, frame(40.0, 0.0)).unwrap();
        assert_eq!(ambiguous.status, JournalStatus::Skipped);
        assert_eq!(ambiguous.attempts, 0);
        assert!(exec.restored_windows.is_empty());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_cleanup_without_a_retained_identity_is_skipped() {
        let mut exec = unbound_native_executor(empty_world());
        for outcome in [
            exec.minimize_window(1234, 2).unwrap(),
            exec.close_window(1234, 2).unwrap(),
        ] {
            assert_eq!(outcome.status, JournalStatus::Skipped);
            assert_eq!(outcome.attempts, 0);
        }
    }

    #[test]
    fn simulated_executor_launches_and_repositions_to_match_snapshot() {
        let bundle = "com.apple.Terminal";
        let snap = snapshot(vec![saved(bundle, "main", frame(100.0, 200.0))]);
        let mut exec = SimulatedExecutor::new(empty_world());
        // Exercise creation separately from launch-window adoption.
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
