use crate::{macos::window::RawWindow, model::Frame};

const MIN_WINDOW_WIDTH: f64 = 80.0;
const MIN_WINDOW_HEIGHT: f64 = 60.0;

const SKIPPED_OWNERS: &[&str] = &[
    "Dock",
    "Window Server",
    "WindowServer",
    "SystemUIServer",
    "Notification Center",
    "Control Center",
    "Spotlight",
    "loginwindow",
    "TextInputMenuAgent",
    "Universal Control",
];

pub fn should_capture_window(window: &RawWindow) -> bool {
    window.is_onscreen
        && window.layer == 0
        && window.alpha > 0.0
        && is_reasonable_size(window.frame)
        && !SKIPPED_OWNERS
            .iter()
            .any(|owner| owner.eq_ignore_ascii_case(&window.owner_name))
}

fn is_reasonable_size(frame: Frame) -> bool {
    frame.width >= MIN_WINDOW_WIDTH && frame.height >= MIN_WINDOW_HEIGHT
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(owner_name: &str, frame: Frame) -> RawWindow {
        RawWindow {
            window_id: 1,
            owner_pid: 10,
            owner_name: owner_name.to_string(),
            window_title: Some("main".to_string()),
            frame,
            layer: 0,
            alpha: 1.0,
            is_onscreen: true,
            z_order: 0,
        }
    }

    #[test]
    fn skips_tiny_windows() {
        assert!(!should_capture_window(&raw(
            "Code",
            Frame {
                x: 0.0,
                y: 0.0,
                width: 20.0,
                height: 20.0
            }
        )));
    }

    #[test]
    fn skips_system_utility_windows() {
        assert!(!should_capture_window(&raw(
            "Dock",
            Frame {
                x: 0.0,
                y: 0.0,
                width: 500.0,
                height: 500.0
            }
        )));
    }
}
