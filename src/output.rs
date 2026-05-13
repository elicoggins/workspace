use serde::Serialize;

use crate::{
    configure::ConfigureReport,
    error::Result,
    model::{RestoreReport, RestoreStatus, SnapshotListEntry, WorkspaceSnapshot},
};

pub fn print_json<T: Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

pub fn print_snapshot_list(entries: &[SnapshotListEntry]) {
    if entries.is_empty() {
        println!("no saved workspaces");
        return;
    }

    for entry in entries {
        println!(
            "{:<24} {}  {:>2} displays  {:>3} windows",
            entry.name,
            entry.created_at.to_rfc3339(),
            entry.display_count,
            entry.window_count
        );
    }
}

pub fn print_snapshot_summary(snapshot: &WorkspaceSnapshot) {
    println!("workspace: {}", snapshot.name);
    println!("created:   {}", snapshot.created_at.to_rfc3339());
    println!(
        "host:      {} ({}/{})",
        snapshot.host.hostname, snapshot.host.os, snapshot.host.arch
    );
    println!("displays:  {}", snapshot.displays.len());
    println!("windows:   {}", snapshot.windows.len());
    println!();

    for window in &snapshot.windows {
        let title = window.title.as_deref().unwrap_or("untitled");
        let bundle = window.bundle_id.as_deref().unwrap_or("unknown bundle");
        let enabled = if window.enabled { "on" } else { "off" };
        println!(
            "{:<4} {:<28} {:<36} x={:<6.0} y={:<6.0} w={:<6.0} h={:<6.0} {}",
            enabled,
            window.app_name,
            bundle,
            window.frame.x,
            window.frame.y,
            window.frame.width,
            window.frame.height,
            title
        );
    }
}

pub fn print_configure_report(report: &ConfigureReport, changed: bool) {
    let verb = if changed {
        "configured"
    } else {
        "configuration"
    };
    println!(
        "{} '{}'  enabled={} disabled={}",
        verb, report.snapshot, report.enabled, report.disabled
    );
    for window in &report.windows {
        let status = if window.enabled { "on" } else { "off" };
        let title = window.title.as_deref().unwrap_or("untitled");
        println!(
            "{:<3} {:<4} {:<24} {}",
            window.index, status, window.app_name, title
        );
    }
}

pub fn print_restore_report(report: &RestoreReport) {
    let planned = if report.dry_run {
        "planned"
    } else {
        "restored"
    };
    println!(
        "{} '{}'  restored={} skipped={} failed={}",
        planned, report.snapshot, report.restored, report.skipped, report.failed
    );

    for action in &report.actions {
        let status = match action.status {
            RestoreStatus::Planned => "plan",
            RestoreStatus::Restored => "ok",
            RestoreStatus::Skipped => "skip",
            RestoreStatus::Failed => "fail",
        };
        let title = action.title.as_deref().unwrap_or("untitled");
        let message = action.message.as_deref().unwrap_or("");
        println!(
            "{:<5} {:<24} x={:<6.0} y={:<6.0} w={:<6.0} h={:<6.0} {} {}",
            status,
            action.app_name,
            action.target_frame.x,
            action.target_frame.y,
            action.target_frame.width,
            action.target_frame.height,
            title,
            message
        );
    }
}
