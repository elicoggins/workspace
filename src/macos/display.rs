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

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn CGGetActiveDisplayList(
            max_displays: u32,
            active_displays: *mut CGDirectDisplayID,
            display_count: *mut u32,
        ) -> CGError;
        fn CGMainDisplayID() -> CGDirectDisplayID;
        fn CGDisplayBounds(display: CGDirectDisplayID) -> CGRect;
        fn CGDisplayPixelsWide(display: CGDirectDisplayID) -> usize;
        fn CGDisplayPixelsHigh(display: CGDirectDisplayID) -> usize;
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
            let pixels_wide = unsafe { CGDisplayPixelsWide(id) } as f64;
            let pixels_high = unsafe { CGDisplayPixelsHigh(id) } as f64;
            let frame = Frame {
                x: bounds.origin.x,
                y: bounds.origin.y,
                width: bounds.size.width,
                height: bounds.size.height,
            };
            let scale_factor = if frame.width > 0.0 {
                pixels_wide / frame.width
            } else {
                1.0
            };

            displays.push(DisplaySnapshot {
                id: format!("cgdisplay-{id}"),
                numeric_id: id,
                name: None,
                frame,
                scale_factor: scale_factor.max(pixels_high / frame.height.max(1.0)),
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
