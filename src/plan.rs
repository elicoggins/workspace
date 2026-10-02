//! Match saved windows to an observed world and build a restore plan.
//!
//! Planning makes no platform calls. Each operation carries a reason for the
//! journal and preview output.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::{
    app_support::{support_for_window, SupportLevel},
    model::{DisplaySnapshot, Frame, WindowSnapshot, WorkspaceSnapshot},
};

/// Policy for extra windows belonging to apps being restored.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RestoreMode {
    /// Move, launch, or create windows; leave extra windows alone.
    #[default]
    Safe,
    /// Also allow minimizing extra windows.
    Reconcile,
    /// Also allow closing extra windows.
    Destructive,
}

impl RestoreMode {
    pub fn allows_minimize(self) -> bool {
        matches!(self, RestoreMode::Reconcile | RestoreMode::Destructive)
    }

    pub fn allows_close(self) -> bool {
        matches!(self, RestoreMode::Destructive)
    }
}

/// An observed CG or AX window, including minimized windows.
#[derive(Debug, Clone, PartialEq)]
pub struct LiveWindow {
    pub bundle_id: Option<String>,
    pub app_name: String,
    pub pid: i32,
    pub window_id: u32,
    pub title: Option<String>,
    pub frame: Frame,
    pub minimized: bool,
}

#[derive(Debug, Clone, Default)]
pub struct WorldState {
    pub displays: Vec<DisplaySnapshot>,
    pub windows: Vec<LiveWindow>,
    /// PIDs known to be currently running, keyed by bundle id.
    pub running_pids: HashMap<String, Vec<i32>>,
}

impl WorldState {
    pub fn pids_for(&self, bundle_id: &str) -> Vec<i32> {
        self.running_pids
            .get(bundle_id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn is_running(&self, bundle_id: &str) -> bool {
        self.running_pids
            .get(bundle_id)
            .map(|pids| !pids.is_empty())
            .unwrap_or(false)
    }
}

/// Components of a saved-to-live match score.
#[derive(Debug, Copy, Clone, PartialEq, Serialize, Deserialize)]
pub struct MatchScore {
    pub title_similarity: f32,
    pub geometry_similarity: f32,
    pub bundle_match: bool,
    /// Whether both titles contained text after normalization.
    #[serde(default)]
    pub title_evidence: bool,
    pub final_score: f32,
}

impl MatchScore {
    pub const MIN_ACCEPT: f32 = 0.20;
    pub const TITLE_MIN_ACCEPT: f32 = 0.50;
    /// Stricter geometry threshold when either title is missing or blank.
    pub const GEOMETRY_ONLY_MIN: f32 = 0.99;

    pub fn is_acceptable(&self) -> bool {
        self.bundle_match
            && self.final_score >= Self::MIN_ACCEPT
            && ((self.title_evidence && self.title_similarity >= Self::TITLE_MIN_ACCEPT)
                || self.geometry_similarity >= Self::GEOMETRY_ONLY_MIN)
    }

    pub fn explain(&self) -> String {
        format!(
            "title={:.2} geometry={:.2} bundle={} final={:.2}",
            self.title_similarity, self.geometry_similarity, self.bundle_match, self.final_score
        )
    }
}

/// The concrete action the executor should attempt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OperationKind {
    /// Reuse an already-open live window and resize/move it.
    Reposition {
        live_pid: i32,
        live_window_id: u32,
        target_frame: Frame,
    },
    /// Launch the app before restoring its windows.
    LaunchApp { bundle_id: String },
    /// Ask the app for an additional window.
    CreateWindow {
        bundle_id: String,
        target_frame: Frame,
    },
    /// Open captured browser tabs in a new window.
    RestoreChromeTabs {
        bundle_id: String,
        target_frame: Frame,
    },
    /// Reconcile mode: minimize a live window that does not correspond to any
    /// saved window.
    MinimizeConflict { live_pid: i32, live_window_id: u32 },
    /// Destructive mode: close a live window that does not correspond to any
    /// saved window.
    CloseConflict { live_pid: i32, live_window_id: u32 },
    /// Leave this saved window untouched.
    Skip { reason: String },
}

impl OperationKind {
    pub fn is_destructive(&self) -> bool {
        matches!(
            self,
            OperationKind::CloseConflict { .. } | OperationKind::MinimizeConflict { .. }
        )
    }

    pub fn short_name(&self) -> &'static str {
        match self {
            OperationKind::Reposition { .. } => "reposition",
            OperationKind::LaunchApp { .. } => "launch",
            OperationKind::CreateWindow { .. } => "create",
            OperationKind::RestoreChromeTabs { .. } => "chrome_tabs",
            OperationKind::MinimizeConflict { .. } => "minimize",
            OperationKind::CloseConflict { .. } => "close",
            OperationKind::Skip { .. } => "skip",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlannedOperation {
    pub kind: OperationKind,
    pub saved_window_index: Option<usize>,
    pub app_name: String,
    pub bundle_id: Option<String>,
    pub rationale: String,
    pub match_score: Option<MatchScore>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RestorePlan {
    pub mode: RestoreMode,
    pub operations: Vec<PlannedOperation>,
    /// Saved window indices that the planner could not satisfy.
    pub unmatched_saved: Vec<usize>,
    /// Number of destructive (close/minimize) operations in the plan.
    pub destructive_ops: usize,
    /// Live windows that look like conflicts but were left alone.
    pub left_alone_conflicts: usize,
}

impl RestorePlan {
    pub fn summary(&self) -> PlanSummary {
        let mut summary = PlanSummary::default();
        for op in &self.operations {
            match &op.kind {
                OperationKind::Reposition { .. } => summary.reposition += 1,
                OperationKind::LaunchApp { .. } => summary.launch += 1,
                OperationKind::CreateWindow { .. } => summary.create += 1,
                OperationKind::RestoreChromeTabs { .. } => summary.chrome += 1,
                OperationKind::MinimizeConflict { .. } => summary.minimize += 1,
                OperationKind::CloseConflict { .. } => summary.close += 1,
                OperationKind::Skip { .. } => summary.skip += 1,
            }
        }
        summary
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanSummary {
    pub reposition: usize,
    pub launch: usize,
    pub create: usize,
    pub chrome: usize,
    pub minimize: usize,
    pub close: usize,
    pub skip: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PlanOptions {
    pub mode: RestoreMode,
    pub dev_mode: bool,
}

const DEV_MODE_PROTECTED_BUNDLES: &[&str] =
    &["com.microsoft.VSCode", "com.todesktop.230313mzl4w4u92"];

pub fn is_dev_mode_protected_bundle(bundle_id: &str) -> bool {
    DEV_MODE_PROTECTED_BUNDLES.contains(&bundle_id)
}

/// Policy exclusions shared by planning and verification.
pub fn restore_skip_reason(window: &WindowSnapshot) -> Option<String> {
    if !window.enabled {
        return Some("disabled in workspace configuration".to_string());
    }
    let support = support_for_window(window);
    if support.level != SupportLevel::FullRestore {
        return Some(support.reason.to_string());
    }
    if window.fullscreen {
        return Some("fullscreen windows are skipped".to_string());
    }
    None
}

/// Build a plan with one target frame per saved window.
pub fn plan_restore(
    snapshot: &WorkspaceSnapshot,
    world: &WorldState,
    options: PlanOptions,
    target_frames: &[Frame],
) -> RestorePlan {
    assert_eq!(
        target_frames.len(),
        snapshot.windows.len(),
        "planner expects one target frame per saved window"
    );

    let mut operations: Vec<PlannedOperation> = Vec::with_capacity(snapshot.windows.len());
    let mut unmatched_saved = Vec::new();
    let mut consumed_live: HashSet<u32> = HashSet::new();

    // Group saved windows by bundle so we can batch app-launch and per-app
    // matching decisions.
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    for (index, window) in snapshot.windows.iter().enumerate() {
        let key = group_key(window);
        if !groups.contains_key(&key) {
            order.push(key.clone());
        }
        groups.entry(key).or_default().push(index);
    }

    for key in &order {
        let indices = &groups[key];
        plan_group(
            snapshot,
            target_frames,
            world,
            indices,
            options,
            &mut operations,
            &mut unmatched_saved,
            &mut consumed_live,
        );
    }

    let (mut destructive_ops, mut left_alone_conflicts) = (0, 0);
    if options.mode.allows_minimize() || options.mode.allows_close() {
        plan_conflicts(
            snapshot,
            world,
            options,
            &consumed_live,
            &mut operations,
            &mut destructive_ops,
            &mut left_alone_conflicts,
        );
    } else {
        left_alone_conflicts = count_unconsumed_conflicts(snapshot, world, &consumed_live);
    }

    RestorePlan {
        mode: options.mode,
        operations,
        unmatched_saved,
        destructive_ops,
        left_alone_conflicts,
    }
}

fn group_key(window: &WindowSnapshot) -> String {
    window
        .bundle_id
        .clone()
        .unwrap_or_else(|| format!("pid:{}", window.pid))
}

#[allow(clippy::too_many_arguments)]
fn plan_group(
    snapshot: &WorkspaceSnapshot,
    target_frames: &[Frame],
    world: &WorldState,
    indices: &[usize],
    options: PlanOptions,
    operations: &mut Vec<PlannedOperation>,
    unmatched_saved: &mut Vec<usize>,
    consumed_live: &mut HashSet<u32>,
) {
    let first = &snapshot.windows[indices[0]];
    let bundle_id = first.bundle_id.clone();

    for &index in indices {
        let window = &snapshot.windows[index];
        if let Some(reason) = restore_skip_reason(window) {
            operations.push(skip(window, index, reason));
        }
    }

    let active_indices: Vec<usize> = indices
        .iter()
        .copied()
        .filter(|index| restore_skip_reason(&snapshot.windows[*index]).is_none())
        .collect();

    if active_indices.is_empty() {
        return;
    }

    let Some(bundle_id) = bundle_id else {
        for index in active_indices {
            let window = &snapshot.windows[index];
            unmatched_saved.push(index);
            operations.push(skip(window, index, "missing bundle identifier".to_string()));
        }
        return;
    };

    let app_running = world.is_running(&bundle_id);
    let dev_protected = options.dev_mode && is_dev_mode_protected_bundle(&bundle_id);
    if !app_running {
        if dev_protected {
            for index in active_indices {
                let window = &snapshot.windows[index];
                unmatched_saved.push(index);
                operations.push(skip(
                    window,
                    index,
                    "dev-mode: not launching protected editor".to_string(),
                ));
            }
            return;
        }
        operations.push(PlannedOperation {
            kind: OperationKind::LaunchApp {
                bundle_id: bundle_id.clone(),
            },
            saved_window_index: None,
            app_name: first.app_name.clone(),
            bundle_id: Some(bundle_id.clone()),
            rationale: format!("{} is not running; launching by bundle id", first.app_name),
            match_score: None,
        });
    }

    let live_candidates: Vec<&LiveWindow> = world
        .windows
        .iter()
        .filter(|window| window.bundle_id.as_deref() == Some(bundle_id.as_str()))
        .collect();

    // Skipped windows still participate in matching: their live counterparts
    // are reserved so neither reuse nor conflict cleanup can touch them.
    let saved: Vec<&WindowSnapshot> = indices
        .iter()
        .map(|index| &snapshot.windows[*index])
        .collect();

    let targets: Vec<_> = indices.iter().map(|index| target_frames[*index]).collect();
    let assignments = assign_matches(&saved, &targets, &live_candidates, consumed_live);

    let tab_capable = crate::app_support::is_tab_capable(Some(bundle_id.as_str()));

    for (&saved_index, assignment) in indices.iter().zip(assignments) {
        let window = &snapshot.windows[saved_index];
        if let Some((live_index, _)) = assignment {
            consumed_live.insert(live_candidates[live_index].window_id);
        }
        if restore_skip_reason(window).is_some() {
            continue;
        }
        let target_frame = target_frames[saved_index];

        if let Some((live_index, score)) = assignment {
            let live = live_candidates[live_index];
            operations.push(PlannedOperation {
                kind: OperationKind::Reposition {
                    live_pid: live.pid,
                    live_window_id: live.window_id,
                    target_frame,
                },
                saved_window_index: Some(saved_index),
                app_name: window.app_name.clone(),
                bundle_id: Some(bundle_id.clone()),
                rationale: format!(
                    "reusing existing window (live title={:?}; score {})",
                    live.title.as_deref().unwrap_or("?"),
                    score.explain()
                ),
                match_score: Some(score),
            });
        } else if tab_capable && !window.browser_tabs.is_empty() {
            operations.push(PlannedOperation {
                kind: OperationKind::RestoreChromeTabs {
                    bundle_id: bundle_id.clone(),
                    target_frame,
                },
                saved_window_index: Some(saved_index),
                app_name: window.app_name.clone(),
                bundle_id: Some(bundle_id.clone()),
                rationale: format!(
                    "creating browser window with {} captured tabs",
                    window.browser_tabs.len()
                ),
                match_score: None,
            });
        } else {
            operations.push(PlannedOperation {
                kind: OperationKind::CreateWindow {
                    bundle_id: bundle_id.clone(),
                    target_frame,
                },
                saved_window_index: Some(saved_index),
                app_name: window.app_name.clone(),
                bundle_id: Some(bundle_id.clone()),
                rationale: "no matching live window; will create a new one".to_string(),
                match_score: None,
            });
        }
    }
}

fn skip(window: &WindowSnapshot, index: usize, reason: String) -> PlannedOperation {
    PlannedOperation {
        kind: OperationKind::Skip {
            reason: reason.clone(),
        },
        saved_window_index: Some(index),
        app_name: window.app_name.clone(),
        bundle_id: window.bundle_id.clone(),
        rationale: reason,
        match_score: None,
    }
}

fn plan_conflicts(
    snapshot: &WorkspaceSnapshot,
    world: &WorldState,
    options: PlanOptions,
    consumed_live: &HashSet<u32>,
    operations: &mut Vec<PlannedOperation>,
    destructive_ops: &mut usize,
    left_alone_conflicts: &mut usize,
) {
    // Build the set of bundle ids the workspace is restoring -- we only act on
    // conflicts for apps the workspace owns, never on unrelated user windows.
    let owned_bundles = restorable_bundles(snapshot);

    for live in &world.windows {
        if consumed_live.contains(&live.window_id) {
            continue;
        }
        let Some(bundle_id) = live.bundle_id.as_deref() else {
            *left_alone_conflicts += 1;
            continue;
        };
        if !owned_bundles.contains(bundle_id)
            || (options.dev_mode && is_dev_mode_protected_bundle(bundle_id))
        {
            *left_alone_conflicts += 1;
            continue;
        }

        if options.mode.allows_close() {
            operations.push(PlannedOperation {
                kind: OperationKind::CloseConflict {
                    live_pid: live.pid,
                    live_window_id: live.window_id,
                },
                saved_window_index: None,
                app_name: live.app_name.clone(),
                bundle_id: Some(bundle_id.to_string()),
                rationale: format!(
                    "destructive mode: closing extra {} window not present in snapshot",
                    live.app_name
                ),
                match_score: None,
            });
            *destructive_ops += 1;
        } else if options.mode.allows_minimize() {
            operations.push(PlannedOperation {
                kind: OperationKind::MinimizeConflict {
                    live_pid: live.pid,
                    live_window_id: live.window_id,
                },
                saved_window_index: None,
                app_name: live.app_name.clone(),
                bundle_id: Some(bundle_id.to_string()),
                rationale: format!(
                    "reconcile mode: minimizing extra {} window not present in snapshot",
                    live.app_name
                ),
                match_score: None,
            });
            *destructive_ops += 1;
        } else {
            *left_alone_conflicts += 1;
        }
    }
}

fn count_unconsumed_conflicts(
    snapshot: &WorkspaceSnapshot,
    world: &WorldState,
    consumed_live: &HashSet<u32>,
) -> usize {
    let owned_bundles = restorable_bundles(snapshot);
    world
        .windows
        .iter()
        .filter(|live| !consumed_live.contains(&live.window_id))
        .filter(|live| {
            live.bundle_id
                .as_deref()
                .map(|bundle| owned_bundles.contains(bundle))
                .unwrap_or(false)
        })
        .count()
}

fn restorable_bundles(snapshot: &WorkspaceSnapshot) -> HashSet<&str> {
    snapshot
        .windows
        .iter()
        .filter(|window| restore_skip_reason(window).is_none())
        .filter_map(|window| window.bundle_id.as_deref())
        .collect()
}

/// Assign the highest-scoring acceptable pairs first, using each live window
/// at most once. Shared with verification to keep their assignments consistent.
/// Skipped windows reserve plausible geometry even when their titles changed.
pub(crate) fn assign_matches(
    saved: &[&WindowSnapshot],
    target_frames: &[Frame],
    live: &[&LiveWindow],
    already_consumed: &HashSet<u32>,
) -> Vec<Option<(usize, MatchScore)>> {
    const PROTECTED_GEOMETRY_MIN: f32 = 0.85;
    assert_eq!(saved.len(), target_frames.len());
    let skipped: Vec<bool> = saved
        .iter()
        .map(|window| restore_skip_reason(window).is_some())
        .collect();
    let mut pairs: Vec<(MatchScore, usize, usize)> = Vec::with_capacity(saved.len() * live.len());
    for (saved_idx, snap) in saved.iter().enumerate() {
        for (live_idx, candidate) in live.iter().enumerate() {
            if already_consumed.contains(&candidate.window_id) {
                continue;
            }
            let score = compute_match_score_with_target(snap, candidate, target_frames[saved_idx]);
            pairs.push((score, saved_idx, live_idx));
        }
    }
    pairs.sort_by(|left, right| {
        right
            .0
            .final_score
            .partial_cmp(&left.0.final_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            // When identity is equally plausible, protect the skipped window.
            .then_with(|| skipped[right.1].cmp(&skipped[left.1]))
    });

    let mut assignments: Vec<Option<(usize, MatchScore)>> = vec![None; saved.len()];
    let mut used_live: HashSet<usize> = HashSet::new();
    for (score, saved_idx, live_idx) in pairs {
        if assignments[saved_idx].is_some() || used_live.contains(&live_idx) {
            continue;
        }
        // Reserve protected counterparts conservatively as titles and positions change.
        let protected_geometry = skipped[saved_idx]
            && score.bundle_match
            && score.geometry_similarity >= PROTECTED_GEOMETRY_MIN;
        if !score.is_acceptable() && !protected_geometry {
            continue;
        }
        assignments[saved_idx] = Some((live_idx, score));
        used_live.insert(live_idx);
    }
    assignments
}

pub fn compute_match_score(saved: &WindowSnapshot, candidate: &LiveWindow) -> MatchScore {
    compute_match_score_with_target(saved, candidate, saved.frame)
}

fn compute_match_score_with_target(
    saved: &WindowSnapshot,
    candidate: &LiveWindow,
    target_frame: Frame,
) -> MatchScore {
    let bundle_match = candidate
        .bundle_id
        .as_deref()
        .zip(saved.bundle_id.as_deref())
        .map(|(left, right)| left == right)
        .unwrap_or(false);

    let title_comparison = title_similarity(saved.title.as_deref(), candidate.title.as_deref());
    let title_evidence = title_comparison.is_some();
    let title_similarity = title_comparison.unwrap_or(0.0);
    // A window can still be at its saved position or already at its remapped target.
    let geometry_similarity = geometry_similarity(saved.frame, candidate.frame)
        .max(geometry_similarity(target_frame, candidate.frame));

    // Weighted final score in [0,1] favoring title similarity once bundle
    // matches.  Bundle mismatch caps the score so cross-app reuse is unlikely.
    let bundle_factor = if bundle_match { 1.0 } else { 0.10 };
    let final_score = (0.55 * title_similarity + 0.45 * geometry_similarity) * bundle_factor;

    MatchScore {
        title_similarity,
        geometry_similarity,
        bundle_match,
        title_evidence,
        final_score,
    }
}

fn title_similarity(left: Option<&str>, right: Option<&str>) -> Option<f32> {
    match (left, right) {
        (Some(a), Some(b)) => {
            let na = normalize_title(a);
            let nb = normalize_title(b);
            if na.is_empty() || nb.is_empty() {
                return None;
            }
            if a == b {
                return Some(1.0);
            }
            if na == nb {
                return Some(0.95);
            }
            if na.contains(&nb) || nb.contains(&na) {
                return Some(0.7);
            }
            Some(jaccard_words(&na, &nb))
        }
        _ => None,
    }
}

fn normalize_title(value: &str) -> String {
    value
        .trim()
        .to_lowercase()
        .chars()
        .filter(|character| !character.is_ascii_punctuation())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn jaccard_words(left: &str, right: &str) -> f32 {
    let lhs: HashSet<&str> = left.split_whitespace().collect();
    let rhs: HashSet<&str> = right.split_whitespace().collect();
    if lhs.is_empty() && rhs.is_empty() {
        return 0.5;
    }
    let intersection = lhs.intersection(&rhs).count() as f32;
    let union = lhs.union(&rhs).count() as f32;
    if union == 0.0 {
        0.0
    } else {
        intersection / union
    }
}

fn geometry_similarity(left: Frame, right: Frame) -> f32 {
    let center_left = (left.x + left.width / 2.0, left.y + left.height / 2.0);
    let center_right = (right.x + right.width / 2.0, right.y + right.height / 2.0);
    let dx = (center_left.0 - center_right.0).abs();
    let dy = (center_left.1 - center_right.1).abs();
    let distance = (dx * dx + dy * dy).sqrt();
    // Position similarity reaches zero at 2,000 points.
    let position_score = (1.0 - (distance / 2000.0)).clamp(0.0, 1.0) as f32;

    let size_left = (left.width.max(1.0), left.height.max(1.0));
    let size_right = (right.width.max(1.0), right.height.max(1.0));
    let width_ratio = (size_left.0.min(size_right.0) / size_left.0.max(size_right.0)) as f32;
    let height_ratio = (size_left.1.min(size_right.1) / size_left.1.max(size_right.1)) as f32;
    let size_score = (width_ratio + height_ratio) / 2.0;

    0.5 * position_score + 0.5 * size_score
}

pub use crate::world::target_frame_for_window;

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::model::{HostInfo, SNAPSHOT_VERSION};

    fn display() -> DisplaySnapshot {
        DisplaySnapshot {
            id: "cgdisplay-1".to_string(),
            numeric_id: 1,
            name: None,
            frame: Frame {
                x: 0.0,
                y: 0.0,
                width: 2000.0,
                height: 1200.0,
            },
            scale_factor: 1.0,
            is_primary: true,
        }
    }

    fn saved(title: &str, frame: Frame, bundle: &str) -> WindowSnapshot {
        WindowSnapshot {
            window_id: 1,
            app_name: bundle.to_string(),
            process_name: bundle.to_string(),
            bundle_id: Some(bundle.to_string()),
            pid: 100,
            title: Some(title.to_string()),
            frame,
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

    fn live(title: &str, frame: Frame, bundle: &str, window_id: u32) -> LiveWindow {
        LiveWindow {
            bundle_id: Some(bundle.to_string()),
            app_name: bundle.to_string(),
            pid: 200,
            window_id,
            title: Some(title.to_string()),
            frame,
            minimized: false,
        }
    }

    fn snap(windows: Vec<WindowSnapshot>) -> WorkspaceSnapshot {
        WorkspaceSnapshot {
            version: SNAPSHOT_VERSION,
            name: "test".to_string(),
            created_at: Utc::now(),
            host: HostInfo {
                hostname: "host".to_string(),
                os: "macos".to_string(),
                arch: "aarch64".to_string(),
            },
            displays: vec![display()],
            windows,
        }
    }

    fn world(windows: Vec<LiveWindow>, running: &[&str]) -> WorldState {
        let mut running_pids = HashMap::new();
        for bundle in running {
            running_pids.insert((*bundle).to_string(), vec![200]);
        }
        WorldState {
            displays: vec![display()],
            windows,
            running_pids,
        }
    }

    fn frames_for(snapshot: &WorkspaceSnapshot) -> Vec<Frame> {
        snapshot.windows.iter().map(|window| window.frame).collect()
    }

    #[test]
    fn launches_missing_app_once_per_bundle() {
        let bundle = "com.microsoft.VSCode";
        let frame = Frame {
            x: 0.0,
            y: 0.0,
            width: 800.0,
            height: 600.0,
        };
        let snapshot = snap(vec![saved("a", frame, bundle), saved("b", frame, bundle)]);
        let world = world(Vec::new(), &[]);
        let frames = frames_for(&snapshot);

        let plan = plan_restore(&snapshot, &world, PlanOptions::default(), &frames);

        let launches = plan
            .operations
            .iter()
            .filter(|op| matches!(op.kind, OperationKind::LaunchApp { .. }))
            .count();
        assert_eq!(launches, 1);
        let creates = plan
            .operations
            .iter()
            .filter(|op| matches!(op.kind, OperationKind::CreateWindow { .. }))
            .count();
        assert_eq!(creates, 2);
    }

    #[test]
    fn already_open_window_is_reused_instead_of_created() {
        let bundle = "com.microsoft.VSCode";
        let frame = Frame {
            x: 0.0,
            y: 0.0,
            width: 800.0,
            height: 600.0,
        };
        let snapshot = snap(vec![saved("main.rs", frame, bundle)]);
        let world = world(vec![live("main.rs", frame, bundle, 42)], &[bundle]);
        let frames = frames_for(&snapshot);

        let plan = plan_restore(&snapshot, &world, PlanOptions::default(), &frames);

        assert_eq!(plan.operations.len(), 1);
        match &plan.operations[0].kind {
            OperationKind::Reposition { live_window_id, .. } => assert_eq!(*live_window_id, 42),
            other => panic!("expected reposition, got {other:?}"),
        }
        let score = plan.operations[0].match_score.unwrap();
        assert!(score.bundle_match);
        assert!(score.final_score >= 0.9);
    }

    #[test]
    fn duplicate_titles_get_distinct_live_windows() {
        let bundle = "com.microsoft.VSCode";
        let frame_left = Frame {
            x: 0.0,
            y: 0.0,
            width: 800.0,
            height: 600.0,
        };
        let frame_right = Frame {
            x: 900.0,
            y: 0.0,
            width: 800.0,
            height: 600.0,
        };
        let snapshot = snap(vec![
            saved("Docs", frame_left, bundle),
            saved("Docs", frame_right, bundle),
        ]);
        let world = world(
            vec![
                live("Docs", frame_right, bundle, 1),
                live("Docs", frame_left, bundle, 2),
            ],
            &[bundle],
        );
        let frames = frames_for(&snapshot);

        let plan = plan_restore(&snapshot, &world, PlanOptions::default(), &frames);

        let repositioned: Vec<u32> = plan
            .operations
            .iter()
            .filter_map(|op| match &op.kind {
                OperationKind::Reposition { live_window_id, .. } => Some(*live_window_id),
                _ => None,
            })
            .collect();
        assert_eq!(repositioned.len(), 2);
        assert_ne!(repositioned[0], repositioned[1]);
    }

    #[test]
    fn unsupported_app_is_skipped_with_reason() {
        let frame = Frame {
            x: 0.0,
            y: 0.0,
            width: 500.0,
            height: 400.0,
        };
        let snapshot = snap(vec![saved("x", frame, "dev.example.Unknown")]);
        let world = world(Vec::new(), &[]);
        let frames = frames_for(&snapshot);

        let plan = plan_restore(&snapshot, &world, PlanOptions::default(), &frames);

        assert_eq!(plan.operations.len(), 1);
        assert!(matches!(
            plan.operations[0].kind,
            OperationKind::Skip { .. }
        ));
    }

    #[test]
    fn fullscreen_windows_are_skipped_even_when_supported() {
        let frame = Frame {
            x: 0.0,
            y: 0.0,
            width: 500.0,
            height: 400.0,
        };
        let mut window = saved("x", frame, "com.microsoft.VSCode");
        window.fullscreen = true;
        let snapshot = snap(vec![window]);
        let world = world(Vec::new(), &[]);
        let frames = frames_for(&snapshot);

        let plan = plan_restore(&snapshot, &world, PlanOptions::default(), &frames);
        assert!(matches!(
            plan.operations[0].kind,
            OperationKind::Skip { .. }
        ));
    }

    #[test]
    fn safe_mode_never_emits_destructive_ops() {
        let bundle = "com.microsoft.VSCode";
        let frame = Frame {
            x: 0.0,
            y: 0.0,
            width: 500.0,
            height: 400.0,
        };
        let snapshot = snap(vec![saved("main", frame, bundle)]);
        let world = world(
            vec![
                live("main", frame, bundle, 1),
                live("extra", frame, bundle, 2),
            ],
            &[bundle],
        );
        let frames = frames_for(&snapshot);

        let plan = plan_restore(&snapshot, &world, PlanOptions::default(), &frames);

        assert_eq!(plan.destructive_ops, 0);
        assert_eq!(plan.left_alone_conflicts, 1);
        assert!(!plan.operations.iter().any(|op| op.kind.is_destructive()));
    }

    #[test]
    fn reconcile_mode_minimizes_conflicts_for_owned_bundles_only() {
        let bundle = "com.microsoft.VSCode";
        let frame = Frame {
            x: 0.0,
            y: 0.0,
            width: 500.0,
            height: 400.0,
        };
        let snapshot = snap(vec![saved("main", frame, bundle)]);
        let world = world(
            vec![
                live("main", frame, bundle, 1),
                live("extra", frame, bundle, 2),
                live("untouchable", frame, "com.unrelated.App", 3),
            ],
            &[bundle, "com.unrelated.App"],
        );
        let frames = frames_for(&snapshot);

        let plan = plan_restore(
            &snapshot,
            &world,
            PlanOptions {
                mode: RestoreMode::Reconcile,
                dev_mode: false,
            },
            &frames,
        );

        let minimizes: Vec<_> = plan
            .operations
            .iter()
            .filter_map(|op| match &op.kind {
                OperationKind::MinimizeConflict { live_window_id, .. } => Some(*live_window_id),
                _ => None,
            })
            .collect();
        assert_eq!(minimizes, vec![2]);
        assert_eq!(plan.destructive_ops, 1);
    }

    #[test]
    fn destructive_mode_closes_conflicts_for_owned_bundles_only() {
        let bundle = "com.microsoft.VSCode";
        let frame = Frame {
            x: 0.0,
            y: 0.0,
            width: 500.0,
            height: 400.0,
        };
        let snapshot = snap(vec![saved("main", frame, bundle)]);
        let world = world(
            vec![
                live("main", frame, bundle, 1),
                live("extra", frame, bundle, 2),
                live("untouchable", frame, "com.unrelated.App", 3),
            ],
            &[bundle, "com.unrelated.App"],
        );
        let frames = frames_for(&snapshot);

        let plan = plan_restore(
            &snapshot,
            &world,
            PlanOptions {
                mode: RestoreMode::Destructive,
                dev_mode: false,
            },
            &frames,
        );

        let closes: Vec<_> = plan
            .operations
            .iter()
            .filter_map(|op| match &op.kind {
                OperationKind::CloseConflict { live_window_id, .. } => Some(*live_window_id),
                _ => None,
            })
            .collect();
        assert_eq!(closes, vec![2]);
        assert_eq!(plan.destructive_ops, 1);
    }

    #[test]
    fn dev_mode_skips_protected_app_when_not_running() {
        let bundle = "com.microsoft.VSCode";
        let frame = Frame {
            x: 0.0,
            y: 0.0,
            width: 500.0,
            height: 400.0,
        };
        let snapshot = snap(vec![saved("x", frame, bundle)]);
        let world = world(Vec::new(), &[]);
        let frames = frames_for(&snapshot);

        let plan = plan_restore(
            &snapshot,
            &world,
            PlanOptions {
                mode: RestoreMode::Safe,
                dev_mode: true,
            },
            &frames,
        );

        assert!(plan
            .operations
            .iter()
            .all(|op| matches!(op.kind, OperationKind::Skip { .. })));
        assert!(plan.operations[0].rationale.contains("dev-mode"));
    }

    #[test]
    fn chrome_window_without_live_match_restores_via_tabs() {
        let bundle = "com.google.Chrome";
        let frame = Frame {
            x: 0.0,
            y: 0.0,
            width: 800.0,
            height: 600.0,
        };
        let mut window = saved("Search", frame, bundle);
        window.browser_tabs.push(crate::model::BrowserTab {
            title: Some("Rust".to_string()),
            url: "https://www.rust-lang.org/".to_string(),
            active: true,
        });
        let snapshot = snap(vec![window]);
        let world = world(Vec::new(), &[]);
        let frames = frames_for(&snapshot);

        let plan = plan_restore(&snapshot, &world, PlanOptions::default(), &frames);

        assert!(plan
            .operations
            .iter()
            .any(|op| matches!(op.kind, OperationKind::RestoreChromeTabs { .. })));
    }

    #[test]
    fn match_score_explains_low_confidence_for_random_titles() {
        let bundle = "com.microsoft.VSCode";
        let saved_window = saved(
            "main.rs",
            Frame {
                x: 0.0,
                y: 0.0,
                width: 800.0,
                height: 600.0,
            },
            bundle,
        );
        let live_window = live(
            "completely unrelated title",
            Frame {
                x: 5000.0,
                y: 5000.0,
                width: 100.0,
                height: 100.0,
            },
            bundle,
            7,
        );
        let score = compute_match_score(&saved_window, &live_window);
        assert!(score.bundle_match);
        assert!(score.final_score < 0.6, "score was {}", score.final_score);
    }

    #[test]
    fn titleless_windows_do_not_match_on_weak_geometry() {
        // Missing titles require a stricter geometry match.
        let bundle = "com.google.Chrome";
        let mut saved_window = saved(
            "ignored",
            Frame {
                x: 200.0,
                y: 120.0,
                width: 900.0,
                height: 600.0,
            },
            bundle,
        );
        saved_window.title = None;
        let mut live_window = live(
            "ignored",
            Frame {
                x: 0.0,
                y: 33.0,
                width: 1470.0,
                height: 833.0,
            },
            bundle,
            9,
        );
        live_window.title = None;
        let snapshot = snap(vec![saved_window]);
        let world = world(vec![live_window], &[bundle]);
        let frames = frames_for(&snapshot);

        let plan = plan_restore(&snapshot, &world, PlanOptions::default(), &frames);

        assert!(
            !plan
                .operations
                .iter()
                .any(|op| matches!(op.kind, OperationKind::Reposition { .. })),
            "titleless weak-geometry match must be rejected: {plan:?}"
        );
        assert!(plan
            .operations
            .iter()
            .any(|op| matches!(op.kind, OperationKind::CreateWindow { .. })));
    }

    #[test]
    fn titleless_windows_match_on_near_exact_geometry() {
        let bundle = "com.google.Chrome";
        let mut saved_window = saved(
            "ignored",
            Frame {
                x: 200.0,
                y: 120.0,
                width: 900.0,
                height: 600.0,
            },
            bundle,
        );
        saved_window.title = None;
        let mut live_window = live(
            "ignored",
            Frame {
                x: 204.0,
                y: 124.0,
                width: 900.0,
                height: 600.0,
            },
            bundle,
            9,
        );
        live_window.title = None;
        let snapshot = snap(vec![saved_window]);
        let world = world(vec![live_window], &[bundle]);
        let frames = frames_for(&snapshot);

        let plan = plan_restore(&snapshot, &world, PlanOptions::default(), &frames);

        assert!(
            plan.operations
                .iter()
                .any(|op| matches!(op.kind, OperationKind::Reposition { .. })),
            "near-exact geometry should still match without titles: {plan:?}"
        );
    }

    #[test]
    fn disabled_windows_skip_without_launching_apps() {
        let bundle = "com.microsoft.VSCode";
        let frame = Frame {
            x: 0.0,
            y: 0.0,
            width: 500.0,
            height: 400.0,
        };
        let mut window = saved("x", frame, bundle);
        window.enabled = false;
        let snapshot = snap(vec![window]);
        let world = world(Vec::new(), &[]);
        let frames = frames_for(&snapshot);

        let plan = plan_restore(&snapshot, &world, PlanOptions::default(), &frames);

        assert!(plan
            .operations
            .iter()
            .all(|op| matches!(op.kind, OperationKind::Skip { .. })));
        assert!(!plan
            .operations
            .iter()
            .any(|op| matches!(op.kind, OperationKind::LaunchApp { .. })));
    }
}
