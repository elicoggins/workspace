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
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum SupportLevel {
    FullRestore,
    Unsupported,
}

pub const KNOWN_APPS: &[KnownApp] = &[
    KnownApp {
        bundle_id: "com.microsoft.VSCode",
        name: "Visual Studio Code",
        support: AppSupport {
            level: SupportLevel::FullRestore,
            reason: "VS Code window geometry and z-order restore are currently supported",
        },
    },
    KnownApp {
        bundle_id: "com.google.Chrome",
        name: "Google Chrome",
        support: AppSupport {
            level: SupportLevel::FullRestore,
            reason: "Chrome window geometry and z-order restore are enabled",
        },
    },
    KnownApp {
        bundle_id: "com.apple.Safari",
        name: "Safari",
        support: AppSupport {
            level: SupportLevel::FullRestore,
            reason: "Safari window geometry and z-order restore are enabled",
        },
    },
    KnownApp {
        bundle_id: "com.apple.Terminal",
        name: "Terminal",
        support: AppSupport {
            level: SupportLevel::FullRestore,
            reason: "Terminal window geometry and z-order restore are enabled",
        },
    },
    KnownApp {
        bundle_id: "com.googlecode.iterm2",
        name: "iTerm2",
        support: AppSupport {
            level: SupportLevel::FullRestore,
            reason: "iTerm2 window geometry and z-order restore are enabled",
        },
    },
    KnownApp {
        bundle_id: "dev.warp.Warp-Stable",
        name: "Warp",
        support: AppSupport {
            level: SupportLevel::FullRestore,
            reason: "Warp window geometry and z-order restore are enabled",
        },
    },
    KnownApp {
        bundle_id: "com.todesktop.230313mzl4w4u92",
        name: "Cursor",
        support: AppSupport {
            level: SupportLevel::FullRestore,
            reason: "Cursor window geometry and z-order restore are enabled",
        },
    },
    KnownApp {
        bundle_id: "com.apple.dt.Xcode",
        name: "Xcode",
        support: AppSupport {
            level: SupportLevel::FullRestore,
            reason: "Xcode window geometry and z-order restore are enabled",
        },
    },
    KnownApp {
        bundle_id: "com.apple.finder",
        name: "Finder",
        support: AppSupport {
            level: SupportLevel::FullRestore,
            reason: "Finder window geometry and z-order restore are enabled",
        },
    },
    KnownApp {
        bundle_id: "com.apple.Notes",
        name: "Notes",
        support: AppSupport {
            level: SupportLevel::FullRestore,
            reason: "Notes window geometry and z-order restore are enabled",
        },
    },
    KnownApp {
        bundle_id: "com.apple.Music",
        name: "Music",
        support: AppSupport {
            level: SupportLevel::FullRestore,
            reason: "Music window geometry and z-order restore are enabled",
        },
    },
    KnownApp {
        bundle_id: "com.apple.MobileSMS",
        name: "Messages",
        support: AppSupport {
            level: SupportLevel::FullRestore,
            reason: "Messages window geometry and z-order restore are enabled",
        },
    },
];

const UNKNOWN_BUNDLE_SUPPORT: AppSupport = AppSupport {
    level: SupportLevel::Unsupported,
    reason: "this app is not in the supported restore allowlist yet",
};

const MISSING_BUNDLE_SUPPORT: AppSupport = AppSupport {
    level: SupportLevel::Unsupported,
    reason: "windows without bundle identifiers are not restored yet",
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
