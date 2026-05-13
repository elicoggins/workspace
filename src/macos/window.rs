use crate::{
    error::{Result, WorkspaceError},
    model::Frame,
};

#[derive(Debug, Clone)]
pub struct RawWindow {
    pub window_id: u32,
    pub owner_pid: i32,
    pub owner_name: String,
    pub window_title: Option<String>,
    pub frame: Frame,
    pub layer: i32,
    pub alpha: f64,
    pub is_onscreen: bool,
    pub z_order: u32,
}

#[cfg(target_os = "macos")]
mod imp {
    use core_foundation::base::{CFRelease, CFTypeRef};
    use libc::c_void;
    use objc::{msg_send, runtime::Object, sel, sel_impl};

    use super::*;
    use crate::macos::util::objc_util::{ns_string, nsstring_to_string, AutoreleasePool};

    type CFArrayRef = *const c_void;

    const K_CG_WINDOW_LIST_OPTION_ON_SCREEN_ONLY: u32 = 1;
    const K_CG_NULL_WINDOW_ID: u32 = 0;

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn CGWindowListCopyWindowInfo(option: u32, relative_to_window: u32) -> CFArrayRef;
    }

    pub fn enumerate_windows() -> Result<Vec<RawWindow>> {
        let _pool = AutoreleasePool::new();
        let array = unsafe {
            CGWindowListCopyWindowInfo(K_CG_WINDOW_LIST_OPTION_ON_SCREEN_ONLY, K_CG_NULL_WINDOW_ID)
        } as *mut Object;
        if array.is_null() {
            return Err(WorkspaceError::MacOs(
                "CGWindowListCopyWindowInfo returned null".to_string(),
            ));
        }

        let mut windows = Vec::new();
        unsafe {
            let count: usize = msg_send![array, count];
            for index in 0..count {
                let dictionary: *mut Object = msg_send![array, objectAtIndex: index];
                if dictionary.is_null() {
                    continue;
                }
                if let Some(window) = parse_window(dictionary, index as u32) {
                    windows.push(window);
                }
            }
            CFRelease(array as CFTypeRef);
        }
        Ok(windows)
    }

    unsafe fn parse_window(dictionary: *mut Object, z_order: u32) -> Option<RawWindow> {
        let bounds = object_for_key(dictionary, "kCGWindowBounds")?;
        Some(RawWindow {
            window_id: number_for_key(dictionary, "kCGWindowNumber")? as u32,
            owner_pid: number_for_key(dictionary, "kCGWindowOwnerPID")? as i32,
            owner_name: string_for_key(dictionary, "kCGWindowOwnerName")?,
            window_title: string_for_key(dictionary, "kCGWindowName"),
            frame: Frame {
                x: number_for_key(bounds, "X")? as f64,
                y: number_for_key(bounds, "Y")? as f64,
                width: number_for_key(bounds, "Width")? as f64,
                height: number_for_key(bounds, "Height")? as f64,
            },
            layer: number_for_key(dictionary, "kCGWindowLayer").unwrap_or(0) as i32,
            alpha: double_for_key(dictionary, "kCGWindowAlpha").unwrap_or(1.0),
            is_onscreen: bool_for_key(dictionary, "kCGWindowIsOnscreen").unwrap_or(true),
            z_order,
        })
    }

    unsafe fn object_for_key(dictionary: *mut Object, key: &str) -> Option<*mut Object> {
        let key = ns_string(key);
        let value: *mut Object = msg_send![dictionary, objectForKey: key];
        if value.is_null() {
            None
        } else {
            Some(value)
        }
    }

    unsafe fn string_for_key(dictionary: *mut Object, key: &str) -> Option<String> {
        object_for_key(dictionary, key).and_then(|value| nsstring_to_string(value))
    }

    unsafe fn number_for_key(dictionary: *mut Object, key: &str) -> Option<i64> {
        let value = object_for_key(dictionary, key)?;
        let number: i64 = msg_send![value, longLongValue];
        Some(number)
    }

    unsafe fn double_for_key(dictionary: *mut Object, key: &str) -> Option<f64> {
        let value = object_for_key(dictionary, key)?;
        let number: f64 = msg_send![value, doubleValue];
        Some(number)
    }

    unsafe fn bool_for_key(dictionary: *mut Object, key: &str) -> Option<bool> {
        let value = object_for_key(dictionary, key)?;
        let number: bool = msg_send![value, boolValue];
        Some(number)
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::*;

    pub fn enumerate_windows() -> Result<Vec<RawWindow>> {
        Err(WorkspaceError::UnsupportedPlatform)
    }
}

pub use imp::enumerate_windows;
