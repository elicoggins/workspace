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
    mark_fullscreen_windows(&mut windows);
    attach_chrome_tabs(&mut windows);

    Ok(WorkspaceSnapshot {
        version: SNAPSHOT_VERSION,
        name: name.to_string(),
        created_at: Utc::now(),
        host: HostInfo {
            hostname: hostname(),
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
        },
        displays,
        windows,
    })
}

/// The `HOSTNAME` env var is rarely set by interactive shells; ask libc.
fn hostname() -> String {
    #[cfg(unix)]
    {
        let mut buf = [0u8; 256];
        let ok =
            unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) } == 0;
        if ok {
            if let Some(end) = buf.iter().position(|byte| *byte == 0) {
                if let Ok(name) = std::str::from_utf8(&buf[..end]) {
                    if !name.is_empty() {
                        return name.to_string();
                    }
                }
            }
        }
    }
    std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown".to_string())
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

/// The CG window list cannot see fullscreen status; enrich captured windows
/// via AX so the planner's fullscreen skip-gate acts on real data. Best
/// effort: without Accessibility permission this is a no-op.
fn mark_fullscreen_windows(windows: &mut [WindowSnapshot]) {
    use crate::macos::accessibility;

    let pids: std::collections::HashSet<i32> = windows.iter().map(|window| window.pid).collect();
    for pid in pids {
        let Ok(states) = accessibility::ax_window_states(pid) else {
            continue;
        };
        let fullscreen: Vec<_> = states
            .into_iter()
            .filter(|state| state.fullscreen)
            .collect();
        if fullscreen.is_empty() {
            continue;
        }
        for window in windows.iter_mut().filter(|window| window.pid == pid) {
            let matched = fullscreen.iter().any(|state| {
                let title_match =
                    state.title.is_some() && state.title.as_deref() == window.title.as_deref();
                let frame_match = state
                    .frame
                    .map(|frame| frames_approx(frame, window.frame))
                    .unwrap_or(false);
                title_match || frame_match
            });
            if matched {
                window.fullscreen = true;
            }
        }
    }
}

fn frames_approx(a: Frame, b: Frame) -> bool {
    (a.x - b.x).abs() <= 2.0
        && (a.y - b.y).abs() <= 2.0
        && (a.width - b.width).abs() <= 2.0
        && (a.height - b.height).abs() <= 2.0
}

fn attach_chrome_tabs(windows: &mut [WindowSnapshot]) {
    for app in crate::app_support::tab_capable_apps() {
        // Only shell out to osascript for browsers that actually have
        // captured windows.
        if !windows
            .iter()
            .any(|window| window.bundle_id.as_deref() == Some(app.bundle_id))
        {
            continue;
        }
        assign_browser_tabs(
            windows,
            app.bundle_id,
            &chrome::capture_windows(app.bundle_id),
        );
    }
}

fn assign_browser_tabs(
    windows: &mut [WindowSnapshot],
    bundle_id: &str,
    chrome_windows: &[chrome::ChromeWindowTabs],
) {
    if chrome_windows.is_empty() {
        return;
    }

    let snapshot_indices: Vec<usize> = windows
        .iter()
        .enumerate()
        .filter(|(_, window)| window.bundle_id.as_deref() == Some(bundle_id))
        .map(|(index, _)| index)
        .collect();

    let mut used = vec![false; chrome_windows.len()];
    let mut assigned = vec![false; snapshot_indices.len()];

    // Pass 1: assign by title, but only on a real match. A zero score means
    // "no evidence" — falling back to an arbitrary window here would attach
    // the wrong tab set whenever titles differ between CG and AppleScript.
    for (slot, &window_index) in snapshot_indices.iter().enumerate() {
        let window = &windows[window_index];
        let matched_index = chrome_windows
            .iter()
            .enumerate()
            .filter(|(index, _)| !used[*index])
            .map(|(index, chrome_window)| {
                (
                    index,
                    chrome_tab_match_score(window, chrome_window.title.as_deref()),
                )
            })
            .filter(|(_, score)| *score > 0)
            .max_by_key(|(_, score)| *score)
            .map(|(index, _)| index);

        if let Some(index) = matched_index {
            used[index] = true;
            assigned[slot] = true;
            windows[window_index].browser_tabs = chrome_windows[index].tabs.clone();
        }
    }

    // Pass 2: pair remaining windows in front-to-back order. Both the CG
    // window list (snapshot order) and Chrome's AppleScript window list are
    // ordered front-to-back, so positional pairing is the best fallback.
    let mut remaining = (0..chrome_windows.len()).filter(|index| !used[*index]);
    for (slot, &window_index) in snapshot_indices.iter().enumerate() {
        if assigned[slot] {
            continue;
        }
        let Some(index) = remaining.next() else {
            break;
        };
        windows[window_index].browser_tabs = chrome_windows[index].tabs.clone();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::macos::chrome::ChromeWindowTabs;
    use crate::model::BrowserTab;

    fn chrome_snapshot_window(title: Option<&str>) -> WindowSnapshot {
        WindowSnapshot {
            window_id: 1,
            app_name: "Google Chrome".to_string(),
            process_name: "Google Chrome".to_string(),
            bundle_id: Some("com.google.Chrome".to_string()),
            pid: 42,
            title: title.map(str::to_string),
            frame: Frame {
                x: 0.0,
                y: 0.0,
                width: 800.0,
                height: 600.0,
            },
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

    fn tabs(url: &str) -> Vec<BrowserTab> {
        vec![BrowserTab {
            title: None,
            url: url.to_string(),
            active: true,
        }]
    }

    #[test]
    fn chrome_tabs_attach_by_title_when_titles_match() {
        let mut windows = vec![
            chrome_snapshot_window(Some("Docs")),
            chrome_snapshot_window(Some("Search")),
        ];
        // Reverse order relative to the snapshot windows.
        let chrome_windows = vec![
            ChromeWindowTabs {
                title: Some("Search".to_string()),
                tabs: tabs("https://example.com/search"),
            },
            ChromeWindowTabs {
                title: Some("Docs".to_string()),
                tabs: tabs("https://example.com/docs"),
            },
        ];

        assign_browser_tabs(&mut windows, "com.google.Chrome", &chrome_windows);

        assert_eq!(windows[0].browser_tabs[0].url, "https://example.com/docs");
        assert_eq!(windows[1].browser_tabs[0].url, "https://example.com/search");
    }

    #[test]
    fn chrome_tabs_fall_back_to_front_to_back_order_when_titles_are_missing() {
        // No CG titles at all (e.g. Screen Recording permission missing) —
        // tabs must still land on distinct windows in z-order, not on
        // whichever window an arbitrary max-by-key tie-break picked.
        let mut windows = vec![chrome_snapshot_window(None), chrome_snapshot_window(None)];
        let chrome_windows = vec![
            ChromeWindowTabs {
                title: Some("Front".to_string()),
                tabs: tabs("https://example.com/front"),
            },
            ChromeWindowTabs {
                title: Some("Back".to_string()),
                tabs: tabs("https://example.com/back"),
            },
        ];

        assign_browser_tabs(&mut windows, "com.google.Chrome", &chrome_windows);

        assert_eq!(windows[0].browser_tabs[0].url, "https://example.com/front");
        assert_eq!(windows[1].browser_tabs[0].url, "https://example.com/back");
    }

    #[test]
    fn partial_title_matches_do_not_steal_other_windows_tabs() {
        let mut windows = vec![
            chrome_snapshot_window(Some("Unrelated")),
            chrome_snapshot_window(Some("Docs")),
        ];
        let chrome_windows = vec![ChromeWindowTabs {
            title: Some("Docs".to_string()),
            tabs: tabs("https://example.com/docs"),
        }];

        assign_browser_tabs(&mut windows, "com.google.Chrome", &chrome_windows);

        // The titled match wins; the unrelated window gets nothing rather
        // than stealing the only tab set.
        assert!(windows[0].browser_tabs.is_empty());
        assert_eq!(windows[1].browser_tabs[0].url, "https://example.com/docs");
    }
}
