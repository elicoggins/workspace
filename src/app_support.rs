use crate::model::WindowSnapshot;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct AppSupport {
    pub level: SupportLevel,
    pub reason: &'static str,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct KnownApp {
    pub bundle_id: &'static str,
    pub name: &'static str,
    pub support: AppSupport,
    /// Uses Chromium's scripting dictionary for tab capture and restore.
    pub tab_capture: bool,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum SupportLevel {
    FullRestore,
    Unsupported,
}

const WINDOW_RESTORE: AppSupport = AppSupport {
    level: SupportLevel::FullRestore,
    reason: "window geometry and z-order restore enabled",
};

const WINDOW_AND_TAB_RESTORE: AppSupport = AppSupport {
    level: SupportLevel::FullRestore,
    reason: "window geometry and tab restore enabled",
};

pub const KNOWN_APPS: &[KnownApp] = &[
    KnownApp {
        bundle_id: "com.microsoft.VSCode",
        name: "Visual Studio Code",
        support: WINDOW_RESTORE,
        tab_capture: false,
    },
    KnownApp {
        bundle_id: "com.google.Chrome",
        name: "Google Chrome",
        support: WINDOW_AND_TAB_RESTORE,
        tab_capture: true,
    },
    KnownApp {
        bundle_id: "com.apple.Safari",
        name: "Safari",
        support: WINDOW_RESTORE,
        tab_capture: false,
    },
    KnownApp {
        bundle_id: "com.apple.Terminal",
        name: "Terminal",
        support: WINDOW_RESTORE,
        tab_capture: false,
    },
    KnownApp {
        bundle_id: "com.googlecode.iterm2",
        name: "iTerm2",
        support: WINDOW_RESTORE,
        tab_capture: false,
    },
    KnownApp {
        bundle_id: "dev.warp.Warp-Stable",
        name: "Warp",
        support: WINDOW_RESTORE,
        tab_capture: false,
    },
    KnownApp {
        bundle_id: "com.todesktop.230313mzl4w4u92",
        name: "Cursor",
        support: WINDOW_RESTORE,
        tab_capture: false,
    },
    KnownApp {
        bundle_id: "com.apple.dt.Xcode",
        name: "Xcode",
        support: WINDOW_RESTORE,
        tab_capture: false,
    },
    KnownApp {
        bundle_id: "com.apple.finder",
        name: "Finder",
        support: WINDOW_RESTORE,
        tab_capture: false,
    },
    KnownApp {
        bundle_id: "com.apple.Notes",
        name: "Notes",
        support: WINDOW_RESTORE,
        tab_capture: false,
    },
    KnownApp {
        bundle_id: "com.apple.Music",
        name: "Music",
        support: WINDOW_RESTORE,
        tab_capture: false,
    },
    KnownApp {
        bundle_id: "com.apple.MobileSMS",
        name: "Messages",
        support: WINDOW_RESTORE,
        tab_capture: false,
    },
    KnownApp {
        bundle_id: "com.google.Chrome.canary",
        name: "Google Chrome Canary",
        support: WINDOW_AND_TAB_RESTORE,
        tab_capture: true,
    },
    KnownApp {
        bundle_id: "com.brave.Browser",
        name: "Brave Browser",
        support: WINDOW_AND_TAB_RESTORE,
        tab_capture: true,
    },
    KnownApp {
        bundle_id: "com.microsoft.edgemac",
        name: "Microsoft Edge",
        support: WINDOW_AND_TAB_RESTORE,
        tab_capture: true,
    },
    KnownApp {
        bundle_id: "org.chromium.Chromium",
        name: "Chromium",
        support: WINDOW_AND_TAB_RESTORE,
        tab_capture: true,
    },
];

const UNKNOWN_BUNDLE_SUPPORT: AppSupport = AppSupport {
    level: SupportLevel::Unsupported,
    reason: "app is not supported for restore",
};

const MISSING_BUNDLE_SUPPORT: AppSupport = AppSupport {
    level: SupportLevel::Unsupported,
    reason: "missing app bundle identifier",
};

pub fn support_for_window(window: &WindowSnapshot) -> AppSupport {
    support_for_bundle_id(window.bundle_id.as_deref())
}

pub fn support_for_bundle_id(bundle_id: Option<&str>) -> AppSupport {
    match bundle_id {
        Some(bundle_id) => KNOWN_APPS
            .iter()
            .find(|app| app.bundle_id == bundle_id)
            .map(|app| app.support)
            .unwrap_or(UNKNOWN_BUNDLE_SUPPORT),
        None => MISSING_BUNDLE_SUPPORT,
    }
}

pub fn full_restore_apps() -> impl Iterator<Item = &'static KnownApp> {
    KNOWN_APPS
        .iter()
        .filter(|app| app.support.level == SupportLevel::FullRestore)
}

/// Browsers whose tabs are captured and restored.
pub fn tab_capable_apps() -> impl Iterator<Item = &'static KnownApp> {
    KNOWN_APPS.iter().filter(|app| app.tab_capture)
}

pub fn is_tab_capable(bundle_id: Option<&str>) -> bool {
    bundle_id
        .map(|bundle_id| {
            KNOWN_APPS
                .iter()
                .any(|app| app.bundle_id == bundle_id && app.tab_capture)
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Frame, WindowSnapshot};

    fn window(bundle_id: Option<&str>) -> WindowSnapshot {
        WindowSnapshot {
            window_id: 1,
            app_name: "App".to_string(),
            process_name: "App".to_string(),
            bundle_id: bundle_id.map(str::to_string),
            pid: 42,
            title: None,
            frame: Frame {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 100.0,
            },
            display_id: None,
            display_frame: None,
            display_relative_frame: None,
            z_order: None,
            fullscreen: false,
            minimized: false,
            enabled: true,
            browser_tabs: Vec::new(),
        }
    }

    #[test]
    fn all_known_apps_are_supported() {
        for app in KNOWN_APPS {
            assert_eq!(
                support_for_window(&window(Some(app.bundle_id))).level,
                SupportLevel::FullRestore,
                "{} should be supported",
                app.bundle_id
            );
        }
        assert_eq!(
            support_for_window(&window(None)).level,
            SupportLevel::Unsupported
        );
    }

    #[test]
    fn common_apps_are_explicitly_classified() {
        for bundle_id in [
            "com.google.Chrome",
            "com.apple.Safari",
            "com.apple.Terminal",
            "com.googlecode.iterm2",
            "dev.warp.Warp-Stable",
            "com.todesktop.230313mzl4w4u92",
            "com.apple.dt.Xcode",
            "com.apple.finder",
            "com.apple.Notes",
            "com.apple.Music",
            "com.apple.MobileSMS",
        ] {
            assert!(
                KNOWN_APPS.iter().any(|app| app.bundle_id == bundle_id),
                "{bundle_id} should be explicitly tracked"
            );
        }
    }
}
