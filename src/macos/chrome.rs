use crate::{
    error::{Result, WorkspaceError},
    model::{BrowserTab, WindowSnapshot},
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

    pub fn restore_windows(windows: &[&WindowSnapshot]) -> Result<bool> {
        let script = restore_script_for_windows(windows);
        let status = Command::new("/usr/bin/osascript")
            .arg("-e")
            .arg(script)
            .stdout(Stdio::null())
            .status()
            .map_err(|source| {
                WorkspaceError::MacOs(format!("failed to run Chrome restore script: {source}"))
            })?;

        if status.success() {
            Ok(true)
        } else {
            Err(WorkspaceError::MacOs(format!(
                "Chrome restore script exited with {status}"
            )))
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

    pub fn restore_script_for_windows(windows: &[&WindowSnapshot]) -> String {
        let mut script = String::from("tell application \"Google Chrome\"\n\tactivate\n");
        script.push_str(&format!(
            "\trepeat while (count of windows) < {}\n\t\tmake new window\n\tend repeat\n",
            windows.len()
        ));

        for (window_index, window) in windows.iter().enumerate() {
            let applescript_window_index = window_index + 1;
            script.push_str("\ttry\n");
            script.push_str(&format!(
                "\t\tset targetWindow to window {}\n",
                applescript_window_index
            ));

            if !window.browser_tabs.is_empty() {
                script.push_str("\t\trepeat while (count of tabs of targetWindow) > 1\n\t\t\tclose tab -1 of targetWindow\n\t\tend repeat\n");

                if let Some(first_tab) = window.browser_tabs.first() {
                    script.push_str(&format!(
                        "\t\tset URL of active tab of targetWindow to {}\n",
                        applescript_string(&first_tab.url)
                    ));
                }

                for tab in window.browser_tabs.iter().skip(1) {
                    script.push_str(&format!(
                        "\t\tmake new tab at end of tabs of targetWindow with properties {{URL:{}}}\n",
                        applescript_string(&tab.url)
                    ));
                }

                let active_index = window
                    .browser_tabs
                    .iter()
                    .position(|tab| tab.active)
                    .map(|index| index + 1)
                    .unwrap_or(1);
                script.push_str("\t\ttry\n");
                script.push_str(&format!(
                    "\t\t\tset active tab index of targetWindow to {}\n",
                    active_index
                ));
                script.push_str("\t\tend try\n");
            }

            script.push_str("\tend try\n");
        }

        script.push_str("\treturn \"\"\nend tell\n");
        script
    }

    fn applescript_string(value: &str) -> String {
        let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
        format!("\"{escaped}\"")
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::*;

    pub fn capture_windows() -> Vec<ChromeWindowTabs> {
        Vec::new()
    }

    pub fn restore_windows(_windows: &[&WindowSnapshot]) -> Result<bool> {
        Err(WorkspaceError::UnsupportedPlatform)
    }
}

pub use imp::{capture_windows, restore_windows};

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::imp::{parse_chrome_windows_json, restore_script_for_windows};
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
    fn restore_script_recreates_multiple_windows_and_urls() {
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

        let script = restore_script_for_windows(&[&first, &second]);

        assert!(script.contains("repeat while (count of windows) < 2"));
        assert!(script.contains("https://www.rust-lang.org/"));
        assert!(script.contains("https://example.com/1"));
        assert!(script.contains("https://example.com/2"));
        assert!(script.contains("set active tab index of targetWindow to 2"));
        assert!(script.contains("try"));
        assert!(script.contains("return \"\""));
    }

    #[test]
    fn restore_script_creates_windows_without_tab_metadata_for_old_snapshots() {
        let first = chrome_window(Vec::new());
        let second = chrome_window(Vec::new());
        let third = chrome_window(Vec::new());

        let script = restore_script_for_windows(&[&first, &second, &third]);

        assert!(script.contains("repeat while (count of windows) < 3"));
        assert!(!script.contains("set URL of active tab"));
    }
}
