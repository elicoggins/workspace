use crate::{
    error::{Result, WorkspaceError},
    model::{Frame, WindowSnapshot},
};

/// Window state read through Accessibility.
#[derive(Debug, Clone, PartialEq)]
pub struct AxWindowState {
    pub title: Option<String>,
    pub frame: Option<Frame>,
    pub minimized: bool,
    pub fullscreen: bool,
}

// Resolve against the selected live observation. Ambiguous matches are rejected.
#[cfg(any(target_os = "macos", test))]
fn observed_window_index(observed: &WindowSnapshot, states: &[AxWindowState]) -> Option<usize> {
    if observed.frame.width <= 0.0 || observed.frame.height <= 0.0 {
        return None;
    }
    let mut matches = states.iter().enumerate().filter(|(_, state)| {
        !state.fullscreen
            && state.minimized == observed.minimized
            && state
                .frame
                .is_some_and(|frame| frames_close(frame, observed.frame))
            && observed
                .title
                .as_ref()
                .is_none_or(|title| state.title.as_ref() == Some(title))
    });
    let first = matches.next()?.0;
    matches.next().is_none().then_some(first)
}

#[cfg(any(target_os = "macos", test))]
fn observed_window_indices(
    observed: &[WindowSnapshot],
    states: &[AxWindowState],
) -> Vec<Option<usize>> {
    let indices: Vec<_> = observed
        .iter()
        .map(|window| observed_window_index(window, states))
        .collect();
    indices
        .iter()
        .map(|index| index.filter(|_| indices.iter().filter(|other| *other == index).count() == 1))
        .collect()
}

#[cfg(any(target_os = "macos", test))]
fn frames_close(left: Frame, right: Frame) -> bool {
    (left.x - right.x).abs() <= 2.0
        && (left.y - right.y).abs() <= 2.0
        && (left.width - right.width).abs() <= 2.0
        && (left.height - right.height).abs() <= 2.0
}

#[cfg(target_os = "macos")]
mod imp {
    use core_foundation::base::{CFEqual, CFRelease, CFRetain, CFTypeRef};
    use libc::{c_char, c_void, pid_t};
    use objc::{msg_send, runtime::Object, sel, sel_impl};

    use super::*;
    use crate::macos::util::objc_util::nsstring_to_string;

    type AXError = i32;
    type AXUIElementRef = *const c_void;
    type AXValueRef = *const c_void;
    type CFStringRef = *const c_void;

    const K_AX_ERROR_SUCCESS: AXError = 0;
    const K_AX_VALUE_CG_POINT: i32 = 1;
    const K_AX_VALUE_CG_SIZE: i32 = 2;
    const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

    #[repr(C)]
    #[derive(Debug, Copy, Clone, Default)]
    struct AxPoint {
        x: f64,
        y: f64,
    }

    #[repr(C)]
    #[derive(Debug, Copy, Clone, Default)]
    struct AxSize {
        width: f64,
        height: f64,
    }

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrusted() -> bool;
        fn AXUIElementCreateApplication(pid: pid_t) -> AXUIElementRef;
        fn AXUIElementCopyAttributeValue(
            element: AXUIElementRef,
            attribute: CFStringRef,
            value: *mut CFTypeRef,
        ) -> AXError;
        fn AXUIElementSetAttributeValue(
            element: AXUIElementRef,
            attribute: CFStringRef,
            value: CFTypeRef,
        ) -> AXError;
        fn AXUIElementPerformAction(element: AXUIElementRef, action: CFStringRef) -> AXError;
        fn AXValueCreate(value_type: i32, value: *const c_void) -> AXValueRef;
        fn AXValueGetValue(value: AXValueRef, value_type: i32, output: *mut c_void) -> bool;
        fn CFStringCreateWithCString(
            allocator: *const c_void,
            c_str: *const c_char,
            encoding: u32,
        ) -> CFStringRef;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        static kCFBooleanTrue: CFTypeRef;
        static kCFBooleanFalse: CFTypeRef;
    }

    pub(super) unsafe fn k_cf_boolean(value: bool) -> CFTypeRef {
        if value {
            kCFBooleanTrue
        } else {
            kCFBooleanFalse
        }
    }

    pub fn ensure_trusted() -> Result<()> {
        let trusted = unsafe { AXIsProcessTrusted() };
        if trusted {
            Ok(())
        } else {
            Err(WorkspaceError::AccessibilityPermissionRequired)
        }
    }

    pub fn is_trusted() -> bool {
        unsafe { AXIsProcessTrusted() }
    }

    /// Owns an AX window reference; cloning retains it and dropping releases it.
    pub struct WindowHandle(AXUIElementRef);

    impl Clone for WindowHandle {
        fn clone(&self) -> Self {
            unsafe { CFRetain(self.0 as CFTypeRef) };
            Self(self.0)
        }
    }

    impl Drop for WindowHandle {
        fn drop(&mut self) {
            unsafe { CFRelease(self.0 as CFTypeRef) };
        }
    }

    impl WindowHandle {
        pub fn same_window(&self, other: &Self) -> bool {
            unsafe { CFEqual(self.0 as CFTypeRef, other.0 as CFTypeRef) != 0 }
        }

        pub fn set_frame(&self, target: Frame) -> Result<bool> {
            if self.is_fullscreen() {
                return Ok(false);
            }
            set_and_verify(self.0, target)
        }

        pub fn raise(&self) -> Result<bool> {
            if self.is_fullscreen() {
                return Ok(false);
            }
            perform_action(self.0, "AXRaise")
        }

        pub fn is_fullscreen(&self) -> bool {
            copy_bool_attribute(self.0, "AXFullScreen") == Some(true)
        }

        pub fn set_minimized(&self, minimized: bool) -> Result<bool> {
            if self.is_fullscreen() {
                return Ok(false);
            }
            let key = cf_string("AXMinimized");
            let error =
                unsafe { AXUIElementSetAttributeValue(self.0, key, k_cf_boolean(minimized)) };
            unsafe { CFRelease(key as CFTypeRef) };
            if error != K_AX_ERROR_SUCCESS {
                return Err(WorkspaceError::MacOs(format!(
                    "AXUIElementSetAttributeValue(AXMinimized) returned {error}"
                )));
            }
            Ok(copy_bool_attribute(self.0, "AXMinimized") == Some(minimized))
        }

        pub fn close(&self) -> Result<bool> {
            if self.is_fullscreen() {
                return Ok(false);
            }
            let key = cf_string("AXCloseButton");
            let mut button: CFTypeRef = std::ptr::null();
            let error = unsafe { AXUIElementCopyAttributeValue(self.0, key, &mut button) };
            unsafe { CFRelease(key as CFTypeRef) };
            if error != K_AX_ERROR_SUCCESS || button.is_null() {
                return Ok(false);
            }
            let result = perform_action(button as AXUIElementRef, "AXPress");
            // Release the copied button even when AXPress fails.
            unsafe { CFRelease(button) };
            result
        }
    }

    pub fn resolve_window(pid: i32, observed: &WindowSnapshot) -> Result<Option<WindowHandle>> {
        let application = unsafe { AXUIElementCreateApplication(pid) };
        if application.is_null() {
            return Ok(None);
        }
        let result = with_matching_window(application, observed, |window| {
            unsafe { CFRetain(window as CFTypeRef) };
            Ok(WindowHandle(window))
        });
        unsafe { CFRelease(application as CFTypeRef) };
        result
    }

    pub fn resolve_windows(
        pid: i32,
        observed: &[WindowSnapshot],
    ) -> Result<Vec<Option<WindowHandle>>> {
        let application = unsafe { AXUIElementCreateApplication(pid) };
        if application.is_null() {
            return Ok(observed.iter().map(|_| None).collect());
        }
        let result = with_windows(application, |windows, states| {
            Ok(observed_window_indices(observed, states)
                .into_iter()
                .map(|index| {
                    index.map(|index| {
                        unsafe { CFRetain(windows[index] as CFTypeRef) };
                        WindowHandle(windows[index])
                    })
                })
                .collect())
        });
        unsafe { CFRelease(application as CFTypeRef) };
        result
    }

    /// Retain all AX windows, including ambiguous ones, to identify windows
    /// that predate a creation operation while AX catches up to CG.
    pub fn window_handles(pid: i32) -> Result<Vec<WindowHandle>> {
        let application = unsafe { AXUIElementCreateApplication(pid) };
        if application.is_null() {
            return Ok(Vec::new());
        }
        let result = with_windows(application, |windows, _| {
            Ok(windows
                .iter()
                .map(|window| {
                    unsafe { CFRetain(*window as CFTypeRef) };
                    WindowHandle(*window)
                })
                .collect())
        });
        unsafe { CFRelease(application as CFTypeRef) };
        result
    }

    pub fn set_window_frame(pid: i32, saved: &WindowSnapshot, target: Frame) -> Result<bool> {
        match resolve_window(pid, saved)? {
            Some(handle) => handle.set_frame(target),
            None => Ok(false),
        }
    }

    pub fn raise_window(pid: i32, saved: &WindowSnapshot) -> Result<bool> {
        match resolve_window(pid, saved)? {
            Some(handle) => handle.raise(),
            None => Ok(false),
        }
    }

    pub fn minimize_window(pid: i32, saved: &WindowSnapshot) -> Result<bool> {
        set_window_minimized(pid, saved, true)
    }

    pub fn unminimize_window(pid: i32, saved: &WindowSnapshot) -> Result<bool> {
        set_window_minimized(pid, saved, false)
    }

    fn set_window_minimized(pid: i32, saved: &WindowSnapshot, minimized: bool) -> Result<bool> {
        match resolve_window(pid, saved)? {
            Some(handle) => handle.set_minimized(minimized),
            None => Ok(false),
        }
    }

    /// Read titles, frames, and visibility flags for one process's AX windows.
    pub fn ax_window_states(pid: i32) -> Result<Vec<AxWindowState>> {
        let application = unsafe { AXUIElementCreateApplication(pid) };
        if application.is_null() {
            return Ok(Vec::new());
        }

        let windows_key = cf_string("AXWindows");
        let mut windows_value: CFTypeRef = std::ptr::null();
        let error =
            unsafe { AXUIElementCopyAttributeValue(application, windows_key, &mut windows_value) };
        unsafe { CFRelease(windows_key as CFTypeRef) };
        if error != K_AX_ERROR_SUCCESS || windows_value.is_null() {
            unsafe { CFRelease(application as CFTypeRef) };
            return Ok(Vec::new());
        }

        let mut states = Vec::new();
        unsafe {
            let array = windows_value as *mut Object;
            let count: usize = msg_send![array, count];
            for index in 0..count {
                let window: AXUIElementRef = msg_send![array, objectAtIndex: index];
                if window.is_null() {
                    continue;
                }
                states.push(AxWindowState {
                    title: copy_string_attribute(window, "AXTitle"),
                    frame: read_frame(window),
                    minimized: copy_bool_attribute(window, "AXMinimized").unwrap_or(false),
                    fullscreen: copy_bool_attribute(window, "AXFullScreen").unwrap_or(false),
                });
            }
            CFRelease(windows_value);
            CFRelease(application as CFTypeRef);
        }
        Ok(states)
    }

    pub fn close_window(pid: i32, saved: &WindowSnapshot) -> Result<bool> {
        match resolve_window(pid, saved)? {
            Some(handle) => handle.close(),
            None => Ok(false),
        }
    }

    fn with_matching_window<T>(
        application: AXUIElementRef,
        saved: &WindowSnapshot,
        operation: impl FnOnce(AXUIElementRef) -> Result<T>,
    ) -> Result<Option<T>> {
        with_windows(application, |windows, states| {
            match observed_window_index(saved, states) {
                Some(index) => operation(windows[index]).map(Some),
                None => Ok(None),
            }
        })
    }

    fn with_windows<T>(
        application: AXUIElementRef,
        operation: impl FnOnce(&[AXUIElementRef], &[AxWindowState]) -> Result<T>,
    ) -> Result<T> {
        let windows_key = cf_string("AXWindows");
        let mut windows_value: CFTypeRef = std::ptr::null();
        tracing::debug!("copying AX windows attribute");
        let error =
            unsafe { AXUIElementCopyAttributeValue(application, windows_key, &mut windows_value) };
        unsafe { CFRelease(windows_key as CFTypeRef) };

        if error != K_AX_ERROR_SUCCESS || windows_value.is_null() {
            return Err(WorkspaceError::MacOs(format!(
                "could not observe AXWindows (error {error})"
            )));
        }

        let mut windows = Vec::new();
        let mut states = Vec::new();

        unsafe {
            let array = windows_value as *mut Object;
            tracing::debug!("reading AX window array count");
            let count: usize = msg_send![array, count];
            tracing::debug!(count, "matching AX windows");
            for index in 0..count {
                tracing::debug!(index, "reading AX window from array");
                let window: AXUIElementRef = msg_send![array, objectAtIndex: index];
                if window.is_null() {
                    continue;
                }
                windows.push(window);
                states.push(AxWindowState {
                    title: copy_string_attribute(window, "AXTitle"),
                    frame: read_frame(window),
                    minimized: copy_bool_attribute(window, "AXMinimized").unwrap_or(false),
                    fullscreen: copy_bool_attribute(window, "AXFullScreen").unwrap_or(false),
                });
            }
        }

        let result = operation(&windows, &states);

        unsafe { CFRelease(windows_value) };
        result
    }

    fn set_and_verify(window: AXUIElementRef, target: Frame) -> Result<bool> {
        set_size(window, target)?;
        set_position(window, target)?;

        if read_frame(window)
            .map(|frame| frames_close(frame, target))
            .unwrap_or(false)
        {
            return Ok(true);
        }

        set_size(window, target)?;
        set_position(window, target)?;
        Ok(read_frame(window)
            .map(|frame| frames_close(frame, target))
            .unwrap_or(false))
    }

    fn set_position(window: AXUIElementRef, frame: Frame) -> Result<()> {
        let point = AxPoint {
            x: frame.x,
            y: frame.y,
        };
        set_ax_value(
            window,
            "AXPosition",
            K_AX_VALUE_CG_POINT,
            &point as *const _ as *const c_void,
        )
    }

    fn set_size(window: AXUIElementRef, frame: Frame) -> Result<()> {
        let size = AxSize {
            width: frame.width,
            height: frame.height,
        };
        set_ax_value(
            window,
            "AXSize",
            K_AX_VALUE_CG_SIZE,
            &size as *const _ as *const c_void,
        )
    }

    fn set_ax_value(
        window: AXUIElementRef,
        attribute: &str,
        value_type: i32,
        value_pointer: *const c_void,
    ) -> Result<()> {
        let key = cf_string(attribute);
        let value = unsafe { AXValueCreate(value_type, value_pointer) };
        if value.is_null() {
            unsafe { CFRelease(key as CFTypeRef) };
            return Err(WorkspaceError::MacOs(format!(
                "AXValueCreate failed for {attribute}"
            )));
        }
        let error = unsafe { AXUIElementSetAttributeValue(window, key, value as CFTypeRef) };
        unsafe {
            CFRelease(key as CFTypeRef);
            CFRelease(value as CFTypeRef);
        }
        if error == K_AX_ERROR_SUCCESS {
            Ok(())
        } else {
            Err(WorkspaceError::MacOs(format!(
                "AXUIElementSetAttributeValue({attribute}) returned {error}"
            )))
        }
    }

    fn perform_action(window: AXUIElementRef, action: &str) -> Result<bool> {
        let action = cf_string(action);
        let error = unsafe { AXUIElementPerformAction(window, action) };
        unsafe { CFRelease(action as CFTypeRef) };

        if error == K_AX_ERROR_SUCCESS {
            Ok(true)
        } else {
            Err(WorkspaceError::MacOs(format!(
                "AXUIElementPerformAction returned {error}"
            )))
        }
    }

    fn read_frame(window: AXUIElementRef) -> Option<Frame> {
        let position = copy_ax_value(window, "AXPosition").and_then(|value| {
            let mut point = AxPoint::default();
            let ok = unsafe {
                AXValueGetValue(
                    value,
                    K_AX_VALUE_CG_POINT,
                    &mut point as *mut _ as *mut c_void,
                )
            };
            unsafe { CFRelease(value as CFTypeRef) };
            ok.then_some(point)
        })?;

        let size = copy_ax_value(window, "AXSize").and_then(|value| {
            let mut size = AxSize::default();
            let ok = unsafe {
                AXValueGetValue(
                    value,
                    K_AX_VALUE_CG_SIZE,
                    &mut size as *mut _ as *mut c_void,
                )
            };
            unsafe { CFRelease(value as CFTypeRef) };
            ok.then_some(size)
        })?;

        Some(Frame {
            x: position.x,
            y: position.y,
            width: size.width,
            height: size.height,
        })
    }

    fn copy_ax_value(window: AXUIElementRef, attribute: &str) -> Option<AXValueRef> {
        let key = cf_string(attribute);
        let mut value: CFTypeRef = std::ptr::null();
        let error = unsafe { AXUIElementCopyAttributeValue(window, key, &mut value) };
        unsafe { CFRelease(key as CFTypeRef) };
        if error == K_AX_ERROR_SUCCESS && !value.is_null() {
            Some(value as AXValueRef)
        } else {
            None
        }
    }

    fn copy_bool_attribute(window: AXUIElementRef, attribute: &str) -> Option<bool> {
        let key = cf_string(attribute);
        let mut value: CFTypeRef = std::ptr::null();
        let error = unsafe { AXUIElementCopyAttributeValue(window, key, &mut value) };
        unsafe { CFRelease(key as CFTypeRef) };
        if error != K_AX_ERROR_SUCCESS || value.is_null() {
            return None;
        }
        let result = unsafe {
            let object = value as *mut Object;
            let flag: bool = msg_send![object, boolValue];
            CFRelease(value);
            flag
        };
        Some(result)
    }

    fn copy_string_attribute(window: AXUIElementRef, attribute: &str) -> Option<String> {
        let key = cf_string(attribute);
        let mut value: CFTypeRef = std::ptr::null();
        let error = unsafe { AXUIElementCopyAttributeValue(window, key, &mut value) };
        unsafe { CFRelease(key as CFTypeRef) };
        if error != K_AX_ERROR_SUCCESS || value.is_null() {
            return None;
        }
        let string = unsafe { nsstring_to_string(value as *mut Object) };
        unsafe { CFRelease(value) };
        string
    }

    fn cf_string(value: &str) -> CFStringRef {
        let c_string = std::ffi::CString::new(value).expect("CFString contained an interior null");
        unsafe {
            CFStringCreateWithCString(
                std::ptr::null(),
                c_string.as_ptr(),
                K_CF_STRING_ENCODING_UTF8,
            )
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::*;

    pub fn ensure_trusted() -> Result<()> {
        Err(WorkspaceError::UnsupportedPlatform)
    }

    pub fn is_trusted() -> bool {
        false
    }

    pub fn set_window_frame(_pid: i32, _saved: &WindowSnapshot, _target: Frame) -> Result<bool> {
        Err(WorkspaceError::UnsupportedPlatform)
    }

    pub fn raise_window(_pid: i32, _saved: &WindowSnapshot) -> Result<bool> {
        Err(WorkspaceError::UnsupportedPlatform)
    }

    pub fn minimize_window(_pid: i32, _saved: &WindowSnapshot) -> Result<bool> {
        Err(WorkspaceError::UnsupportedPlatform)
    }

    pub fn unminimize_window(_pid: i32, _saved: &WindowSnapshot) -> Result<bool> {
        Err(WorkspaceError::UnsupportedPlatform)
    }

    pub fn close_window(_pid: i32, _saved: &WindowSnapshot) -> Result<bool> {
        Err(WorkspaceError::UnsupportedPlatform)
    }

    pub fn ax_window_states(_pid: i32) -> Result<Vec<AxWindowState>> {
        Ok(Vec::new())
    }
}

pub use imp::{
    ax_window_states, close_window, ensure_trusted, is_trusted, minimize_window, raise_window,
    set_window_frame, unminimize_window,
};

#[cfg(target_os = "macos")]
pub use imp::{resolve_window, resolve_windows, window_handles, WindowHandle};

#[cfg(test)]
mod tests {
    use super::*;

    fn observed(title: Option<&str>, x: f64) -> WindowSnapshot {
        WindowSnapshot {
            window_id: 1,
            app_name: "Terminal".into(),
            process_name: "Terminal".into(),
            bundle_id: Some("com.apple.Terminal".into()),
            pid: 1,
            title: title.map(str::to_owned),
            frame: Frame {
                x,
                y: 0.0,
                width: 800.0,
                height: 600.0,
            },
            display_id: None,
            display_frame: None,
            display_relative_frame: None,
            z_order: None,
            fullscreen: false,
            minimized: false,
            enabled: true,
            browser_tabs: vec![],
        }
    }

    fn state(window: &WindowSnapshot) -> AxWindowState {
        AxWindowState {
            title: window.title.clone(),
            frame: Some(window.frame),
            minimized: window.minimized,
            fullscreen: window.fullscreen,
        }
    }

    #[test]
    fn identity_follows_the_planned_live_observation() {
        let historical = observed(Some("wanted"), 0.0);
        let live = observed(Some("wanted"), 5000.0);
        let other = observed(Some("unrelated"), historical.frame.x);
        let states = vec![state(&other), state(&live)];
        assert_eq!(observed_window_index(&live, &states), Some(1));
        // If that window disappears, the old coordinates do not pick a replacement.
        assert_eq!(observed_window_index(&live, &states[..1]), None);
    }

    #[test]
    fn missing_frames_and_contradictory_titles_are_rejected() {
        let live = observed(Some("wanted"), 0.0);
        let mut unreadable = state(&live);
        unreadable.frame = None;
        let different = state(&observed(Some("unrelated"), 0.0));
        assert_eq!(observed_window_index(&live, &[unreadable, different]), None);
    }

    #[test]
    fn overlapping_windows_require_unique_evidence() {
        let first = observed(Some("first"), 0.0);
        let second = observed(Some("second"), 0.0);
        let states = vec![state(&first), state(&second)];
        assert_eq!(observed_window_index(&first, &states), Some(0));
        assert_eq!(observed_window_index(&second, &states), Some(1));
        assert_eq!(observed_window_index(&observed(None, 0.0), &states), None);
        assert_eq!(
            observed_window_index(&first, &[state(&first), state(&first)]),
            None
        );
    }

    #[test]
    fn two_observations_cannot_bind_the_same_ax_window() {
        let first = observed(Some("window"), 0.0);
        let nearby = observed(Some("window"), 1.0);
        assert_eq!(
            observed_window_indices(&[first.clone(), nearby], &[state(&first)]),
            vec![None, None]
        );
        let separate = observed(Some("window"), 1000.0);
        assert_eq!(
            observed_window_indices(
                &[first.clone(), separate.clone()],
                &[state(&first), state(&separate)]
            ),
            vec![Some(0), Some(1)]
        );
    }

    #[test]
    fn identity_checks_visibility_and_fullscreen_state() {
        let mut live = observed(Some("window"), 0.0);
        live.minimized = true;
        let mut candidate = state(&live);
        candidate.minimized = false;
        assert_eq!(observed_window_index(&live, &[candidate]), None);
        assert_eq!(observed_window_index(&live, &[state(&live)]), Some(0));
        let mut fullscreen = state(&live);
        fullscreen.fullscreen = true;
        assert_eq!(observed_window_index(&live, &[fullscreen]), None);
    }

    #[test]
    fn identity_requires_valid_geometry_within_tolerance() {
        let live = observed(None, 0.0);
        let mut near = state(&live);
        near.frame.as_mut().unwrap().x += 1.9;
        assert_eq!(observed_window_index(&live, &[near.clone()]), Some(0));
        near.frame.as_mut().unwrap().width += 3.0;
        assert_eq!(observed_window_index(&live, &[near]), None);
        let mut invalid = live.clone();
        invalid.frame.width = 0.0;
        assert_eq!(observed_window_index(&invalid, &[state(&invalid)]), None);
        invalid.frame.width = f64::NAN;
        assert_eq!(observed_window_index(&invalid, &[state(&invalid)]), None);
    }
}
