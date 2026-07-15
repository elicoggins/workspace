use crate::{
    error::{Result, WorkspaceError},
    model::{DisplaySnapshot, Frame},
};

#[cfg(target_os = "macos")]
mod imp {
    use core_graphics::geometry::CGRect;
    use libc::c_uint;

    use super::*;

    type CGDirectDisplayID = c_uint;
    type CGError = i32;

    const MAX_DISPLAYS: usize = 32;

    type CGDisplayModeRef = *const std::ffi::c_void;

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn CGGetActiveDisplayList(
            max_displays: u32,
            active_displays: *mut CGDirectDisplayID,
            display_count: *mut u32,
        ) -> CGError;
        fn CGMainDisplayID() -> CGDirectDisplayID;
        fn CGDisplayBounds(display: CGDirectDisplayID) -> CGRect;
        fn CGDisplayCopyDisplayMode(display: CGDirectDisplayID) -> CGDisplayModeRef;
        fn CGDisplayModeGetPixelWidth(mode: CGDisplayModeRef) -> usize;
        fn CGDisplayModeRelease(mode: CGDisplayModeRef);
    }

    /// Backing-pixel width ÷ point width. (`CGDisplayPixelsWide` returns
    /// points on modern macOS, which made every display report ~1.0.)
    fn display_scale_factor(id: CGDirectDisplayID, point_width: f64) -> f64 {
        if point_width <= 0.0 {
            return 1.0;
        }
        let mode = unsafe { CGDisplayCopyDisplayMode(id) };
        if mode.is_null() {
            return 1.0;
        }
        let pixel_width = unsafe { CGDisplayModeGetPixelWidth(mode) } as f64;
        unsafe { CGDisplayModeRelease(mode) };
        if pixel_width <= 0.0 {
            1.0
        } else {
            pixel_width / point_width
        }
    }

    pub fn current_displays() -> Result<Vec<DisplaySnapshot>> {
        let mut ids = [0_u32; MAX_DISPLAYS];
        let mut count = 0_u32;
        let error =
            unsafe { CGGetActiveDisplayList(MAX_DISPLAYS as u32, ids.as_mut_ptr(), &mut count) };
        if error != 0 {
            return Err(WorkspaceError::MacOs(format!(
                "CGGetActiveDisplayList returned {error}"
            )));
        }

        let primary = unsafe { CGMainDisplayID() };
        let mut displays = Vec::with_capacity(count as usize);

        for id in ids.into_iter().take(count as usize) {
            let bounds = unsafe { CGDisplayBounds(id) };
            let frame = Frame {
                x: bounds.origin.x,
                y: bounds.origin.y,
                width: bounds.size.width,
                height: bounds.size.height,
            };

            displays.push(DisplaySnapshot {
                id: format!("cgdisplay-{id}"),
                numeric_id: id,
                name: None,
                frame,
                scale_factor: display_scale_factor(id, frame.width),
                is_primary: id == primary,
            });
        }

        displays.sort_by_key(|display| (!display.is_primary, display.numeric_id));
        Ok(displays)
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::*;

    pub fn current_displays() -> Result<Vec<DisplaySnapshot>> {
        Err(WorkspaceError::UnsupportedPlatform)
    }
}

pub use imp::current_displays;
