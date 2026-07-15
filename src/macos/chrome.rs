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
const chrome = Application('Google Chrome');
if (!chrome.running()) {
  JSON.stringify([]);
} else {
  JSON.stringify(chrome.windows().map((window) => {
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

    pub fn capture_windows() -> Vec<ChromeWindowTabs> {
        let output = Command::new("/usr/bin/osascript")
            .arg("-l")
            .arg("JavaScript")
            .arg("-e")
            .arg(CAPTURE_SCRIPT)
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
  const specs = JSON.parse(argv[0]);
  const chrome = Application('Google Chrome');
  chrome.activate();
  const used = [];
  const errors = [];
  const isBlank = function (w) {
    try {
      if (w.tabs().length !== 1) return false;
      const url = w.activeTab.url() || '';
      return url === '' || url.indexOf('chrome://newtab') === 0;
    } catch (e) {
      return false;
    }
  };
  const windowIds = function () {
    return chrome.windows().map(function (w) {
      try { return w.id(); } catch (e) { return null; }
    });
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
        chrome.windows.push(chrome.Window());
        const after = chrome.windows();
        for (let i = 0; i < after.length; i++) {
          let id;
          try { id = after[i].id(); } catch (e) { continue; }
          if (before.indexOf(id) === -1) { target = after[i]; used.push(id); break; }
        }
        if (!target) throw new Error('created a window but could not find it');
      }
      if (spec.urls.length > 0) {
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

    pub fn restore_windows(windows: &[(&WindowSnapshot, Frame)]) -> Result<bool> {
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

    pub fn capture_windows() -> Vec<ChromeWindowTabs> {
        Vec::new()
    }

    pub fn restore_windows(_windows: &[(&WindowSnapshot, Frame)]) -> Result<bool> {
        Err(WorkspaceError::UnsupportedPlatform)
    }
}

pub use imp::{capture_windows, restore_windows};

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
