#[cfg(target_os = "macos")]
pub mod objc_util {
    use std::ffi::{CStr, CString};

    use objc::{class, msg_send, runtime::Object, sel, sel_impl};

    pub struct AutoreleasePool {
        pool: *mut Object,
    }

    impl AutoreleasePool {
        pub fn new() -> Self {
            let pool = unsafe { msg_send![class!(NSAutoreleasePool), new] };
            Self { pool }
        }
    }

    impl Default for AutoreleasePool {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Drop for AutoreleasePool {
        fn drop(&mut self) {
            unsafe {
                let _: () = msg_send![self.pool, drain];
            }
        }
    }

    pub fn ns_string(value: &str) -> *mut Object {
        let c_string = CString::new(value).expect("Objective-C string contained an interior null");
        unsafe { msg_send![class!(NSString), stringWithUTF8String: c_string.as_ptr()] }
    }

    /// Converts an Objective-C NSString-compatible object into a Rust `String`.
    ///
    /// # Safety
    ///
    /// `value` must be null or a valid Objective-C object that responds to `UTF8String` for the
    /// duration of this call.
    pub unsafe fn nsstring_to_string(value: *mut Object) -> Option<String> {
        if value.is_null() {
            return None;
        }
        let c_string: *const libc::c_char = msg_send![value, UTF8String];
        if c_string.is_null() {
            None
        } else {
            Some(CStr::from_ptr(c_string).to_string_lossy().into_owned())
        }
    }
}
