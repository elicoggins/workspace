use crate::{
    error::{Result, WorkspaceError},
    model::{BrowserTab, Frame, WindowSnapshot},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChromeWindowTabs {
    pub title: Option<String>,
    pub tabs: Vec<BrowserTab>,
}

#[cfg(target_os = "macos")]
mod imp {
    use std::process::{Command, Stdio};

    use serde::Deserialize;

    use super::*;

    const CAPTURE_SCRIPT: &str = r#"
function run(argv) {
  const chrome = Application(argv[0]);
  if (!chrome.running()) {
    return JSON.stringify([]);
  }
  return JSON.stringify(chrome.windows().map((window) => {
    const activeTabIndex = window.activeTabIndex();
    return {
      title: window.name(),
      tabs: window.tabs().map((tab, index) => ({
        title: tab.title(),
        url: tab.url(),
        active: index + 1 === activeTabIndex
      }))
    };
  }));
}
"#;

    #[derive(Debug, Deserialize)]
    struct RawChromeWindow {
        title: Option<String>,
        #[serde(default)]
        tabs: Vec<RawChromeTab>,
    }

    #[derive(Debug, Deserialize)]
    struct RawChromeTab {
        title: Option<String>,
        url: Option<String>,
        #[serde(default)]
        active: bool,
    }

    pub fn capture_windows(bundle_id: &str) -> Vec<ChromeWindowTabs> {
        let output = Command::new("/usr/bin/osascript")
            .arg("-l")
            .arg("JavaScript")
            .arg("-e")
            .arg(CAPTURE_SCRIPT)
            .arg(bundle_id)
            .output();

        let Ok(output) = output else {
            return Vec::new();
        };
        if !output.status.success() {
            return Vec::new();
        }

        parse_chrome_windows_json(&String::from_utf8_lossy(&output.stdout)).unwrap_or_default()
    }

    /// One window per spec entry. The script NEVER rewrites tabs of an
    /// existing non-blank window: for each entry it reuses an unused blank
    /// new-tab window (e.g. the one Chrome opens on a cold launch) or creates
    /// a fresh window, fills its tabs, and sets its bounds. Windows the
    /// planner matched live are handled by `Reposition` ops and stay intact.
    const RESTORE_SCRIPT: &str = r#"
function run(argv) {
  const chrome = Application(argv[0]);
  const specs = JSON.parse(argv[1]);
  chrome.activate();
  const used = [];
  const errors = [];
  const isBlank = function (w) {
    try {
      if (w.tabs().length !== 1) return false;
      const url = w.activeTab.url() || '';
      return url === '' || url === 'about:blank' || /^[a-z-]+:\/\/newtab/.test(url);
    } catch (e) {
      return false;
    }
  };
  const windowIds = function () {
    return chrome.windows().map(function (w) {
      try { return w.id(); } catch (e) { return null; }
    });
  };
  // A freshly created window's initial tab materializes asynchronously;
  // when Chrome is busy (e.g. it just processed an AX reposition) indexing
  // tabs[0] immediately throws "Wrong index". Poll until the tab exists.
  const waitForFirstTab = function (w) {
    for (let i = 0; i < 40; i++) {
      try { if (w.tabs().length > 0) return true; } catch (e) {}
      delay(0.05);
    }
    return false;
  };
  specs.forEach(function (spec, specIndex) {
    try {
      let target = null;
      const wins = chrome.windows();
      for (let i = 0; i < wins.length; i++) {
        let id;
        try { id = wins[i].id(); } catch (e) { continue; }
        if (used.indexOf(id) !== -1) continue;
        if (isBlank(wins[i])) { target = wins[i]; used.push(id); break; }
      }
      if (!target) {
        // The object handed to push() does not track the created window;
        // re-acquire it by diffing window ids before and after.
        const before = windowIds();
        // push() can throw "Wrong index" while Chrome is busy (e.g. right
        // after an AX reposition) even though the window IS created — ignore
        // the throw and locate the new window by id-diff below.
        try { chrome.windows.push(chrome.Window()); } catch (e) {}
        for (let attempt = 0; attempt < 40 && !target; attempt++) {
          const after = chrome.windows();
          for (let i = 0; i < after.length; i++) {
            let id;
            try { id = after[i].id(); } catch (e) { continue; }
            if (before.indexOf(id) === -1) { target = after[i]; used.push(id); break; }
          }
          if (!target) delay(0.05);
        }
        if (!target) throw new Error('created a window but could not find it');
      }
      if (spec.urls.length > 0) {
        if (!waitForFirstTab(target)) throw new Error('window has no tabs after waiting');
        target.tabs[0].url = spec.urls[0];
        for (let i = 1; i < spec.urls.length; i++) {
          target.tabs.push(chrome.Tab({ url: spec.urls[i] }));
        }
        try { target.activeTabIndex = spec.active; } catch (e) {}
      }
      target.bounds = { x: spec.x, y: spec.y, width: spec.width, height: spec.height };
    } catch (e) {
      errors.push('window ' + specIndex + ': ' + e);
    }
  });
  return errors.join('; ');
}
"#;

    /// Re-open saved tabs that are missing from an already-matched live
    /// window. Safe by construction: only ADDS tabs (by URL), never closes
    /// or reorders anything the user has open. The window is located by its
    /// bounds — the executor has just AX-moved it to `target`.
    const RECONCILE_SCRIPT: &str = r#"
function run(argv) {
  const chrome = Application(argv[0]);
  const spec = JSON.parse(argv[1]);
  if (!chrome.running()) return 'error: browser not running';
  const wins = chrome.windows();
  let best = null, bestDist = Infinity;
  for (let i = 0; i < wins.length; i++) {
    let b;
    try { b = wins[i].bounds(); } catch (e) { continue; }
    const d = Math.abs(b.x - spec.x) + Math.abs(b.y - spec.y) +
              Math.abs(b.width - spec.width) + Math.abs(b.height - spec.height);
    if (d < bestDist) { bestDist = d; best = wins[i]; }
  }
  if (!best || bestDist > 40) return 'error: no window near target bounds (dist ' + bestDist + ')';
  const existing = best.tabs().map(function (t) {
    try { return t.url() || ''; } catch (e) { return ''; }
  });
  // Identity guard: if the window shares NO saved tab and is not a blank
  // new-tab window, the geometry match picked a different window (likely
  // because the saved one was closed). Grafting saved tabs onto it would
  // pollute an unrelated window — skip instead.
  const isBlankWindow = existing.length === 1 &&
    (existing[0] === '' || existing[0] === 'about:blank' || /^[a-z-]+:\/\/newtab/.test(existing[0]));
  const overlap = existing.filter(function (u) { return spec.urls.indexOf(u) !== -1; }).length;
  if (!isBlankWindow && overlap === 0) return 'error: matched window shares no saved tabs; not reconciling';
  let added = 0;
  for (let i = 0; i < spec.urls.length; i++) {
    if (existing.indexOf(spec.urls[i]) === -1) {
      try { best.tabs.push(chrome.Tab({ url: spec.urls[i] })); added++; } catch (e) {}
    }
  }
  if (spec.activeUrl) {
    const now = best.tabs().map(function (t) {
      try { return t.url() || ''; } catch (e) { return ''; }
    });
    const idx = now.indexOf(spec.activeUrl);
    if (idx !== -1) { try { best.activeTabIndex = idx + 1; } catch (e) {} }
  }
  return 'added ' + added;
}
"#;

    /// Move a browser window by scripting `bounds`. Chromium's AX
    /// implementation intermittently rejects `AXSize` writes
    /// (kAXErrorFailure -25200); the scripting dictionary is deterministic.
    /// The window is located by its current frame.
    const SET_BOUNDS_SCRIPT: &str = r#"
function run(argv) {
  const chrome = Application(argv[0]);
  const spec = JSON.parse(argv[1]);
  if (!chrome.running()) return 'error: browser not running';
  const wins = chrome.windows();
  let best = null, bestDist = Infinity;
  for (let i = 0; i < wins.length; i++) {
    let b;
    try { b = wins[i].bounds(); } catch (e) { continue; }
    const d = Math.abs(b.x - spec.fx) + Math.abs(b.y - spec.fy) +
              Math.abs(b.width - spec.fw) + Math.abs(b.height - spec.fh);
    if (d < bestDist) { bestDist = d; best = wins[i]; }
  }
  if (!best || bestDist > 60) return 'error: no window near source bounds (dist ' + bestDist + ')';
  best.bounds = { x: spec.tx, y: spec.ty, width: spec.tw, height: spec.th };
  return 'ok';
}
"#;

    /// Returns `Ok(true)` when a window near `from` was moved to `to`.
    pub fn set_window_bounds(bundle_id: &str, from: Frame, to: Frame) -> Result<bool> {
        let spec = serde_json::json!({
            "fx": from.x.round() as i64,
            "fy": from.y.round() as i64,
            "fw": from.width.round() as i64,
            "fh": from.height.round() as i64,
            "tx": to.x.round() as i64,
            "ty": to.y.round() as i64,
            "tw": to.width.round() as i64,
            "th": to.height.round() as i64,
        })
        .to_string();
        let output = Command::new("/usr/bin/osascript")
            .arg("-l")
            .arg("JavaScript")
            .arg("-e")
            .arg(SET_BOUNDS_SCRIPT)
            .arg(bundle_id)
            .arg(&spec)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|source| {
                WorkspaceError::MacOs(format!("failed to run browser bounds script: {source}"))
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(WorkspaceError::MacOs(format!(
                "browser bounds script exited with {}: {}",
                output.status,
                stderr.trim()
            )));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stdout = stdout.trim();
        if stdout == "ok" {
            Ok(true)
        } else {
            tracing::debug!(%stdout, "browser bounds script could not move window");
            Ok(false)
        }
    }

    /// Returns `Ok(Some(n))` with the number of re-opened tabs, or `Ok(None)`
    /// when the target window could not be located.
    pub fn reconcile_window_tabs(
        bundle_id: &str,
        saved: &WindowSnapshot,
        target: Frame,
    ) -> Result<Option<usize>> {
        if saved.browser_tabs.is_empty() {
            return Ok(Some(0));
        }
        let spec = serde_json::json!({
            "urls": saved.browser_tabs.iter().map(|t| t.url.as_str()).collect::<Vec<_>>(),
            "activeUrl": saved.browser_tabs.iter().find(|t| t.active).map(|t| t.url.as_str()),
            "x": target.x.round() as i64,
            "y": target.y.round() as i64,
            "width": target.width.round() as i64,
            "height": target.height.round() as i64,
        })
        .to_string();
        tracing::debug!(%spec, "running Chrome tab reconcile");
        let output = Command::new("/usr/bin/osascript")
            .arg("-l")
            .arg("JavaScript")
            .arg("-e")
            .arg(RECONCILE_SCRIPT)
            .arg(bundle_id)
            .arg(&spec)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|source| {
                WorkspaceError::MacOs(format!("failed to run Chrome reconcile script: {source}"))
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(WorkspaceError::MacOs(format!(
                "Chrome reconcile script exited with {}: {}",
                output.status,
                stderr.trim()
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stdout = stdout.trim();
        if let Some(count) = stdout.strip_prefix("added ") {
            Ok(count.parse::<usize>().ok())
        } else {
            tracing::warn!(%stdout, "Chrome tab reconcile could not locate window");
            Ok(None)
        }
    }

    pub fn restore_windows(bundle_id: &str, windows: &[(&WindowSnapshot, Frame)]) -> Result<bool> {
        if windows.is_empty() {
            return Ok(true);
        }
        let spec = restore_spec_json(windows);
        tracing::debug!(window_count = windows.len(), %spec, "running Chrome JXA restore");
        let output = Command::new("/usr/bin/osascript")
            .arg("-l")
            .arg("JavaScript")
            .arg("-e")
            .arg(RESTORE_SCRIPT)
            .arg(bundle_id)
            .arg(&spec)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|source| {
                WorkspaceError::MacOs(format!("failed to run Chrome restore script: {source}"))
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(WorkspaceError::MacOs(format!(
                "Chrome restore script exited with {}: {}",
                output.status,
                stderr.trim()
            )));
        }

        let errors = String::from_utf8_lossy(&output.stdout);
        let errors = errors.trim();
        if errors.is_empty() {
            Ok(true)
        } else {
            tracing::warn!(%errors, "Chrome restore completed with per-window errors");
            Ok(false)
        }
    }

    pub fn parse_chrome_windows_json(input: &str) -> serde_json::Result<Vec<ChromeWindowTabs>> {
        let raw: Vec<RawChromeWindow> = serde_json::from_str(input.trim())?;
        Ok(raw
            .into_iter()
            .map(|window| ChromeWindowTabs {
                title: window.title.filter(|title| !title.is_empty()),
                tabs: window
                    .tabs
                    .into_iter()
                    .filter_map(|tab| {
                        let url = tab.url?.trim().to_string();
                        if url.is_empty() {
                            return None;
                        }
                        Some(BrowserTab {
                            title: tab.title.filter(|title| !title.is_empty()),
                            url,
                            active: tab.active,
                        })
                    })
                    .collect(),
            })
            .collect())
    }

    /// Serialize the restore targets to the JSON spec consumed by
    /// `RESTORE_SCRIPT`. Passing data as an osascript argument (instead of
    /// splicing it into script source) sidesteps quoting/escaping entirely.
    pub fn restore_spec_json(windows: &[(&WindowSnapshot, Frame)]) -> String {
        let specs: Vec<serde_json::Value> = windows
            .iter()
            .map(|(window, target)| {
                let urls: Vec<&str> = window
                    .browser_tabs
                    .iter()
                    .map(|tab| tab.url.as_str())
                    .collect();
                let active = window
                    .browser_tabs
                    .iter()
                    .position(|tab| tab.active)
                    .map(|index| index + 1)
                    .unwrap_or(1);
                serde_json::json!({
                    "urls": urls,
                    "active": active,
                    "x": target.x.round() as i64,
                    "y": target.y.round() as i64,
                    "width": target.width.round() as i64,
                    "height": target.height.round() as i64,
                })
            })
            .collect();
        serde_json::to_string(&specs).expect("chrome restore spec always serializes")
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::*;

    pub fn capture_windows(_bundle_id: &str) -> Vec<ChromeWindowTabs> {
        Vec::new()
    }

    pub fn restore_windows(
        _bundle_id: &str,
        _windows: &[(&WindowSnapshot, Frame)],
    ) -> Result<bool> {
        Err(WorkspaceError::UnsupportedPlatform)
    }

    pub fn reconcile_window_tabs(
        _bundle_id: &str,
        _saved: &WindowSnapshot,
        _target: Frame,
    ) -> Result<Option<usize>> {
        Err(WorkspaceError::UnsupportedPlatform)
    }

    pub fn set_window_bounds(_bundle_id: &str, _from: Frame, _to: Frame) -> Result<bool> {
        Err(WorkspaceError::UnsupportedPlatform)
    }
}

pub use imp::{capture_windows, reconcile_window_tabs, restore_windows, set_window_bounds};

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::imp::{parse_chrome_windows_json, restore_spec_json};
    use crate::model::{BrowserTab, Frame, WindowSnapshot};

    fn chrome_window(tabs: Vec<BrowserTab>) -> WindowSnapshot {
        WindowSnapshot {
            window_id: 1,
            app_name: "Google Chrome".to_string(),
            process_name: "Google Chrome".to_string(),
            bundle_id: Some("com.google.Chrome".to_string()),
            pid: 42,
            title: Some("Example".to_string()),
            frame: Frame {
                x: 0.0,
                y: 0.0,
                width: 900.0,
                height: 700.0,
            },
            display_id: None,
            display_frame: None,
            display_relative_frame: None,
            z_order: Some(0),
            fullscreen: false,
            minimized: false,
            enabled: true,
            browser_tabs: tabs,
        }
    }

    fn frame(x: f64, y: f64, w: f64, h: f64) -> Frame {
        Frame {
            x,
            y,
            width: w,
            height: h,
        }
    }

    #[test]
    fn parses_multiple_chrome_windows_and_active_tabs() {
        let json = r#"[
            {"title":"Docs","tabs":[{"title":"Rust","url":"https://www.rust-lang.org/","active":true}]},
            {"title":"Search","tabs":[{"title":"One","url":"https://example.com/1","active":false},{"title":"Two","url":"https://example.com/2","active":true}]}
        ]"#;

        let windows = parse_chrome_windows_json(json).unwrap();

        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].tabs[0].url, "https://www.rust-lang.org/");
        assert!(windows[0].tabs[0].active);
        assert_eq!(windows[1].tabs.len(), 2);
        assert!(windows[1].tabs[1].active);
    }

    #[test]
    fn restore_spec_encodes_multiple_windows_urls_and_active_tab() {
        let first = chrome_window(vec![BrowserTab {
            title: Some("Rust".to_string()),
            url: "https://www.rust-lang.org/".to_string(),
            active: true,
        }]);
        let second = chrome_window(vec![
            BrowserTab {
                title: Some("One".to_string()),
                url: "https://example.com/1".to_string(),
                active: false,
            },
            BrowserTab {
                title: Some("Two".to_string()),
                url: "https://example.com/2".to_string(),
                active: true,
            },
        ]);

        let spec = restore_spec_json(&[
            (&first, frame(0.0, 0.0, 900.0, 700.0)),
            (&second, frame(100.0, 50.0, 800.0, 600.0)),
        ]);
        let parsed: serde_json::Value = serde_json::from_str(&spec).unwrap();

        assert_eq!(parsed.as_array().unwrap().len(), 2);
        assert_eq!(parsed[0]["urls"][0], "https://www.rust-lang.org/");
        assert_eq!(parsed[0]["active"], 1);
        assert_eq!(parsed[0]["width"], 900);
        assert_eq!(parsed[0]["height"], 700);
        assert_eq!(parsed[1]["urls"].as_array().unwrap().len(), 2);
        assert_eq!(parsed[1]["active"], 2);
        assert_eq!(parsed[1]["x"], 100);
        assert_eq!(parsed[1]["y"], 50);
    }

    #[test]
    fn restore_spec_handles_windows_without_tab_metadata_for_old_snapshots() {
        let first = chrome_window(Vec::new());
        let second = chrome_window(Vec::new());

        let spec = restore_spec_json(&[
            (&first, frame(0.0, 0.0, 100.0, 100.0)),
            (&second, frame(10.0, 20.0, 300.0, 400.0)),
        ]);
        let parsed: serde_json::Value = serde_json::from_str(&spec).unwrap();

        assert_eq!(parsed.as_array().unwrap().len(), 2);
        assert!(parsed[0]["urls"].as_array().unwrap().is_empty());
        // Bounds are still present even when no tabs were captured.
        assert_eq!(parsed[1]["x"], 10);
        assert_eq!(parsed[1]["height"], 400);
    }

    #[test]
    fn restore_spec_rounds_fractional_frames_to_integers() {
        let win = chrome_window(Vec::new());
        let spec = restore_spec_json(&[(&win, frame(12.7, 8.4, 100.6, 50.5))]);
        let parsed: serde_json::Value = serde_json::from_str(&spec).unwrap();

        assert_eq!(parsed[0]["x"], 13);
        assert_eq!(parsed[0]["y"], 8);
        assert_eq!(parsed[0]["width"], 101);
        assert_eq!(parsed[0]["height"], 51);
    }
}
