use crate::{
    error::{Result, WorkspaceError},
    model::{BrowserTab, Frame, WindowSnapshot},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChromeWindowTabs {
    pub title: Option<String>,
    pub tabs: Vec<BrowserTab>,
}

#[derive(Debug, serde::Deserialize)]
pub struct RestoredBrowserWindow {
    pub id: i64,
    pub title: Option<String>,
    pub frame: Frame,
}

#[derive(Debug, serde::Deserialize)]
pub struct BrowserRestoreResult {
    pub windows: Vec<Option<RestoredBrowserWindow>>,
    pub errors: Vec<String>,
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

    /// Create a fresh window per entry. Existing windows, including blank
    /// windows already matched by the planner, are never adopted.
    const RESTORE_SCRIPT: &str = r#"
function run(argv) {
  const chrome = Application(argv[0]);
  const specs = JSON.parse(argv[1]);
  chrome.activate();
  const restored = specs.map(function () { return null; });
  const errors = [];
  const windowIds = function () {
    return chrome.windows().map(function (w) {
      return w.id();
    });
  };
  // New windows can appear before their first tab; indexing early throws "Wrong index".
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
      const before = windowIds();
      // A creation response can fail after the window materializes. Resolve
      // exactly one new ID; concurrent new windows make ownership ambiguous.
      try { chrome.windows.push(chrome.Window()); } catch (e) {}
      for (let attempt = 0; attempt < 40 && !target; attempt++) {
        const added = windowIds().filter(function (id) { return before.indexOf(id) === -1; });
        if (added.length > 1) throw new Error('multiple new windows; ownership is ambiguous');
        if (added.length === 1) target = chrome.windows.byId(added[0]);
        if (!target) delay(0.05);
      }
      if (!target) throw new Error('created a window but could not find it');
      if (!waitForFirstTab(target)) throw new Error('window has no tabs after waiting');
      const initial = target.tabs().map(function (tab) { return tab.url() || ''; });
      if (initial.length !== 1 || !(initial[0] === '' || initial[0] === 'about:blank' ||
          /^[a-z-]+:\/\/newtab/.test(initial[0]))) throw new Error('new window was not blank');
      if (spec.urls.length > 0) {
        target.tabs[0].url = spec.urls[0];
        for (let i = 1; i < spec.urls.length; i++) {
          target.tabs.push(chrome.Tab({ url: spec.urls[i] }));
        }
        target.activeTabIndex = spec.active;
        const actual = target.tabs().map(function (tab) { return tab.url() || ''; });
        if (actual.length !== spec.urls.length || actual.some(function (url, i) { return url !== spec.urls[i]; }))
          throw new Error('restored tabs could not be verified');
        if (target.activeTabIndex() !== spec.active) throw new Error('active tab could not be verified');
      }
      target.bounds = { x: spec.x, y: spec.y, width: spec.width, height: spec.height };
      const frame = target.bounds();
      if (!(Math.abs(frame.x - spec.x) <= 2 && Math.abs(frame.y - spec.y) <= 2 &&
            Math.abs(frame.width - spec.width) <= 2 && Math.abs(frame.height - spec.height) <= 2))
        throw new Error('restored bounds could not be verified');
      restored[specIndex] = { id: target.id(), title: target.name(), frame: frame };
    } catch (e) {
      errors.push('window ' + specIndex + ': ' + e);
    }
  });
  return JSON.stringify({ windows: restored, errors: errors });
}
"#;

    /// Reopen missing URLs in the selected window, preserving existing tabs.
    const RECONCILE_SCRIPT: &str = r#"
function run(argv) {
  const chrome = Application(argv[0]);
  const spec = JSON.parse(argv[1]);
  if (!chrome.running()) return 'error: browser not running';
  if (!Number.isSafeInteger(spec.windowId)) return 'error: missing window identity';
  try {
    const target = chrome.windows.byId(spec.windowId);
    if (target.id() !== spec.windowId) return 'error: window identity changed';
    const existing = target.tabs().map(function (t) { return t.url() || ''; });
    // Reject unrelated content: the initial saved-to-live match is heuristic.
    const isBlankWindow = existing.length === 1 &&
      (existing[0] === '' || existing[0] === 'about:blank' || /^[a-z-]+:\/\/newtab/.test(existing[0]));
    const overlap = existing.filter(function (u) { return spec.urls.indexOf(u) !== -1; }).length;
    if (!isBlankWindow && overlap === 0) return 'error: matched window shares no saved tabs; not reconciling';
    let added = 0;
    for (let i = 0; i < spec.urls.length; i++) {
      if (existing.indexOf(spec.urls[i]) === -1) {
        target.tabs.push(chrome.Tab({ url: spec.urls[i] }));
        existing.push(spec.urls[i]);
        added++;
      }
    }
    const now = target.tabs().map(function (t) { return t.url() || ''; });
    if (spec.urls.some(function (url) { return now.indexOf(url) === -1; }))
      return 'error: restored tabs could not be verified';
    if (spec.activeUrl) {
      const idx = now.indexOf(spec.activeUrl);
      if (idx === -1) return 'error: saved active tab is missing';
      target.activeTabIndex = idx + 1;
      if (target.activeTabIndex() !== idx + 1) return 'error: active tab could not be verified';
    }
    return 'added ' + added;
  } catch (e) { return 'error: ' + e; }
}
"#;

    /// Bridge one live observation to a scripting ID before any mutations.
    /// Titles are optional; bounds must agree and the match must be unique.
    const RESOLVE_WINDOW_SCRIPT: &str = r#"
function run(argv) {
  const chrome = Application(argv[0]);
  const payload = JSON.parse(argv[1]);
  const specs = Array.isArray(payload) ? payload : [payload];
  const windows = chrome.running() ? chrome.windows().map(function (w) {
    return { id: w.id(), title: w.name(), frame: w.bounds() };
  }) : [];
  const ids = specs.map(function (spec) {
    const matches = windows.filter(function (w) {
      const b = w.frame;
      return spec.width > 0 && spec.height > 0 &&
             Math.abs(b.x - spec.x) <= 2 && Math.abs(b.y - spec.y) <= 2 &&
             Math.abs(b.width - spec.width) <= 2 && Math.abs(b.height - spec.height) <= 2 &&
             (spec.title === null || w.title === spec.title);
    });
    return matches.length === 1 ? matches[0].id : null;
  });
  const unique = ids.map(function (id) {
    return id !== null && ids.filter(function (other) { return other === id; }).length === 1 ? id : null;
  });
  return JSON.stringify(Array.isArray(payload) ? unique : unique[0]);
}
"#;

    /// Set bounds by the resolved scripting ID. Chromium sometimes rejects
    /// AXSize writes with -25200, so prefer its scripting dictionary.
    const SET_BOUNDS_SCRIPT: &str = r#"
function run(argv) {
  const chrome = Application(argv[0]);
  const spec = JSON.parse(argv[1]);
  if (!chrome.running()) return 'error: browser not running';
  if (!Number.isSafeInteger(spec.windowId)) return 'error: missing window identity';
  try {
    const target = chrome.windows.byId(spec.windowId);
    if (target.id() !== spec.windowId) return 'error: window identity changed';
    target.bounds = { x: spec.tx, y: spec.ty, width: spec.tw, height: spec.th };
    const bounds = target.bounds();
    if (!(Math.abs(bounds.x - spec.tx) <= 2 && Math.abs(bounds.y - spec.ty) <= 2 &&
          Math.abs(bounds.width - spec.tw) <= 2 && Math.abs(bounds.height - spec.th) <= 2))
      return 'error: bounds could not be verified';
    return 'ok';
  } catch (e) { return 'error: ' + e; }
}
"#;

    /// Returns `Ok(true)` only after the selected window's bounds are observed.
    pub fn set_window_bounds(bundle_id: &str, window_id: i64, to: Frame) -> Result<bool> {
        let spec = serde_json::json!({
            "windowId": window_id,
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

    /// Number of reopened tabs, or None when reconciliation could not be confirmed.
    pub fn reconcile_window_tabs(
        bundle_id: &str,
        window_id: i64,
        saved: &WindowSnapshot,
    ) -> Result<Option<usize>> {
        if saved.browser_tabs.is_empty() {
            return Ok(Some(0));
        }
        let spec = serde_json::json!({
            "windowId": window_id,
            "urls": saved.browser_tabs.iter().map(|t| t.url.as_str()).collect::<Vec<_>>(),
            "activeUrl": saved.browser_tabs.iter().find(|t| t.active).map(|t| t.url.as_str()),
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

    pub fn resolve_window_ids(
        bundle_id: &str,
        observed: &[WindowSnapshot],
    ) -> Result<Vec<Option<i64>>> {
        let specs: Vec<_> = observed
            .iter()
            .map(|window| {
                serde_json::json!({
                    "title": window.title,
                    "x": window.frame.x, "y": window.frame.y,
                    "width": window.frame.width, "height": window.frame.height,
                })
            })
            .collect();
        let spec = serde_json::to_string(&specs)?;
        let output = Command::new("/usr/bin/osascript")
            .args([
                "-l",
                "JavaScript",
                "-e",
                RESOLVE_WINDOW_SCRIPT,
                bundle_id,
                &spec,
            ])
            .output()
            .map_err(|e| WorkspaceError::MacOs(format!("browser identity lookup failed: {e}")))?;
        if !output.status.success() {
            return Err(WorkspaceError::MacOs(format!(
                "browser identity lookup failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        let ids: Vec<Option<i64>> = serde_json::from_slice(&output.stdout).map_err(|e| {
            WorkspaceError::MacOs(format!("invalid browser identity response: {e}"))
        })?;
        if ids.len() != observed.len() {
            return Err(WorkspaceError::MacOs(
                "browser identity response has an incorrect window count".into(),
            ));
        }
        Ok(ids)
    }

    pub fn restore_windows(
        bundle_id: &str,
        windows: &[(&WindowSnapshot, Frame)],
    ) -> Result<BrowserRestoreResult> {
        if windows.is_empty() {
            return Ok(BrowserRestoreResult {
                windows: Vec::new(),
                errors: Vec::new(),
            });
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

        let result: BrowserRestoreResult = serde_json::from_slice(&output.stdout)
            .map_err(|e| WorkspaceError::MacOs(format!("invalid browser restore response: {e}")))?;
        if result.windows.len() != windows.len() {
            return Err(WorkspaceError::MacOs(
                "browser restore response has an incorrect window count".into(),
            ));
        }
        Ok(result)
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

    /// Pass data as JSON arguments so URLs and titles never become script source.
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
    ) -> Result<BrowserRestoreResult> {
        Err(WorkspaceError::UnsupportedPlatform)
    }

    pub fn reconcile_window_tabs(
        _bundle_id: &str,
        _window_id: i64,
        _saved: &WindowSnapshot,
    ) -> Result<Option<usize>> {
        Err(WorkspaceError::UnsupportedPlatform)
    }

    pub fn set_window_bounds(_bundle_id: &str, _window_id: i64, _to: Frame) -> Result<bool> {
        Err(WorkspaceError::UnsupportedPlatform)
    }

    pub fn resolve_window_ids(
        _bundle_id: &str,
        _observed: &[WindowSnapshot],
    ) -> Result<Vec<Option<i64>>> {
        Err(WorkspaceError::UnsupportedPlatform)
    }
}

pub use imp::{
    capture_windows, reconcile_window_tabs, resolve_window_ids, restore_windows, set_window_bounds,
};

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
