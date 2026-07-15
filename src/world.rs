//! World observation, display remapping, and diagnostics.
//!
//! Everything here is glue between the pure planner/verifier and the live
//! macOS world: observe the current windows, project saved windows onto the
//! current displays, and report environment health.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::{
    error::Result,
    filter::should_capture_window,
    macos::{accessibility, app, display, window},
    model::{DisplaySnapshot, Frame, WindowSnapshot, WorkspaceSnapshot},
    plan::{plan_restore, restore_skip_reason, LiveWindow, PlanOptions, RestorePlan, WorldState},
    verify as verify_mod,
};

const RESTORE_MARGIN: f64 = 12.0;

/// Snapshot the current macOS world into a [`WorldState`] suitable for the
/// pure planner. Returns an empty world on non-macOS targets.
pub fn observe_world() -> Result<WorldState> {
    let displays = display::current_displays()?;
    let raw_windows = window::enumerate_windows()?;
    let mut windows = Vec::new();
    let mut running_pids: HashMap<String, Vec<i32>> = HashMap::new();

    for raw in raw_windows.into_iter().filter(should_capture_window) {
        let info = app::application_for_pid(raw.owner_pid);
        let bundle_id = info.as_ref().and_then(|info| info.bundle_id.clone());
        let app_name = info
            .as_ref()
            .and_then(|info| info.localized_name.clone())
            .unwrap_or_else(|| raw.owner_name.clone());

        if let Some(bundle) = &bundle_id {
            let pids = running_pids.entry(bundle.clone()).or_default();
            if !pids.contains(&raw.owner_pid) {
                pids.push(raw.owner_pid);
            }
        }

        windows.push(LiveWindow {
            bundle_id,
            app_name,
            pid: raw.owner_pid,
            window_id: raw.window_id,
            title: raw.window_title,
            frame: raw.frame,
            minimized: false,
        });
    }

    // CGWindowList only reports on-screen windows: minimized windows are
    // invisible to it, which used to make the planner treat them as missing
    // and open duplicates. Enrich the world with AX-visible minimized windows
    // (and register pids) for every supported app that is running.
    let mut synthetic_id = u32::MAX;
    for known in crate::app_support::full_restore_apps() {
        for pid in app::running_pids_for_bundle(known.bundle_id) {
            let pids = running_pids.entry(known.bundle_id.to_string()).or_default();
            if !pids.contains(&pid) {
                pids.push(pid);
            }
            let Ok(states) = accessibility::ax_window_states(pid) else {
                continue;
            };
            for state in states.into_iter().filter(|state| state.minimized) {
                windows.push(LiveWindow {
                    bundle_id: Some(known.bundle_id.to_string()),
                    app_name: known.name.to_string(),
                    pid,
                    window_id: synthetic_id,
                    title: state.title,
                    frame: state.frame.unwrap_or(Frame {
                        x: 0.0,
                        y: 0.0,
                        width: 0.0,
                        height: 0.0,
                    }),
                    minimized: true,
                });
                synthetic_id -= 1;
            }
        }
    }

    Ok(WorldState {
        displays,
        windows,
        running_pids,
    })
}

/// Build the restore plan for a snapshot against the live world.
pub fn build_plan(
    snapshot: &WorkspaceSnapshot,
    mode: crate::plan::RestoreMode,
    dev_mode: bool,
) -> Result<RestorePlan> {
    let current_displays = display::current_displays()?;
    let mut target_frames = Vec::with_capacity(snapshot.windows.len());
    for window in &snapshot.windows {
        target_frames.push(target_frame_for_window(
            window,
            &snapshot.displays,
            &current_displays,
        ));
    }
    let world = observe_world()?;
    Ok(plan_restore(
        snapshot,
        &world,
        PlanOptions { mode, dev_mode },
        &target_frames,
    ))
}

/// Compare a snapshot to the current world and produce a verification report.
pub fn verify_workspace(snapshot: &WorkspaceSnapshot) -> Result<verify_mod::VerifyReport> {
    let current_displays = display::current_displays()?;
    let mut target_frames = Vec::with_capacity(snapshot.windows.len());
    for window in &snapshot.windows {
        target_frames.push(target_frame_for_window(
            window,
            &snapshot.displays,
            &current_displays,
        ));
    }
    let world = observe_world()?;
    Ok(verify_mod::verify(snapshot, &world, &target_frames))
}

/// Best-effort z-order replay: raise restorable windows back-to-front so the
/// frontmost saved window ends up frontmost.
pub fn replay_z_order(snapshot: &WorkspaceSnapshot) {
    let mut windows: Vec<&WindowSnapshot> = snapshot
        .windows
        .iter()
        .filter(|window| restore_skip_reason(window).is_none())
        .collect();
    windows.sort_by_key(|window| std::cmp::Reverse(window.z_order.unwrap_or(u32::MAX)));
    tracing::debug!(count = windows.len(), "replaying saved z-order");
    for window in windows {
        let Some(bundle_id) = &window.bundle_id else {
            continue;
        };
        let _ = app::activate_bundle(bundle_id);
        for pid in app::running_pids_for_bundle(bundle_id) {
            match accessibility::raise_window(pid, window) {
                Ok(true) => break,
                Ok(false) => continue,
                Err(error) => {
                    tracing::debug!(app = %window.app_name, pid, %error, "z-order raise failed");
                    continue;
                }
            }
        }
    }
}

// -----------------------------------------------------------------------
// Display remapping
// -----------------------------------------------------------------------

pub fn target_frame_for_window(
    window: &WindowSnapshot,
    saved_displays: &[DisplaySnapshot],
    current_displays: &[DisplaySnapshot],
) -> Frame {
    if current_displays.is_empty() {
        return window.frame;
    }

    let saved_display = window
        .display_id
        .as_deref()
        .and_then(|id| saved_displays.iter().find(|display| display.id == id))
        .or_else(|| {
            window
                .display_frame
                .and_then(|frame| find_display_by_frame(saved_displays, frame))
        });

    if let Some(saved_display) = saved_display {
        if let Some(current) = exact_current_display(saved_display, current_displays) {
            if frame_fits_display(window.frame, current.frame) {
                return window.frame;
            }
            return clamp_to_display(window.frame, current.frame);
        }

        let mapped = best_current_display(saved_display, current_displays);
        let relative = window
            .display_relative_frame
            .unwrap_or_else(|| window.frame.relative_to(saved_display.frame));
        return clamp_to_display(relative.to_frame(mapped.frame), mapped.frame);
    }

    let current = current_displays
        .iter()
        .find(|display| display.is_primary)
        .unwrap_or(&current_displays[0]);
    clamp_to_display(window.frame, current.frame)
}

fn exact_current_display<'a>(
    saved: &DisplaySnapshot,
    current_displays: &'a [DisplaySnapshot],
) -> Option<&'a DisplaySnapshot> {
    current_displays
        .iter()
        .find(|current| current.id == saved.id && current.frame == saved.frame)
        .or_else(|| {
            current_displays.iter().find(|current| {
                current.numeric_id == saved.numeric_id && same_frame(current.frame, saved.frame)
            })
        })
}

fn best_current_display<'a>(
    saved: &DisplaySnapshot,
    current_displays: &'a [DisplaySnapshot],
) -> &'a DisplaySnapshot {
    current_displays
        .iter()
        .min_by(|left, right| {
            let left_score = display_distance(saved.frame, left.frame, left.is_primary);
            let right_score = display_distance(saved.frame, right.frame, right.is_primary);
            left_score
                .partial_cmp(&right_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .expect("current displays checked by caller")
}

fn display_distance(saved: Frame, current: Frame, primary: bool) -> f64 {
    let area_ratio = (saved.area() - current.area()).abs() / saved.area().max(1.0);
    let aspect_saved = saved.width / saved.height.max(1.0);
    let aspect_current = current.width / current.height.max(1.0);
    let primary_penalty = if primary { 0.0 } else { 0.15 };
    area_ratio + (aspect_saved - aspect_current).abs() + primary_penalty
}

fn find_display_by_frame(displays: &[DisplaySnapshot], frame: Frame) -> Option<&DisplaySnapshot> {
    displays
        .iter()
        .find(|display| same_frame(display.frame, frame))
}

fn frame_fits_display(frame: Frame, display: Frame) -> bool {
    frame.x >= display.x
        && frame.y >= display.y
        && frame.right() <= display.right()
        && frame.bottom() <= display.bottom()
}

fn same_frame(left: Frame, right: Frame) -> bool {
    (left.x - right.x).abs() < 0.5
        && (left.y - right.y).abs() < 0.5
        && (left.width - right.width).abs() < 0.5
        && (left.height - right.height).abs() < 0.5
}

pub fn clamp_to_display(frame: Frame, display: Frame) -> Frame {
    let max_width = (display.width - RESTORE_MARGIN * 2.0).max(80.0);
    let max_height = (display.height - RESTORE_MARGIN * 2.0).max(60.0);
    let width = frame.width.min(max_width).max(80.0);
    let height = frame.height.min(max_height).max(60.0);

    let min_x = display.x + RESTORE_MARGIN;
    let min_y = display.y + RESTORE_MARGIN;
    let max_x = display.x + display.width - width - RESTORE_MARGIN;
    let max_y = display.y + display.height - height - RESTORE_MARGIN;

    Frame {
        x: frame.x.clamp(min_x, max_x.max(min_x)),
        y: frame.y.clamp(min_y, max_y.max(min_y)),
        width,
        height,
    }
}

// -----------------------------------------------------------------------
// Doctor
// -----------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorReport {
    pub data_dir: String,
    pub data_dir_writable: bool,
    pub accessibility_trusted: bool,
    pub display_count: usize,
    pub supported_bundles: Vec<String>,
    pub warnings: Vec<String>,
}

/// Run environment diagnostics.
pub fn doctor() -> Result<DoctorReport> {
    use crate::app_support::KNOWN_APPS;
    use crate::storage::SnapshotStore;

    let mut warnings = Vec::new();

    let store = SnapshotStore::open_default()?;
    let data_dir = store.root().display().to_string();
    let data_dir_writable = is_dir_writable(store.root());
    if !data_dir_writable {
        warnings.push(format!("data dir {data_dir} is not writable"));
    }

    let accessibility_trusted = accessibility::is_trusted();
    if !accessibility_trusted {
        warnings.push(
            "Accessibility permission is not granted; restore will not be able to move windows."
                .to_string(),
        );
    }

    let displays = display::current_displays().unwrap_or_default();
    if displays.is_empty() {
        warnings.push("no displays detected".to_string());
    }

    // Window titles come from CGWindowList and require the Screen Recording
    // permission on modern macOS. Without them, save still works but window
    // matching degrades to geometry-only and browser tab attribution falls
    // back to window order.
    let visible: Vec<_> = window::enumerate_windows()
        .unwrap_or_default()
        .into_iter()
        .filter(should_capture_window)
        .collect();
    if !visible.is_empty() && visible.iter().all(|window| window.window_title.is_none()) {
        warnings.push(
            "no window titles are visible; grant Screen Recording permission \
             (System Settings → Privacy & Security → Screen Recording) for reliable window matching"
                .to_string(),
        );
    }

    let supported_bundles = KNOWN_APPS
        .iter()
        .map(|app| app.bundle_id.to_string())
        .collect();

    Ok(DoctorReport {
        data_dir,
        data_dir_writable,
        accessibility_trusted,
        display_count: displays.len(),
        supported_bundles,
        warnings,
    })
}

fn is_dir_writable(path: &std::path::Path) -> bool {
    let probe = path.join(".workspace-doctor-probe");
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{RelativeFrame, WindowSnapshot};

    fn display(id: &str, frame: Frame, primary: bool) -> DisplaySnapshot {
        DisplaySnapshot {
            id: id.to_string(),
            numeric_id: if primary { 1 } else { 2 },
            name: None,
            frame,
            scale_factor: 1.0,
            is_primary: primary,
        }
    }

    fn window(frame: Frame, display: &DisplaySnapshot) -> WindowSnapshot {
        WindowSnapshot {
            window_id: 1,
            app_name: "Code".to_string(),
            process_name: "Code".to_string(),
            bundle_id: Some("com.microsoft.VSCode".to_string()),
            pid: 42,
            title: Some("main.rs".to_string()),
            frame,
            display_id: Some(display.id.clone()),
            display_frame: Some(display.frame),
            display_relative_frame: Some(RelativeFrame {
                x: 0.25,
                y: 0.25,
                width: 0.5,
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
    fn preserves_exact_frame_when_display_is_unchanged() {
        let saved = display(
            "a",
            Frame {
                x: 0.0,
                y: 0.0,
                width: 2000.0,
                height: 1000.0,
            },
            true,
        );
        let saved_window = window(
            Frame {
                x: 100.0,
                y: 80.0,
                width: 900.0,
                height: 700.0,
            },
            &saved,
        );
        assert_eq!(
            target_frame_for_window(
                &saved_window,
                std::slice::from_ref(&saved),
                std::slice::from_ref(&saved),
            ),
            Frame {
                x: 100.0,
                y: 80.0,
                width: 900.0,
                height: 700.0
            }
        );
    }

    #[test]
    fn proportionally_remaps_missing_display() {
        let saved = display(
            "external",
            Frame {
                x: 2000.0,
                y: 0.0,
                width: 2000.0,
                height: 1000.0,
            },
            false,
        );
        let current = display(
            "built-in",
            Frame {
                x: 0.0,
                y: 0.0,
                width: 1000.0,
                height: 500.0,
            },
            true,
        );
        let saved_window = window(
            Frame {
                x: 2500.0,
                y: 250.0,
                width: 1000.0,
                height: 500.0,
            },
            &saved,
        );
        let target = target_frame_for_window(&saved_window, &[saved], &[current]);
        assert_eq!(target.width, 500.0);
        assert_eq!(target.height, 250.0);
        assert!(target.x >= 12.0);
        assert!(target.y >= 12.0);
    }

    #[test]
    fn clamps_offscreen_windows() {
        let display = Frame {
            x: 0.0,
            y: 0.0,
            width: 800.0,
            height: 600.0,
        };
        let target = clamp_to_display(
            Frame {
                x: 700.0,
                y: 550.0,
                width: 400.0,
                height: 300.0,
            },
            display,
        );
        assert!(target.x + target.width <= 800.0 - 12.0 + 0.1);
        assert!(target.y + target.height <= 600.0 - 12.0 + 0.1);
    }

    #[test]
    fn z_order_replay_sorts_back_to_front() {
        let display = display(
            "a",
            Frame {
                x: 0.0,
                y: 0.0,
                width: 1000.0,
                height: 800.0,
            },
            true,
        );
        let mut front = window(
            Frame {
                x: 0.0,
                y: 0.0,
                width: 500.0,
                height: 400.0,
            },
            &display,
        );
        front.app_name = "front".to_string();
        front.z_order = Some(0);

        let mut back = front.clone();
        back.app_name = "back".to_string();
        back.z_order = Some(10);

        let mut windows = [&front, &back];
        windows.sort_by_key(|window| std::cmp::Reverse(window.z_order.unwrap_or(u32::MAX)));

        assert_eq!(windows[0].app_name, "back");
        assert_eq!(windows[1].app_name, "front");
    }
}
