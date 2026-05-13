use std::collections::HashSet;

use dialoguer::{theme::ColorfulTheme, MultiSelect};
use serde::Serialize;

use crate::{
    error::{Result, WorkspaceError},
    model::{WindowSnapshot, WorkspaceSnapshot},
};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ConfigureReport {
    pub snapshot: String,
    pub enabled: usize,
    pub disabled: usize,
    pub windows: Vec<ConfiguredWindow>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ConfiguredWindow {
    pub index: usize,
    pub enabled: bool,
    pub app_name: String,
    pub title: Option<String>,
    pub bundle_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigureRequest {
    pub list: bool,
    pub enable: Vec<usize>,
    pub disable: Vec<usize>,
}

impl ConfigureRequest {
    fn has_explicit_changes(&self) -> bool {
        !self.enable.is_empty() || !self.disable.is_empty()
    }
}

pub fn configure_snapshot(
    snapshot: &mut WorkspaceSnapshot,
    request: ConfigureRequest,
) -> Result<bool> {
    if request.list {
        return Ok(false);
    }

    if request.has_explicit_changes() {
        apply_index_changes(snapshot, &request.enable, &request.disable)?;
        return Ok(true);
    }

    if snapshot.windows.is_empty() {
        return Ok(false);
    }

    let labels: Vec<_> = snapshot.windows.iter().map(window_label).collect();
    let defaults: Vec<_> = snapshot
        .windows
        .iter()
        .map(|window| window.enabled)
        .collect();
    let selected = MultiSelect::with_theme(&ColorfulTheme::default())
        .with_prompt("Select windows to restore")
        .items(&labels)
        .defaults(&defaults)
        .interact()
        .map_err(|error| WorkspaceError::Interaction(error.to_string()))?;

    apply_selection(snapshot, &selected);
    Ok(true)
}

pub fn apply_index_changes(
    snapshot: &mut WorkspaceSnapshot,
    enable: &[usize],
    disable: &[usize],
) -> Result<()> {
    for index in enable.iter().chain(disable) {
        if *index >= snapshot.windows.len() {
            return Err(WorkspaceError::Interaction(format!(
                "window index {index} is out of range; run 'workspace configure {} --list'",
                snapshot.name
            )));
        }
    }

    for index in enable {
        snapshot.windows[*index].enabled = true;
    }
    for index in disable {
        snapshot.windows[*index].enabled = false;
    }
    Ok(())
}

pub fn apply_selection(snapshot: &mut WorkspaceSnapshot, selected: &[usize]) {
    let selected: HashSet<_> = selected.iter().copied().collect();
    for (index, window) in snapshot.windows.iter_mut().enumerate() {
        window.enabled = selected.contains(&index);
    }
}

pub fn report(snapshot: &WorkspaceSnapshot) -> ConfigureReport {
    let windows: Vec<_> = snapshot
        .windows
        .iter()
        .enumerate()
        .map(|(index, window)| ConfiguredWindow {
            index,
            enabled: window.enabled,
            app_name: window.app_name.clone(),
            title: window.title.clone(),
            bundle_id: window.bundle_id.clone(),
        })
        .collect();
    ConfigureReport {
        snapshot: snapshot.name.clone(),
        enabled: windows.iter().filter(|window| window.enabled).count(),
        disabled: windows.iter().filter(|window| !window.enabled).count(),
        windows,
    }
}

pub fn window_label(window: &WindowSnapshot) -> String {
    let title = window.title.as_deref().unwrap_or("untitled");
    let bundle = window.bundle_id.as_deref().unwrap_or("unknown bundle");
    let tabs = match window.browser_tabs.len() {
        0 => String::new(),
        1 => "  1 tab".to_string(),
        count => format!("  {count} tabs"),
    };
    format!(
        "{:<24} x={:<5.0} y={:<5.0} w={:<5.0} h={:<5.0} {:<24} {}{}",
        window.app_name,
        window.frame.x,
        window.frame.y,
        window.frame.width,
        window.frame.height,
        bundle,
        title,
        tabs
    )
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::*;
    use crate::model::{Frame, HostInfo, WindowSnapshot, SNAPSHOT_VERSION};

    fn window(app_name: &str) -> WindowSnapshot {
        WindowSnapshot {
            window_id: 1,
            app_name: app_name.to_string(),
            process_name: app_name.to_string(),
            bundle_id: Some(format!("dev.example.{app_name}")),
            pid: 42,
            title: Some("main".to_string()),
            frame: Frame {
                x: 1.0,
                y: 2.0,
                width: 300.0,
                height: 400.0,
            },
            display_id: None,
            display_frame: None,
            display_relative_frame: None,
            z_order: Some(0),
            fullscreen: false,
            minimized: false,
            enabled: true,
            browser_tabs: Vec::new(),
        }
    }

    fn snapshot() -> WorkspaceSnapshot {
        WorkspaceSnapshot {
            version: SNAPSHOT_VERSION,
            name: "test".to_string(),
            created_at: Utc.with_ymd_and_hms(2026, 5, 12, 15, 30, 0).unwrap(),
            host: HostInfo {
                hostname: "host".to_string(),
                os: "macos".to_string(),
                arch: "aarch64".to_string(),
            },
            displays: Vec::new(),
            windows: vec![window("Code"), window("Chrome"), window("Notes")],
        }
    }

    #[test]
    fn apply_selection_disables_unselected_windows() {
        let mut snapshot = snapshot();

        apply_selection(&mut snapshot, &[0, 2]);

        assert!(snapshot.windows[0].enabled);
        assert!(!snapshot.windows[1].enabled);
        assert!(snapshot.windows[2].enabled);
        let report = report(&snapshot);
        assert_eq!(report.enabled, 2);
        assert_eq!(report.disabled, 1);
    }

    #[test]
    fn apply_index_changes_enables_and_disables_requested_windows() {
        let mut snapshot = snapshot();
        snapshot.windows[0].enabled = false;

        apply_index_changes(&mut snapshot, &[0], &[1]).unwrap();

        assert!(snapshot.windows[0].enabled);
        assert!(!snapshot.windows[1].enabled);
        assert!(snapshot.windows[2].enabled);
    }

    #[test]
    fn apply_index_changes_rejects_out_of_range_indexes() {
        let mut snapshot = snapshot();

        let error = apply_index_changes(&mut snapshot, &[], &[99]).unwrap_err();

        assert!(error.to_string().contains("out of range"));
    }

    #[test]
    fn label_includes_app_geometry_and_title() {
        let label = window_label(&window("Code"));

        assert!(label.contains("Code"));
        assert!(label.contains("x=1"));
        assert!(label.contains("main"));
    }
}
