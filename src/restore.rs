use std::{collections::BTreeMap, thread, time::Duration};

use crate::{
    app_support::{support_for_window, SupportLevel},
    error::Result,
    macos::{accessibility, app, chrome, display},
    model::{
        DisplaySnapshot, Frame, RestoreAction, RestoreReport, RestoreStatus, WindowSnapshot,
        WorkspaceSnapshot,
    },
    plan::RestoreMode,
};

const RESTORE_MARGIN: f64 = 12.0;
const APP_LAUNCH_WAIT_ATTEMPTS: usize = 20;
const APP_LAUNCH_WAIT_INTERVAL: Duration = Duration::from_millis(100);
const WINDOW_READY_WAIT_ATTEMPTS: usize = 40;
const WINDOW_READY_WAIT_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Copy, Clone, Default, PartialEq, Eq)]
pub struct RestoreOptions {
    pub dry_run: bool,
    pub dev_mode: bool,
    pub mode: RestoreMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CandidatePids {
    pids: Vec<i32>,
    launched: bool,
}

#[derive(Debug, Copy, Clone)]
struct RestoreJob<'a> {
    index: usize,
    window: &'a WindowSnapshot,
    target_frame: Frame,
}

pub fn restore_workspace(
    snapshot: &WorkspaceSnapshot,
    options: RestoreOptions,
) -> Result<RestoreReport> {
    tracing::debug!(snapshot = %snapshot.name, dry_run = options.dry_run, dev_mode = options.dev_mode, "starting restore");
    tracing::debug!("enumerating current displays");
    let current_displays = display::current_displays()?;
    tracing::debug!(
        count = current_displays.len(),
        "current displays enumerated"
    );
    if !options.dry_run {
        tracing::debug!("checking Accessibility permission");
        accessibility::ensure_trusted()?;
        tracing::debug!("Accessibility permission is available");
    }

    let mut report = RestoreReport {
        snapshot: snapshot.name.clone(),
        dry_run: options.dry_run,
        restored: 0,
        skipped: 0,
        failed: 0,
        actions: Vec::new(),
    };
    let mut restored_windows = Vec::new();
    let mut target_frames = Vec::with_capacity(snapshot.windows.len());
    let mut outcomes: Vec<Option<(RestoreStatus, Option<String>)>> =
        vec![None; snapshot.windows.len()];
    let mut jobs = Vec::new();

    for (index, window) in snapshot.windows.iter().enumerate() {
        let target_frame = target_frame_for_window(window, &snapshot.displays, &current_displays);
        target_frames.push(target_frame);

        if !window.enabled {
            outcomes[index] = Some((
                RestoreStatus::Skipped,
                Some("disabled in workspace configuration".to_string()),
            ));
            continue;
        }

        let support = support_for_window(window);

        if support.level != SupportLevel::FullRestore {
            outcomes[index] = Some((RestoreStatus::Skipped, Some(support.reason.to_string())));
            continue;
        }

        if window.fullscreen {
            outcomes[index] = Some((
                RestoreStatus::Skipped,
                Some("fullscreen windows are recorded but not resized in the MVP".to_string()),
            ));
            continue;
        }

        if options.dry_run {
            outcomes[index] = Some((RestoreStatus::Planned, None));
            continue;
        }

        jobs.push(RestoreJob {
            index,
            window,
            target_frame,
        });
    }

    if !options.dry_run {
        for group in grouped_restore_jobs(&jobs).values() {
            let results = restore_window_group(group, options);
            for (job, result) in group.iter().zip(results) {
                outcomes[job.index] = Some(match result {
                    Ok(()) => (RestoreStatus::Restored, None),
                    Err(error) => (RestoreStatus::Failed, Some(error.to_string())),
                });
            }
        }
    }

    for (index, window) in snapshot.windows.iter().enumerate() {
        let (status, message) = outcomes[index].clone().unwrap_or_else(|| {
            (
                RestoreStatus::Failed,
                Some("restore did not produce a result".to_string()),
            )
        });
        match status {
            RestoreStatus::Restored => {
                report.restored += 1;
                restored_windows.push(window);
            }
            RestoreStatus::Skipped => report.skipped += 1,
            RestoreStatus::Failed => report.failed += 1,
            RestoreStatus::Planned => {}
        }
        report
            .actions
            .push(action(window, target_frames[index], status, message));
    }

    if !options.dry_run {
        replay_z_order(&restored_windows);
    }

    Ok(report)
}

fn grouped_restore_jobs<'a>(jobs: &[RestoreJob<'a>]) -> BTreeMap<String, Vec<RestoreJob<'a>>> {
    let mut groups: BTreeMap<String, Vec<RestoreJob<'a>>> = BTreeMap::new();
    for job in jobs {
        let key = job
            .window
            .bundle_id
            .clone()
            .unwrap_or_else(|| format!("pid:{}", job.window.pid));
        groups.entry(key).or_default().push(*job);
    }
    groups
}

fn restore_window_group(jobs: &[RestoreJob<'_>], options: RestoreOptions) -> Vec<Result<()>> {
    if jobs.is_empty() {
        return Vec::new();
    }

    let first = jobs[0].window;

    // Chrome restores ALWAYS go through chrome::restore_windows, even when
    // only a single Chrome window is being restored. The legacy "single
    // window goes through generic AX path" was producing blank tabs, no
    // active-tab restoration, and unreliable resize. AppleScript's
    // `set bounds of window N` is the deterministic path for Chrome.
    if first.bundle_id.as_deref() == Some("com.google.Chrome") {
        return restore_chrome_group(jobs, options);
    }

    if jobs.len() == 1 {
        let job = jobs[0];
        return vec![restore_window(job.window, job.target_frame, options)];
    }

    tracing::debug!(
        app = %first.app_name,
        bundle_id = ?first.bundle_id,
        count = jobs.len(),
        "restoring window group"
    );

    let candidate = current_or_launched_pids(first, options);
    let targets: Vec<_> = jobs
        .iter()
        .map(|job| (job.window, job.target_frame))
        .collect();

    let pids = prepare_window_group(jobs, candidate, options);

    match set_group_frames_with_retry(first, jobs.len(), &targets, pids) {
        Ok(Some(results)) => {
            return results
                .into_iter()
                .map(|restored| {
                    if restored {
                        Ok(())
                    } else {
                        Err(crate::error::WorkspaceError::MacOs(format!(
                            "could not find a distinct matching live window for {}",
                            first.app_name
                        )))
                    }
                })
                .collect();
        }
        Ok(None) => {}
        Err(error) => {
            let message = error.to_string();
            return jobs
                .iter()
                .map(|_| Err(crate::error::WorkspaceError::MacOs(message.clone())))
                .collect();
        }
    }

    jobs.iter()
        .map(|_| {
            Err(crate::error::WorkspaceError::MacOs(format!(
                "could not find matching live windows for {}",
                first.app_name
            )))
        })
        .collect()
}

fn prepare_window_group(
    jobs: &[RestoreJob<'_>],
    candidate: CandidatePids,
    options: RestoreOptions,
) -> Vec<i32> {
    let first = jobs[0].window;
    ensure_window_count(first, jobs.len(), options, candidate.launched);
    current_pids(first)
}

fn set_group_frames_with_retry(
    first: &WindowSnapshot,
    desired_count: usize,
    targets: &[(&WindowSnapshot, Frame)],
    initial_pids: Vec<i32>,
) -> Result<Option<Vec<bool>>> {
    let mut pids = initial_pids;
    for attempt in 0..=WINDOW_READY_WAIT_ATTEMPTS {
        for pid in &pids {
            match accessibility::set_window_frames(*pid, targets) {
                Ok(results) if results.iter().any(|result| *result) => return Ok(Some(results)),
                Ok(_) => continue,
                Err(error) => return Err(error),
            }
        }

        if attempt == WINDOW_READY_WAIT_ATTEMPTS {
            break;
        }
        if visible_window_count(first) < desired_count {
            thread::sleep(WINDOW_READY_WAIT_INTERVAL);
        } else {
            thread::sleep(WINDOW_READY_WAIT_INTERVAL / 2);
        }
        pids = current_pids(first);
    }

    Ok(None)
}

fn ensure_window_count(
    window: &WindowSnapshot,
    desired_count: usize,
    options: RestoreOptions,
    launched: bool,
) {
    if window.bundle_id.is_none() {
        return;
    }
    let Some(bundle_id) = window.bundle_id.as_deref() else {
        return;
    };
    if options.dev_mode
        && is_dev_mode_protected_bundle(bundle_id)
        && current_pids(window).is_empty()
    {
        return;
    }

    wait_for_window_count(window, desired_count, options);
    for _ in 0..desired_count {
        if visible_window_count(window) >= desired_count {
            return;
        }
        let _ = app::create_new_window(bundle_id, &window.process_name);
        wait_for_window_count(window, desired_count, options);
        if !launched && visible_window_count(window) == 0 {
            return;
        }
    }
}

fn wait_for_window_count(window: &WindowSnapshot, desired_count: usize, _options: RestoreOptions) {
    for _ in 0..WINDOW_READY_WAIT_ATTEMPTS {
        if visible_window_count(window) >= desired_count {
            return;
        }
        thread::sleep(WINDOW_READY_WAIT_INTERVAL);
    }
}

fn visible_window_count(window: &WindowSnapshot) -> usize {
    current_pids(window)
        .into_iter()
        .filter_map(|pid| accessibility::window_count(pid).ok())
        .sum()
}

fn restore_chrome_group(jobs: &[RestoreJob<'_>], options: RestoreOptions) -> Vec<Result<()>> {
    let first = jobs[0].window;
    let targets: Vec<(&WindowSnapshot, Frame)> = jobs
        .iter()
        .map(|job| (job.window, job.target_frame))
        .collect();

    tracing::debug!(
        app = %first.app_name,
        count = jobs.len(),
        "restoring Chrome window group via AppleScript"
    );

    // If Chrome isn't running and dev-mode protects it, skip launching.
    if options.dev_mode
        && first
            .bundle_id
            .as_deref()
            .map(is_dev_mode_protected_bundle)
            .unwrap_or(false)
        && current_pids(first).is_empty()
    {
        return jobs
            .iter()
            .map(|_| {
                Err(crate::error::WorkspaceError::MacOs(
                    "Chrome is not running and --dev-mode is set; skipping launch".to_string(),
                ))
            })
            .collect();
    }

    match chrome::restore_windows(&targets) {
        Ok(true) => {
            // Wait for Chrome to actually produce the expected window count
            // before declaring success. AppleScript `make new window` is
            // synchronous, so on the happy path this returns immediately.
            wait_for_window_count(first, jobs.len(), options);
            jobs.iter().map(|_| Ok(())).collect()
        }
        Ok(false) => jobs
            .iter()
            .map(|_| {
                Err(crate::error::WorkspaceError::MacOs(
                    "Chrome restore script did not run".to_string(),
                ))
            })
            .collect(),
        Err(error) => {
            let message = error.to_string();
            jobs.iter()
                .map(|_| Err(crate::error::WorkspaceError::MacOs(message.clone())))
                .collect()
        }
    }
}

fn restore_window(
    window: &WindowSnapshot,
    target_frame: Frame,
    options: RestoreOptions,
) -> Result<()> {
    tracing::debug!(
        app = %window.app_name,
        bundle_id = ?window.bundle_id,
        pid = window.pid,
        "restoring window"
    );
    let candidate = current_or_launched_pids(window, options);
    tracing::debug!(app = %window.app_name, pids = ?candidate.pids, launched = candidate.launched, "candidate restore pids");
    if set_frame_with_window_ready_retry(
        &candidate.pids,
        candidate.launched,
        || current_pids(window),
        |pids| try_set_window_frame(pids, window, target_frame),
        || {
            tracing::debug!(app = %window.app_name, "waiting for launched app window to become AX-visible");
            thread::sleep(WINDOW_READY_WAIT_INTERVAL);
        },
    )? {
        return Ok(());
    }

    Err(crate::error::WorkspaceError::MacOs(format!(
        "could not find a matching live window for {}",
        window.app_name
    )))
}

fn try_set_window_frame(
    pids: &[i32],
    window: &WindowSnapshot,
    target_frame: Frame,
) -> Result<bool> {
    for pid in pids {
        tracing::debug!(app = %window.app_name, pid, "setting AX window frame");
        if accessibility::set_window_frame(*pid, window, target_frame)? {
            return Ok(true);
        }
    }

    Ok(false)
}

fn current_or_launched_pids(window: &WindowSnapshot, options: RestoreOptions) -> CandidatePids {
    if let Some(bundle_id) = &window.bundle_id {
        tracing::debug!(%bundle_id, "looking up running app pids");
        let mut pids = app::running_pids_for_bundle(bundle_id);
        let mut launched = false;
        if pids.is_empty() {
            if options.dev_mode && is_dev_mode_protected_bundle(bundle_id) {
                tracing::debug!(%bundle_id, "dev-mode protected app is not running; launch skipped");
                return CandidatePids {
                    pids: Vec::new(),
                    launched: false,
                };
            }
            tracing::debug!(%bundle_id, "app not running, launching");
            launched = app::launch_bundle(bundle_id).unwrap_or(false);
            for _ in 0..APP_LAUNCH_WAIT_ATTEMPTS {
                thread::sleep(APP_LAUNCH_WAIT_INTERVAL);
                pids = app::running_pids_for_bundle(bundle_id);
                if !pids.is_empty() {
                    break;
                }
            }
        }
        if !pids.is_empty() {
            return CandidatePids { pids, launched };
        }
    }

    CandidatePids {
        pids: vec![window.pid],
        launched: false,
    }
}

fn is_dev_mode_protected_bundle(bundle_id: &str) -> bool {
    matches!(
        bundle_id,
        "com.microsoft.VSCode" | "com.todesktop.230313mzl4w4u92"
    )
}

fn window_ready_retry_attempts(launched: bool) -> usize {
    if launched {
        WINDOW_READY_WAIT_ATTEMPTS
    } else {
        0
    }
}

fn set_frame_with_window_ready_retry(
    initial_pids: &[i32],
    launched: bool,
    mut current_pids: impl FnMut() -> Vec<i32>,
    mut set_frame: impl FnMut(&[i32]) -> Result<bool>,
    mut wait: impl FnMut(),
) -> Result<bool> {
    if set_frame(initial_pids)? {
        return Ok(true);
    }

    for _ in 0..window_ready_retry_attempts(launched) {
        wait();
        let pids = current_pids();
        if set_frame(&pids)? {
            return Ok(true);
        }
    }

    Ok(false)
}

fn replay_z_order(windows: &[&WindowSnapshot]) {
    let mut windows = windows.to_vec();
    windows.sort_by_key(|window| std::cmp::Reverse(window.z_order.unwrap_or(u32::MAX)));
    tracing::debug!(count = windows.len(), "replaying saved z-order");
    for window in windows {
        tracing::debug!(
            app = %window.app_name,
            bundle_id = ?window.bundle_id,
            z_order = ?window.z_order,
            "raising window for z-order replay"
        );

        if let Some(bundle_id) = &window.bundle_id {
            let _ = app::activate_bundle(bundle_id);
        }

        for pid in current_pids(window) {
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

fn current_pids(window: &WindowSnapshot) -> Vec<i32> {
    if let Some(bundle_id) = &window.bundle_id {
        let pids = app::running_pids_for_bundle(bundle_id);
        if !pids.is_empty() {
            return pids;
        }
    }

    vec![window.pid]
}

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

fn action(
    window: &WindowSnapshot,
    target_frame: Frame,
    status: RestoreStatus,
    message: Option<String>,
) -> RestoreAction {
    RestoreAction {
        bundle_id: window.bundle_id.clone(),
        app_name: window.app_name.clone(),
        title: window.title.clone(),
        saved_frame: window.frame,
        target_frame,
        display_id: window.display_id.clone(),
        status,
        message,
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

    #[test]
    fn unknown_apps_are_skipped_before_restore() {
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
        let mut unknown = window(
            Frame {
                x: 0.0,
                y: 0.0,
                width: 500.0,
                height: 400.0,
            },
            &display,
        );
        unknown.bundle_id = Some("dev.example.Unknown".to_string());
        unknown.app_name = "Unknown".to_string();

        let snapshot = WorkspaceSnapshot {
            version: crate::model::SNAPSHOT_VERSION,
            name: "test".to_string(),
            created_at: chrono::Utc::now(),
            host: crate::model::HostInfo {
                hostname: "host".to_string(),
                os: "macos".to_string(),
                arch: "aarch64".to_string(),
            },
            displays: vec![display],
            windows: vec![unknown],
        };

        let report = restore_workspace(
            &snapshot,
            RestoreOptions {
                dry_run: true,
                dev_mode: false,
                mode: RestoreMode::Safe,
            },
        )
        .unwrap();

        assert_eq!(report.restored, 0);
        assert_eq!(report.skipped, 1);
        assert_eq!(report.actions[0].status, RestoreStatus::Skipped);
        assert!(report.actions[0]
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("not in the supported restore allowlist"));
    }

    #[test]
    fn launched_apps_get_window_ready_retry_budget() {
        assert_eq!(window_ready_retry_attempts(false), 0);
        assert_eq!(
            window_ready_retry_attempts(true),
            WINDOW_READY_WAIT_ATTEMPTS
        );
    }

    #[test]
    fn closed_launched_app_retries_until_window_is_ready() {
        let mut attempts = 0;
        let mut waits = 0;

        let restored = set_frame_with_window_ready_retry(
            &[101],
            true,
            || vec![202],
            |_| {
                attempts += 1;
                Ok(attempts == 4)
            },
            || waits += 1,
        )
        .unwrap();

        assert!(restored);
        assert_eq!(attempts, 4);
        assert_eq!(waits, 3);
    }

    #[test]
    fn already_running_app_does_not_retry_after_no_match() {
        let mut attempts = 0;
        let mut waits = 0;

        let restored = set_frame_with_window_ready_retry(
            &[101],
            false,
            || vec![202],
            |_| {
                attempts += 1;
                Ok(false)
            },
            || waits += 1,
        )
        .unwrap();

        assert!(!restored);
        assert_eq!(attempts, 1);
        assert_eq!(waits, 0);
    }

    #[test]
    fn dev_mode_protects_editor_bundles() {
        assert!(is_dev_mode_protected_bundle("com.microsoft.VSCode"));
        assert!(is_dev_mode_protected_bundle(
            "com.todesktop.230313mzl4w4u92"
        ));
        assert!(!is_dev_mode_protected_bundle("com.apple.Notes"));
    }
}

// -----------------------------------------------------------------------
// Plan / verify / doctor helpers
// -----------------------------------------------------------------------

use crate::filter::should_capture_window;
use crate::macos::window;
use crate::plan::{plan_restore, LiveWindow, PlanOptions, RestorePlan, WorldState};
use crate::verify as verify_mod;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
    // matching degrades to geometry-only and Chrome tab attribution falls
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
