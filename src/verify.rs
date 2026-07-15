//! Post-restore verification.
//!
//! Compares the desired state (a snapshot + per-window target frames) against
//! a [`WorldState`] observation, and emits a per-window geometry delta plus
//! summary metrics.  Pure logic so tests don't need real windows.

use serde::{Deserialize, Serialize};

use crate::{
    model::{Frame, WindowSnapshot, WorkspaceSnapshot},
    plan::{compute_match_score, restore_skip_reason, MatchScore, WorldState},
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerifyReport {
    pub snapshot: String,
    /// All saved windows, including ones restore would skip.
    pub total: usize,
    pub matched: usize,
    /// Restorable windows with no live counterpart.
    pub unmatched: usize,
    /// Windows restore never touches (disabled / unsupported / fullscreen).
    /// Excluded from `accuracy` so a converged world can reach 100%.
    #[serde(default)]
    pub skipped: usize,
    pub mean_geometry_delta: f64,
    pub max_geometry_delta: f64,
    pub accuracy: f32,
    pub windows: Vec<VerifyEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerifyEntry {
    pub saved_window_index: usize,
    pub app_name: String,
    pub bundle_id: Option<String>,
    pub matched: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped_reason: Option<String>,
    pub match_score: Option<MatchScore>,
    pub expected_frame: Frame,
    pub observed_frame: Option<Frame>,
    pub geometry_delta: Option<f64>,
}

pub fn verify(
    snapshot: &WorkspaceSnapshot,
    world: &WorldState,
    target_frames: &[Frame],
) -> VerifyReport {
    assert_eq!(target_frames.len(), snapshot.windows.len());

    let mut entries = Vec::with_capacity(snapshot.windows.len());
    let mut total_delta = 0.0;
    let mut max_delta: f64 = 0.0;
    let mut matched = 0;
    let mut skipped = 0;

    // Track live windows we've already attributed to a saved window so two
    // saved windows can't both claim the same live window.
    let mut consumed: Vec<bool> = vec![false; world.windows.len()];

    for (index, window) in snapshot.windows.iter().enumerate() {
        let expected = target_frames[index];

        // Windows restore never touches must not count against accuracy —
        // otherwise any snapshot containing an unsupported app can never
        // verify at 100% and `apply --converge` never stops early.
        if let Some(reason) = restore_skip_reason(window) {
            skipped += 1;
            entries.push(VerifyEntry {
                saved_window_index: index,
                app_name: window.app_name.clone(),
                bundle_id: window.bundle_id.clone(),
                matched: false,
                skipped_reason: Some(reason),
                match_score: None,
                expected_frame: expected,
                observed_frame: None,
                geometry_delta: None,
            });
            continue;
        }

        let candidate = pick_best_live(window, world, &consumed);

        match candidate {
            Some((live_index, score)) => {
                consumed[live_index] = true;
                let observed = world.windows[live_index].frame;
                let delta = frame_delta(expected, observed);
                total_delta += delta;
                max_delta = max_delta.max(delta);
                matched += 1;
                entries.push(VerifyEntry {
                    saved_window_index: index,
                    app_name: window.app_name.clone(),
                    bundle_id: window.bundle_id.clone(),
                    matched: true,
                    skipped_reason: None,
                    match_score: Some(score),
                    expected_frame: expected,
                    observed_frame: Some(observed),
                    geometry_delta: Some(delta),
                });
            }
            None => entries.push(VerifyEntry {
                saved_window_index: index,
                app_name: window.app_name.clone(),
                bundle_id: window.bundle_id.clone(),
                matched: false,
                skipped_reason: None,
                match_score: None,
                expected_frame: expected,
                observed_frame: None,
                geometry_delta: None,
            }),
        }
    }

    let total = snapshot.windows.len();
    let restorable = total - skipped;
    let unmatched = restorable - matched;
    let mean_geometry_delta = if matched == 0 {
        0.0
    } else {
        total_delta / matched as f64
    };
    let accuracy = if restorable == 0 {
        1.0
    } else {
        let geometry_quality = if matched == 0 {
            0.0
        } else {
            (1.0 - (mean_geometry_delta / 200.0).min(1.0)) as f32
        };
        let match_ratio = matched as f32 / restorable as f32;
        (match_ratio * geometry_quality).clamp(0.0, 1.0)
    };

    VerifyReport {
        snapshot: snapshot.name.clone(),
        total,
        matched,
        unmatched,
        skipped,
        mean_geometry_delta,
        max_geometry_delta: max_delta,
        accuracy,
        windows: entries,
    }
}

fn pick_best_live(
    saved: &WindowSnapshot,
    world: &WorldState,
    consumed: &[bool],
) -> Option<(usize, MatchScore)> {
    let mut best: Option<(usize, MatchScore)> = None;
    for (index, live) in world.windows.iter().enumerate() {
        if consumed[index] {
            continue;
        }
        if live.bundle_id != saved.bundle_id {
            continue;
        }
        let score = compute_match_score(saved, live);
        if score.final_score < MatchScore::MIN_ACCEPT {
            continue;
        }
        match &best {
            Some((_, current)) if current.final_score >= score.final_score => continue,
            _ => best = Some((index, score)),
        }
    }
    best
}

fn frame_delta(a: Frame, b: Frame) -> f64 {
    let dx = (a.x - b.x).abs();
    let dy = (a.y - b.y).abs();
    let dw = (a.width - b.width).abs();
    let dh = (a.height - b.height).abs();
    ((dx * dx + dy * dy + dw * dw + dh * dh) / 4.0).sqrt()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use chrono::Utc;

    use super::*;
    use crate::model::{HostInfo, SNAPSHOT_VERSION};
    use crate::plan::LiveWindow;

    fn snapshot(window: WindowSnapshot) -> WorkspaceSnapshot {
        WorkspaceSnapshot {
            version: SNAPSHOT_VERSION,
            name: "verify".to_string(),
            created_at: Utc::now(),
            host: HostInfo {
                hostname: "host".to_string(),
                os: "macos".to_string(),
                arch: "aarch64".to_string(),
            },
            displays: Vec::new(),
            windows: vec![window],
        }
    }

    fn frame(x: f64, y: f64) -> Frame {
        Frame {
            x,
            y,
            width: 800.0,
            height: 600.0,
        }
    }

    fn saved_window(title: &str, bundle: &str, frame: Frame) -> WindowSnapshot {
        WindowSnapshot {
            window_id: 1,
            app_name: bundle.to_string(),
            process_name: bundle.to_string(),
            bundle_id: Some(bundle.to_string()),
            pid: 1,
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

    fn live_window(title: &str, bundle: &str, frame: Frame, id: u32) -> LiveWindow {
        LiveWindow {
            bundle_id: Some(bundle.to_string()),
            app_name: bundle.to_string(),
            pid: 200,
            window_id: id,
            title: Some(title.to_string()),
            frame,
            minimized: false,
        }
    }

    fn world(windows: Vec<LiveWindow>) -> WorldState {
        WorldState {
            displays: Vec::new(),
            windows,
            running_pids: HashMap::new(),
        }
    }

    #[test]
    fn fully_matched_workspace_reports_100_percent_accuracy() {
        let bundle = "com.microsoft.VSCode";
        let frame = frame(0.0, 0.0);
        let snap = snapshot(saved_window("main", bundle, frame));
        let world = world(vec![live_window("main", bundle, frame, 1)]);
        let report = verify(&snap, &world, &[frame]);

        assert_eq!(report.matched, 1);
        assert_eq!(report.unmatched, 0);
        assert!(report.accuracy > 0.99);
        assert!(report.mean_geometry_delta < 0.5);
    }

    #[test]
    fn unmatched_window_lowers_accuracy() {
        let bundle = "com.microsoft.VSCode";
        let frame_a = frame(0.0, 0.0);
        let snap = snapshot(saved_window("main", bundle, frame_a));
        let report = verify(&snap, &world(Vec::new()), &[frame_a]);

        assert_eq!(report.matched, 0);
        assert_eq!(report.unmatched, 1);
        assert!(report.accuracy < 0.01);
    }

    #[test]
    fn unsupported_and_disabled_windows_do_not_count_against_accuracy() {
        let frame_a = frame(0.0, 0.0);
        let mut snap = snapshot(saved_window("main", "com.microsoft.VSCode", frame_a));
        // An unsupported app and a disabled window join the supported one.
        snap.windows
            .push(saved_window("dash", "dev.example.Unknown", frame_a));
        let mut disabled = saved_window("off", "com.apple.Terminal", frame_a);
        disabled.enabled = false;
        snap.windows.push(disabled);

        let report = verify(
            &snap,
            &world(vec![live_window(
                "main",
                "com.microsoft.VSCode",
                frame_a,
                1,
            )]),
            &[frame_a, frame_a, frame_a],
        );

        assert_eq!(report.total, 3);
        assert_eq!(report.skipped, 2);
        assert_eq!(report.matched, 1);
        assert_eq!(report.unmatched, 0);
        assert!(
            report.accuracy > 0.99,
            "skipped windows must not drag accuracy below 100%, got {}",
            report.accuracy
        );
        assert!(report.windows[1].skipped_reason.is_some());
        assert!(report.windows[2].skipped_reason.is_some());
    }

    #[test]
    fn geometry_drift_is_observable() {
        let bundle = "com.microsoft.VSCode";
        let expected = frame(0.0, 0.0);
        let drifted = frame(50.0, 30.0);
        let snap = snapshot(saved_window("main", bundle, expected));
        let report = verify(
            &snap,
            &world(vec![live_window("main", bundle, drifted, 1)]),
            &[expected],
        );

        assert_eq!(report.matched, 1);
        assert!(report.mean_geometry_delta > 10.0);
        assert!(report.accuracy < 1.0);
    }
}
