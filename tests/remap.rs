use workspace::{
    model::{DisplaySnapshot, Frame, RelativeFrame, WindowSnapshot},
    world::target_frame_for_window,
};

fn display(id: &str, numeric_id: u32, frame: Frame, primary: bool) -> DisplaySnapshot {
    DisplaySnapshot {
        id: id.to_string(),
        numeric_id,
        name: None,
        frame,
        scale_factor: 1.0,
        is_primary: primary,
    }
}

fn window(frame: Frame, display: &DisplaySnapshot) -> WindowSnapshot {
    WindowSnapshot {
        window_id: 9,
        app_name: "Terminal".to_string(),
        process_name: "Terminal".to_string(),
        bundle_id: Some("com.apple.Terminal".to_string()),
        pid: 42,
        title: Some("shell".to_string()),
        frame,
        display_id: Some(display.id.clone()),
        display_frame: Some(display.frame),
        display_relative_frame: Some(RelativeFrame {
            x: 0.1,
            y: 0.2,
            width: 0.4,
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
fn unchanged_monitor_keeps_exact_pixels() {
    let saved_display = display(
        "cgdisplay-1",
        1,
        Frame {
            x: 0.0,
            y: 0.0,
            width: 2560.0,
            height: 1440.0,
        },
        true,
    );
    let saved_window = window(
        Frame {
            x: 0.0,
            y: 0.0,
            width: 1280.0,
            height: 900.0,
        },
        &saved_display,
    );

    let target = target_frame_for_window(
        &saved_window,
        std::slice::from_ref(&saved_display),
        std::slice::from_ref(&saved_display),
    );

    assert_eq!(target, saved_window.frame);
}

#[test]
fn removed_monitor_uses_relative_geometry_on_primary() {
    let saved_display = display(
        "external",
        2,
        Frame {
            x: 2560.0,
            y: 0.0,
            width: 1920.0,
            height: 1080.0,
        },
        false,
    );
    let current_display = display(
        "built-in",
        1,
        Frame {
            x: 0.0,
            y: 0.0,
            width: 1440.0,
            height: 900.0,
        },
        true,
    );
    let saved_window = window(
        Frame {
            x: 2752.0,
            y: 216.0,
            width: 768.0,
            height: 540.0,
        },
        &saved_display,
    );

    let target = target_frame_for_window(&saved_window, &[saved_display], &[current_display]);

    assert_eq!(target.width, 576.0);
    assert_eq!(target.height, 450.0);
    assert!(target.x >= 12.0);
    assert!(target.y >= 12.0);
}
