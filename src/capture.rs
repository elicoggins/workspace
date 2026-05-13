use chrono::Utc;

use crate::{
    error::Result,
    filter::should_capture_window,
    macos::{app, chrome, display, window},
    model::{
        DisplaySnapshot, Frame, HostInfo, WindowSnapshot, WorkspaceSnapshot, SNAPSHOT_VERSION,
    },
};

pub fn capture_workspace(name: &str) -> Result<WorkspaceSnapshot> {
    let displays = display::current_displays()?;
    let raw_windows = window::enumerate_windows()?;
    let mut windows = Vec::new();

    for raw in raw_windows.into_iter().filter(should_capture_window) {
        let app_info = app::application_for_pid(raw.owner_pid);
        let display = dominant_display(raw.frame, &displays);
        let display_frame = display.map(|display| display.frame);
        let display_relative_frame = display_frame.map(|frame| raw.frame.relative_to(frame));

        windows.push(WindowSnapshot {
            window_id: raw.window_id,
            app_name: app_info
                .as_ref()
                .and_then(|app| app.localized_name.clone())
                .unwrap_or_else(|| raw.owner_name.clone()),
            process_name: app_info
                .as_ref()
                .and_then(|app| app.process_name.clone())
                .unwrap_or_else(|| raw.owner_name.clone()),
            bundle_id: app_info.and_then(|app| app.bundle_id),
            pid: raw.owner_pid,
            title: raw.window_title,
            frame: raw.frame,
            display_id: display.map(|display| display.id.clone()),
            display_frame,
            display_relative_frame,
            z_order: Some(raw.z_order),
            fullscreen: false,
            minimized: false,
            enabled: true,
            browser_tabs: Vec::new(),
        });
    }

    windows.sort_by_key(|window| window.z_order.unwrap_or(u32::MAX));
    attach_chrome_tabs(&mut windows);

    Ok(WorkspaceSnapshot {
        version: SNAPSHOT_VERSION,
        name: name.to_string(),
        created_at: Utc::now(),
        host: HostInfo {
            hostname: std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown".to_string()),
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
        },
        displays,
        windows,
    })
}

fn dominant_display(frame: Frame, displays: &[DisplaySnapshot]) -> Option<&DisplaySnapshot> {
    displays.iter().max_by(|left, right| {
        let left_area = frame.intersection_area(left.frame);
        let right_area = frame.intersection_area(right.frame);
        left_area
            .partial_cmp(&right_area)
            .unwrap_or(std::cmp::Ordering::Equal)
    })
}

fn attach_chrome_tabs(windows: &mut [WindowSnapshot]) {
    let chrome_windows = chrome::capture_windows();
    if chrome_windows.is_empty() {
        return;
    }

    let mut used = vec![false; chrome_windows.len()];
    for window in windows
        .iter_mut()
        .filter(|window| window.bundle_id.as_deref() == Some("com.google.Chrome"))
    {
        let matched_index = chrome_windows
            .iter()
            .enumerate()
            .filter(|(index, _)| !used[*index])
            .max_by_key(|(_, chrome_window)| {
                chrome_tab_match_score(window, chrome_window.title.as_deref())
            })
            .map(|(index, _)| index);

        if let Some(index) = matched_index {
            used[index] = true;
            window.browser_tabs = chrome_windows[index].tabs.clone();
        }
    }
}

fn chrome_tab_match_score(window: &WindowSnapshot, chrome_title: Option<&str>) -> i32 {
    match (window.title.as_deref(), chrome_title) {
        (Some(saved), Some(candidate)) if saved == candidate => 100,
        (Some(saved), Some(candidate))
            if saved.contains(candidate) || candidate.contains(saved) =>
        {
            50
        }
        _ => 0,
    }
}
