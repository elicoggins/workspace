#[derive(Debug, Clone)]
pub struct RunningAppInfo {
    pub bundle_id: Option<String>,
    pub localized_name: Option<String>,
    pub process_name: Option<String>,
}

#[cfg(target_os = "macos")]
mod imp {
    use std::process::Command;

    use objc::{class, msg_send, runtime::Object, sel, sel_impl};

    use super::RunningAppInfo;
    use crate::{
        error::{Result, WorkspaceError},
        macos::util::objc_util::{nsstring_to_string, AutoreleasePool},
    };

    const NS_APPLICATION_ACTIVATE_IGNORING_OTHER_APPS: u64 = 1 << 1;

    #[link(name = "AppKit", kind = "framework")]
    extern "C" {}

    pub fn application_for_pid(pid: i32) -> Option<RunningAppInfo> {
        let _pool = AutoreleasePool::new();
        unsafe {
            let app: *mut Object = msg_send![
                class!(NSRunningApplication),
                runningApplicationWithProcessIdentifier: pid
            ];
            if app.is_null() {
                return None;
            }

            let bundle: *mut Object = msg_send![app, bundleIdentifier];
            let localized: *mut Object = msg_send![app, localizedName];
            let executable_url: *mut Object = msg_send![app, executableURL];
            let last_path_component: *mut Object = if executable_url.is_null() {
                std::ptr::null_mut()
            } else {
                msg_send![executable_url, lastPathComponent]
            };

            Some(RunningAppInfo {
                bundle_id: nsstring_to_string(bundle),
                localized_name: nsstring_to_string(localized),
                process_name: nsstring_to_string(last_path_component),
            })
        }
    }

    pub fn launch_bundle(bundle_id: &str) -> Result<bool> {
        let status = Command::new("/usr/bin/open")
            .arg("-b")
            .arg(bundle_id)
            .status()
            .map_err(|source| {
                WorkspaceError::MacOs(format!("failed to launch {bundle_id}: {source}"))
            })?;

        if status.success() {
            Ok(true)
        } else {
            Err(WorkspaceError::MacOs(format!(
                "failed to launch {bundle_id}: open exited with {status}"
            )))
        }
    }

    pub fn create_new_window(bundle_id: &str, process_name: &str) -> Result<bool> {
        let script = format!(
            "tell application id {} to activate\n\
             delay 0.2\n\
             tell application \"System Events\"\n\
             	if exists process {} then\n\
             		tell process {} to keystroke \"n\" using command down\n\
             	end if\n\
             end tell",
            applescript_string(bundle_id),
            applescript_string(process_name),
            applescript_string(process_name)
        );
        let status = Command::new("/usr/bin/osascript")
            .arg("-e")
            .arg(script)
            .status()
            .map_err(|source| {
                WorkspaceError::MacOs(format!(
                    "failed to create new window for {bundle_id}: {source}"
                ))
            })?;

        if status.success() {
            Ok(true)
        } else {
            Err(WorkspaceError::MacOs(format!(
                "failed to create new window for {bundle_id}: osascript exited with {status}"
            )))
        }
    }

    pub fn activate_bundle(bundle_id: &str) -> Result<bool> {
        let _pool = AutoreleasePool::new();
        unsafe {
            let workspace: *mut Object = msg_send![class!(NSWorkspace), sharedWorkspace];
            let running: *mut Object = msg_send![workspace, runningApplications];
            let count: usize = msg_send![running, count];
            for index in 0..count {
                let app: *mut Object = msg_send![running, objectAtIndex: index];
                let candidate: *mut Object = msg_send![app, bundleIdentifier];
                if nsstring_to_string(candidate).as_deref() == Some(bundle_id) {
                    let activated: bool = msg_send![
                        app,
                        activateWithOptions: NS_APPLICATION_ACTIVATE_IGNORING_OTHER_APPS
                    ];
                    return Ok(activated);
                }
            }
            Err(WorkspaceError::MacOs(format!(
                "could not find running application for bundle id {bundle_id}"
            )))
        }
    }

    pub fn running_pids_for_bundle(bundle_id: &str) -> Vec<i32> {
        let _pool = AutoreleasePool::new();
        let mut pids = Vec::new();
        unsafe {
            let workspace: *mut Object = msg_send![class!(NSWorkspace), sharedWorkspace];
            let running: *mut Object = msg_send![workspace, runningApplications];
            let count: usize = msg_send![running, count];
            for index in 0..count {
                let app: *mut Object = msg_send![running, objectAtIndex: index];
                let candidate: *mut Object = msg_send![app, bundleIdentifier];
                if nsstring_to_string(candidate).as_deref() == Some(bundle_id) {
                    let pid: i32 = msg_send![app, processIdentifier];
                    pids.push(pid);
                }
            }
        }
        pids
    }

    fn applescript_string(value: &str) -> String {
        let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
        format!("\"{escaped}\"")
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::RunningAppInfo;
    use crate::error::{Result, WorkspaceError};

    pub fn application_for_pid(_pid: i32) -> Option<RunningAppInfo> {
        None
    }

    pub fn launch_bundle(_bundle_id: &str) -> Result<bool> {
        Err(WorkspaceError::UnsupportedPlatform)
    }

    pub fn create_new_window(_bundle_id: &str, _process_name: &str) -> Result<bool> {
        Err(WorkspaceError::UnsupportedPlatform)
    }

    pub fn activate_bundle(_bundle_id: &str) -> Result<bool> {
        Err(WorkspaceError::UnsupportedPlatform)
    }

    pub fn running_pids_for_bundle(_bundle_id: &str) -> Vec<i32> {
        Vec::new()
    }
}

pub use imp::{
    activate_bundle, application_for_pid, create_new_window, launch_bundle, running_pids_for_bundle,
};
