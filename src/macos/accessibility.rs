use crate::{
    error::{Result, WorkspaceError},
    model::{Frame, WindowSnapshot},
};

#[cfg(target_os = "macos")]
mod imp {
    use core_foundation::base::{CFRelease, CFTypeRef};
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
    }

    pub(super) unsafe fn k_cf_boolean_true() -> CFTypeRef {
        kCFBooleanTrue
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

    pub fn set_window_frame(pid: i32, saved: &WindowSnapshot, target: Frame) -> Result<bool> {
        tracing::debug!(pid, app = %saved.app_name, "creating AX application element");
        let application = unsafe { AXUIElementCreateApplication(pid) };
        if application.is_null() {
            return Ok(false);
        }

        let result = set_window_frame_for_application(application, saved, target);
        unsafe { CFRelease(application as CFTypeRef) };
        result
    }

    pub fn set_window_frames(pid: i32, targets: &[(&WindowSnapshot, Frame)]) -> Result<Vec<bool>> {
        tracing::debug!(
            pid,
            count = targets.len(),
            "creating AX application element for batch restore"
        );
        let application = unsafe { AXUIElementCreateApplication(pid) };
        if application.is_null() {
            return Ok(vec![false; targets.len()]);
        }

        let result = set_window_frames_for_application(application, targets);
        unsafe { CFRelease(application as CFTypeRef) };
        result
    }

    pub fn window_count(pid: i32) -> Result<usize> {
        let application = unsafe { AXUIElementCreateApplication(pid) };
        if application.is_null() {
            return Ok(0);
        }
        let result = window_count_for_application(application);
        unsafe { CFRelease(application as CFTypeRef) };
        result
    }

    pub fn raise_window(pid: i32, saved: &WindowSnapshot) -> Result<bool> {
        tracing::debug!(pid, app = %saved.app_name, "creating AX application element for raise");
        let application = unsafe { AXUIElementCreateApplication(pid) };
        if application.is_null() {
            return Ok(false);
        }

        let result = raise_window_for_application(application, saved);
        unsafe { CFRelease(application as CFTypeRef) };
        result
    }

    pub fn minimize_window(pid: i32, saved: &WindowSnapshot) -> Result<bool> {
        let application = unsafe { AXUIElementCreateApplication(pid) };
        if application.is_null() {
            return Ok(false);
        }
        let result = with_matching_window(application, saved, |window| {
            // Set AXMinimized = true via a CFBoolean true.
            let key = cf_string("AXMinimized");
            let true_val: CFTypeRef = unsafe { k_cf_boolean_true() };
            let error = unsafe { AXUIElementSetAttributeValue(window, key, true_val) };
            unsafe { CFRelease(key as CFTypeRef) };
            if error == K_AX_ERROR_SUCCESS {
                Ok(true)
            } else {
                Err(WorkspaceError::MacOs(format!(
                    "AXUIElementSetAttributeValue(AXMinimized) returned {error}"
                )))
            }
        })
        .map(|matched| matched.unwrap_or(false));
        unsafe { CFRelease(application as CFTypeRef) };
        result
    }

    pub fn close_window(pid: i32, saved: &WindowSnapshot) -> Result<bool> {
        let application = unsafe { AXUIElementCreateApplication(pid) };
        if application.is_null() {
            return Ok(false);
        }
        let result = with_matching_window(application, saved, |window| {
            // Find AXCloseButton subelement and perform AXPress.
            let key = cf_string("AXCloseButton");
            let mut button: CFTypeRef = std::ptr::null();
            let error = unsafe { AXUIElementCopyAttributeValue(window, key, &mut button) };
            unsafe { CFRelease(key as CFTypeRef) };
            if error != K_AX_ERROR_SUCCESS || button.is_null() {
                return Ok(false);
            }
            let pressed = perform_action(button as AXUIElementRef, "AXPress")?;
            unsafe { CFRelease(button) };
            Ok(pressed)
        })
        .map(|matched| matched.unwrap_or(false));
        unsafe { CFRelease(application as CFTypeRef) };
        result
    }

    fn set_window_frame_for_application(
        application: AXUIElementRef,
        saved: &WindowSnapshot,
        target: Frame,
    ) -> Result<bool> {
        with_matching_window(application, saved, |window| set_and_verify(window, target))
            .map(|matched| matched.unwrap_or(false))
    }

    fn set_window_frames_for_application(
        application: AXUIElementRef,
        targets: &[(&WindowSnapshot, Frame)],
    ) -> Result<Vec<bool>> {
        let app_name = targets
            .first()
            .map(|target| target.0.app_name.as_str())
            .unwrap_or("unknown");
        let Some((windows_value, candidates)) = candidate_windows(application, app_name)? else {
            return Ok(vec![false; targets.len()]);
        };

        let saved: Vec<WindowMatchInput> = targets
            .iter()
            .map(|(window, _)| WindowMatchInput {
                title: window.title.clone(),
                frame: window.frame,
            })
            .collect();
        let candidate_inputs: Vec<WindowMatchInput> = candidates
            .iter()
            .map(|candidate| WindowMatchInput {
                title: candidate.title.clone(),
                frame: candidate.frame,
            })
            .collect();
        let assignments = assign_distinct_windows(&saved, &candidate_inputs);
        let mut results = vec![false; targets.len()];

        for (target_index, assignment) in assignments.into_iter().enumerate() {
            let Some(candidate_index) = assignment else {
                continue;
            };
            results[target_index] = set_and_verify_for_app(
                app_name,
                candidates[candidate_index].element,
                targets[target_index].1,
            )?;
        }

        unsafe { CFRelease(windows_value) };
        Ok(results)
    }

    fn window_count_for_application(application: AXUIElementRef) -> Result<usize> {
        let windows_key = cf_string("AXWindows");
        let mut windows_value: CFTypeRef = std::ptr::null();
        let error =
            unsafe { AXUIElementCopyAttributeValue(application, windows_key, &mut windows_value) };
        unsafe { CFRelease(windows_key as CFTypeRef) };

        if error != K_AX_ERROR_SUCCESS || windows_value.is_null() {
            return Ok(0);
        }

        let count = unsafe {
            let array = windows_value as *mut Object;
            let count: usize = msg_send![array, count];
            count
        };
        unsafe { CFRelease(windows_value) };
        Ok(count)
    }

    fn raise_window_for_application(
        application: AXUIElementRef,
        saved: &WindowSnapshot,
    ) -> Result<bool> {
        with_matching_window(application, saved, |window| {
            perform_action(window, "AXRaise")
        })
        .map(|matched| matched.unwrap_or(false))
    }

    fn with_matching_window<T>(
        application: AXUIElementRef,
        saved: &WindowSnapshot,
        operation: impl FnOnce(AXUIElementRef) -> Result<T>,
    ) -> Result<Option<T>> {
        let windows_key = cf_string("AXWindows");
        let mut windows_value: CFTypeRef = std::ptr::null();
        tracing::debug!(app = %saved.app_name, "copying AX windows attribute");
        let error =
            unsafe { AXUIElementCopyAttributeValue(application, windows_key, &mut windows_value) };
        unsafe { CFRelease(windows_key as CFTypeRef) };

        if error != K_AX_ERROR_SUCCESS || windows_value.is_null() {
            return Ok(None);
        }

        let mut best_window = std::ptr::null();
        let mut best_score = i32::MIN;

        unsafe {
            let array = windows_value as *mut Object;
            tracing::debug!(app = %saved.app_name, "reading AX window array count");
            let count: usize = msg_send![array, count];
            tracing::debug!(app = %saved.app_name, count, "matching AX windows");
            for index in 0..count {
                tracing::debug!(app = %saved.app_name, index, "reading AX window from array");
                let window: AXUIElementRef = msg_send![array, objectAtIndex: index];
                if window.is_null() {
                    continue;
                }
                let title = copy_string_attribute(window, "AXTitle");
                let frame = read_frame(window).unwrap_or(saved.frame);
                let score = score_window(saved, title.as_deref(), frame);
                if score > best_score {
                    best_score = score;
                    best_window = window;
                }
            }
        }

        let result = if best_window.is_null() {
            Ok(None)
        } else {
            operation(best_window).map(Some)
        };

        unsafe { CFRelease(windows_value) };
        result
    }

    struct AxWindowCandidate {
        element: AXUIElementRef,
        title: Option<String>,
        frame: Frame,
    }

    #[derive(Debug, Clone)]
    pub(super) struct WindowMatchInput {
        pub(super) title: Option<String>,
        pub(super) frame: Frame,
    }

    fn candidate_windows(
        application: AXUIElementRef,
        app_name: &str,
    ) -> Result<Option<(CFTypeRef, Vec<AxWindowCandidate>)>> {
        let windows_key = cf_string("AXWindows");
        let mut windows_value: CFTypeRef = std::ptr::null();
        tracing::debug!(app = %app_name, "copying AX windows attribute");
        let error =
            unsafe { AXUIElementCopyAttributeValue(application, windows_key, &mut windows_value) };
        unsafe { CFRelease(windows_key as CFTypeRef) };

        if error != K_AX_ERROR_SUCCESS || windows_value.is_null() {
            return Ok(None);
        }

        let mut candidates = Vec::new();
        unsafe {
            let array = windows_value as *mut Object;
            let count: usize = msg_send![array, count];
            tracing::debug!(app = %app_name, count, "reading AX windows");
            for index in 0..count {
                let window: AXUIElementRef = msg_send![array, objectAtIndex: index];
                if window.is_null() {
                    continue;
                }
                candidates.push(AxWindowCandidate {
                    element: window,
                    title: copy_string_attribute(window, "AXTitle"),
                    frame: read_frame(window).unwrap_or(Frame {
                        x: 0.0,
                        y: 0.0,
                        width: 0.0,
                        height: 0.0,
                    }),
                });
            }
        }

        Ok(Some((windows_value, candidates)))
    }

    pub(super) fn assign_distinct_windows(
        saved: &[WindowMatchInput],
        candidates: &[WindowMatchInput],
    ) -> Vec<Option<usize>> {
        let mut pairs = Vec::new();
        for (saved_index, saved_window) in saved.iter().enumerate() {
            for (candidate_index, candidate) in candidates.iter().enumerate() {
                pairs.push((
                    score_match(
                        saved_window.title.as_deref(),
                        saved_window.frame,
                        candidate.title.as_deref(),
                        candidate.frame,
                    ),
                    saved_index,
                    candidate_index,
                ));
            }
        }
        pairs.sort_by_key(|pair| std::cmp::Reverse(pair.0));

        let mut assignments = vec![None; saved.len()];
        let mut used_candidates = vec![false; candidates.len()];
        for (_, saved_index, candidate_index) in pairs {
            if assignments[saved_index].is_some() || used_candidates[candidate_index] {
                continue;
            }
            assignments[saved_index] = Some(candidate_index);
            used_candidates[candidate_index] = true;
        }

        assignments
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
            .unwrap_or(true))
    }

    fn set_and_verify_for_app(
        app_name: &str,
        window: AXUIElementRef,
        target: Frame,
    ) -> Result<bool> {
        if app_name == "Google Chrome" {
            set_size(window, target)?;
            set_position(window, target)?;
            return Ok(true);
        }

        set_and_verify(window, target)
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

    fn score_window(saved: &WindowSnapshot, title: Option<&str>, frame: Frame) -> i32 {
        score_match(saved.title.as_deref(), saved.frame, title, frame)
    }

    fn score_match(
        saved_title: Option<&str>,
        saved_frame: Frame,
        candidate_title: Option<&str>,
        candidate_frame: Frame,
    ) -> i32 {
        let mut score = 0;
        if let (Some(saved_title), Some(candidate_title)) = (saved_title, candidate_title) {
            if saved_title == candidate_title {
                score += 1000;
            } else if candidate_title.contains(saved_title) || saved_title.contains(candidate_title)
            {
                score += 250;
            }
        }

        let distance = (saved_frame.x - candidate_frame.x).abs()
            + (saved_frame.y - candidate_frame.y).abs()
            + (saved_frame.width - candidate_frame.width).abs()
            + (saved_frame.height - candidate_frame.height).abs();
        score - distance.min(5000.0) as i32
    }

    fn frames_close(left: Frame, right: Frame) -> bool {
        (left.x - right.x).abs() <= 2.0
            && (left.y - right.y).abs() <= 2.0
            && (left.width - right.width).abs() <= 2.0
            && (left.height - right.height).abs() <= 2.0
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

    pub fn set_window_frames(
        _pid: i32,
        _targets: &[(&WindowSnapshot, Frame)],
    ) -> Result<Vec<bool>> {
        Err(WorkspaceError::UnsupportedPlatform)
    }

    pub fn window_count(_pid: i32) -> Result<usize> {
        Err(WorkspaceError::UnsupportedPlatform)
    }

    pub fn raise_window(_pid: i32, _saved: &WindowSnapshot) -> Result<bool> {
        Err(WorkspaceError::UnsupportedPlatform)
    }
}

pub use imp::{
    close_window, ensure_trusted, is_trusted, minimize_window, raise_window, set_window_frame,
    set_window_frames, window_count,
};

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::imp::{assign_distinct_windows, WindowMatchInput};
    use crate::model::Frame;

    fn input(title: &str, x: f64) -> WindowMatchInput {
        WindowMatchInput {
            title: Some(title.to_string()),
            frame: Frame {
                x,
                y: 0.0,
                width: 800.0,
                height: 600.0,
            },
        }
    }

    #[test]
    fn assigns_saved_windows_to_distinct_candidates() {
        let saved = vec![
            input("Docs", 0.0),
            input("Docs", 900.0),
            input("Docs", 1800.0),
        ];
        let candidates = vec![
            input("Docs", 1800.0),
            input("Docs", 0.0),
            input("Docs", 900.0),
        ];

        let assignments = assign_distinct_windows(&saved, &candidates);

        assert_eq!(assignments, vec![Some(1), Some(2), Some(0)]);
    }
}
