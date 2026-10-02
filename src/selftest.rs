//! Checks against the current desktop. Moving windows requires `--live`.

use serde::{Deserialize, Serialize};

use crate::{capture, error::Result, model::WorkspaceSnapshot, plan::RestoreMode, world};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelftestCheck {
    pub name: String,
    pub passed: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SelftestReport {
    pub checks: Vec<SelftestCheck>,
}

impl SelftestReport {
    fn check(&mut self, name: &str, passed: bool, detail: impl Into<String>) {
        self.checks.push(SelftestCheck {
            name: name.to_string(),
            passed,
            detail: detail.into(),
        });
    }

    pub fn failed(&self) -> usize {
        self.checks.iter().filter(|check| !check.passed).count()
    }
}

/// Check capture, planning, and verification. With `live`, also move a window
/// and restore the snapshot.
pub fn run(live: bool) -> Result<SelftestReport> {
    let mut report = SelftestReport::default();

    let doctor = world::doctor()?;
    report.check(
        "data dir writable",
        doctor.data_dir_writable,
        doctor.data_dir.clone(),
    );
    report.check(
        "accessibility granted",
        doctor.accessibility_trusted,
        if doctor.accessibility_trusted {
            "AXIsProcessTrusted = true"
        } else {
            "grant in System Settings → Privacy & Security → Accessibility"
        },
    );
    report.check(
        "displays detected",
        doctor.display_count > 0,
        format!("{} display(s)", doctor.display_count),
    );

    let world_state = world::observe_world()?;
    report.check(
        "world observation",
        !world_state.windows.is_empty(),
        format!("{} live window(s)", world_state.windows.len()),
    );

    let snapshot = capture::capture_workspace("selftest")?;
    report.check(
        "capture",
        !snapshot.windows.is_empty(),
        format!("{} window(s) captured", snapshot.windows.len()),
    );
    let round_trip = serde_json::to_string(&snapshot)
        .ok()
        .and_then(|json| serde_json::from_str::<WorkspaceSnapshot>(&json).ok())
        .map(|copy| copy == snapshot)
        .unwrap_or(false);
    report.check(
        "snapshot JSON round-trip",
        round_trip,
        "serialize → parse → equal",
    );

    // A fresh capture should match unless the desktop changed in the meantime.
    let plan = world::build_plan(&snapshot, RestoreMode::Safe, false)?;
    report.check(
        "plan builds",
        true,
        format!("{} op(s)", plan.operations.len()),
    );
    let verify = world::verify_workspace(&snapshot)?;
    report.check(
        "fresh snapshot matches the current layout",
        verify.converged,
        format!(
            "accuracy {:.1}% ({}/{} restorable)",
            verify.accuracy * 100.0,
            verify.matched,
            verify.total - verify.skipped
        ),
    );

    if live {
        run_live_check(&mut report, &snapshot)?;
    }

    Ok(report)
}

#[cfg(target_os = "macos")]
fn run_live_check(report: &mut SelftestReport, snapshot: &WorkspaceSnapshot) -> Result<()> {
    use std::{thread, time::Duration};

    use crate::execute::{execute_plan, ExecuteOptions, Executor, MacOsExecutor};
    use crate::model::Frame;
    use crate::plan::OperationKind;

    crate::macos::accessibility::ensure_trusted()?;

    let observed = world::observe_world()?;
    let plan = world::plan_for_world(snapshot, &observed, RestoreMode::Safe, false);
    let Some((saved_index, live_pid, live_window_id, target_frame)) =
        plan.operations.iter().find_map(|op| match &op.kind {
            OperationKind::Reposition {
                live_pid,
                live_window_id,
                target_frame,
            } => op
                .saved_window_index
                .map(|index| (index, *live_pid, *live_window_id, *target_frame)),
            _ => None,
        })
    else {
        report.check(
            "live move (+40px)",
            false,
            "no repositionable window found to exercise",
        );
        return Ok(());
    };
    let saved = &snapshot.windows[saved_index];

    let shifted = Frame {
        x: target_frame.x + 40.0,
        ..target_frame
    };
    let mut executor = MacOsExecutor::new(observed);
    let outcome = executor.reposition(live_pid, live_window_id, saved, shifted)?;
    thread::sleep(Duration::from_millis(300));
    let after = world::observe_world()?;
    let moved = after.windows.iter().any(|window| {
        window.pid == live_pid
            && (window.frame.x - shifted.x).abs() <= 3.0
            && (window.frame.y - shifted.y).abs() <= 3.0
    });
    report.check(
        "live move (+40px)",
        moved,
        format!("{} ({})", saved.app_name, outcome.message),
    );

    let observed_back = world::observe_world()?;
    let plan_back = world::plan_for_world(snapshot, &observed_back, RestoreMode::Safe, false);
    let mut executor = MacOsExecutor::new(observed_back);
    let journal = execute_plan(
        snapshot,
        &plan_back,
        &mut executor,
        ExecuteOptions::default(),
    );
    thread::sleep(Duration::from_millis(300));
    let verify = world::verify_workspace(snapshot)?;
    report.check(
        "live restore back",
        verify.converged && journal.succeeded(&plan_back),
        format!(
            "accuracy {:.1}% after {} op(s)",
            verify.accuracy * 100.0,
            journal.entries.len()
        ),
    );
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn run_live_check(report: &mut SelftestReport, _snapshot: &WorkspaceSnapshot) -> Result<()> {
    report.check("live checks", false, "only supported on macOS");
    Ok(())
}
