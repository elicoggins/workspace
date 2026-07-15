use serde::Serialize;

use crate::{
    configure::ConfigureReport,
    error::Result,
    model::{SnapshotListEntry, WorkspaceSnapshot},
    style,
};

pub fn print_json<T: Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

pub fn print_snapshot_list(entries: &[SnapshotListEntry]) {
    if entries.is_empty() {
        println!("no saved workspaces yet");
        println!("  try: workspace save <name>");
        return;
    }

    println!(
        "{}",
        style::bold(&format!(
            "{:<24} {:<25} {:>3} {:>4}",
            "NAME", "SAVED", "DSP", "WIN"
        ))
    );
    for entry in entries {
        println!(
            "{:<24} {:<25} {:>3} {:>4}",
            entry.name,
            entry.created_at.format("%Y-%m-%d %H:%M:%S %z"),
            entry.display_count,
            entry.window_count
        );
    }
}

pub fn print_snapshot_summary(snapshot: &WorkspaceSnapshot) {
    println!("{} {}", style::bold("workspace:"), snapshot.name);
    println!(
        "  created   {}",
        snapshot.created_at.format("%Y-%m-%d %H:%M:%S %z")
    );
    println!(
        "  host      {} ({}/{})",
        snapshot.host.hostname, snapshot.host.os, snapshot.host.arch
    );
    println!("  displays  {}", snapshot.displays.len());
    println!("  windows   {}", snapshot.windows.len());
    println!();
    println!(
        "{}",
        style::bold(&format!(
            "{:<4} {:<28} {:<36} {:>6} {:>6} {:>6} {:>6}  TITLE",
            "EN", "APP", "BUNDLE", "X", "Y", "W", "H"
        ))
    );
    for window in &snapshot.windows {
        let title = window.title.as_deref().unwrap_or("(untitled)");
        let bundle = window.bundle_id.as_deref().unwrap_or("(unknown)");
        let enabled = if window.enabled { "on" } else { "off" };
        println!(
            "{:<4} {:<28} {:<36} {:>6.0} {:>6.0} {:>6.0} {:>6.0}  {}",
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
        style::green("configured")
    } else {
        style::dim("unchanged")
    };
    println!(
        "{} {}  ({} enabled, {} disabled)",
        verb,
        style::bold(&report.snapshot),
        report.enabled,
        report.disabled
    );
    println!();
    println!(
        "{}",
        style::bold(&format!("{:>3} {:<4} {:<28} TITLE", "IDX", "EN", "APP"))
    );
    for window in &report.windows {
        let status = if window.enabled { "on" } else { "off" };
        let title = window.title.as_deref().unwrap_or("(untitled)");
        println!(
            "{:>3} {:<4} {:<28} {}",
            window.index, status, window.app_name, title
        );
    }
}

pub fn print_restore_plan(plan: &crate::plan::RestorePlan) {
    let summary = plan.summary();
    println!(
        "{} mode={:?}  ops={}",
        style::bold("plan:"),
        plan.mode,
        plan.operations.len()
    );
    println!(
        "  reposition={} launch={} create={} chrome={} minimize={} close={} skip={}",
        summary.reposition,
        summary.launch,
        summary.create,
        summary.chrome,
        summary.minimize,
        summary.close,
        summary.skip,
    );
    if !plan.unmatched_saved.is_empty() {
        println!(
            "  {} indexes: {:?}",
            style::yellow("unmatched saved"),
            plan.unmatched_saved
        );
    }
    if plan.operations.is_empty() {
        println!();
        println!("{}", style::dim("(nothing to do — world matches snapshot)"));
        return;
    }
    println!();
    for op in &plan.operations {
        let bundle = op.bundle_id.as_deref().unwrap_or("(unknown)");
        println!(
            "  {:<10} {:<28} {:<36}  {}",
            op.kind.short_name(),
            op.app_name,
            bundle,
            style::dim(&op.rationale),
        );
        if let Some(score) = &op.match_score {
            println!(
                "             {}",
                style::dim(&format!("score: {}", score.explain()))
            );
        }
    }
}

pub fn print_verify_report(report: &crate::verify::VerifyReport) {
    let pct = report.accuracy * 100.0;
    let pct_str = format!("{pct:.1}%");
    let pct_styled = if pct >= 99.9 {
        style::green(&pct_str)
    } else if pct >= 80.0 {
        style::yellow(&pct_str)
    } else {
        style::red(&pct_str)
    };
    let restorable = report.total - report.skipped;
    println!(
        "{} {}  {} matched ({}/{} restorable, {} skipped, drift mean {:.1}px, max {:.1}px)",
        style::bold("verify:"),
        report.snapshot,
        pct_styled,
        report.matched,
        restorable,
        report.skipped,
        report.mean_geometry_delta,
        report.max_geometry_delta,
    );
    if report.unmatched > 0 {
        println!(
            "  {} unmatched:",
            style::yellow(&report.unmatched.to_string())
        );
        for entry in report
            .windows
            .iter()
            .filter(|e| !e.matched && e.skipped_reason.is_none())
        {
            println!(
                "    - {} ({})",
                entry.app_name,
                entry.bundle_id.as_deref().unwrap_or("?")
            );
        }
    }
}

pub fn print_doctor_report(report: &crate::world::DoctorReport) {
    let line = |ok: bool, label: &str, value: &str| {
        let tag = if ok {
            style::tag_ok()
        } else {
            style::tag_fail()
        };
        println!("  {tag} {label:<14} {value}");
    };
    println!("{}", style::bold("doctor:"));
    line(report.data_dir_writable, "data dir", &report.data_dir);
    line(
        report.accessibility_trusted,
        "accessibility",
        if report.accessibility_trusted {
            "granted"
        } else {
            "not granted (System Settings → Privacy & Security → Accessibility)"
        },
    );
    line(
        report.display_count > 0,
        "displays",
        &report.display_count.to_string(),
    );
    line(
        true,
        "supported apps",
        &report.supported_bundles.len().to_string(),
    );
    if !report.warnings.is_empty() {
        println!();
        println!("{}", style::yellow("warnings:"));
        for w in &report.warnings {
            println!("  - {w}");
        }
    }
}

pub fn print_selftest_report(report: &crate::selftest::SelftestReport) {
    let failed = report.failed();
    let verdict = if failed == 0 {
        style::green("all checks passed")
    } else {
        style::red(&format!("{failed} check(s) failed"))
    };
    println!(
        "{} {} ({}/{})",
        style::bold("selftest:"),
        verdict,
        report.checks.len() - failed,
        report.checks.len()
    );
    for check in &report.checks {
        let tag = if check.passed {
            style::tag_ok()
        } else {
            style::tag_fail()
        };
        println!("  {} {:<34} {}", tag, check.name, style::dim(&check.detail));
    }
}

pub fn print_journal(journal: &crate::execute::ExecutionJournal) {
    let counts = journal.counts();
    println!(
        "{} {}  {} ops in {} ms",
        style::bold("restore:"),
        journal.snapshot,
        journal.entries.len(),
        journal.duration_ms
    );
    println!(
        "  {} {}  {} {}  {} {}  {} {}",
        style::green("ok"),
        counts.success,
        style::yellow("part"),
        counts.partial,
        style::dim("skip"),
        counts.skipped,
        style::red("fail"),
        counts.failed,
    );
    if journal.entries.is_empty() {
        println!();
        println!("{}", style::dim("(no operations)"));
        return;
    }
    for (i, e) in journal.entries.iter().enumerate() {
        let tag = match e.status {
            crate::execute::JournalStatus::Success => style::tag_ok(),
            crate::execute::JournalStatus::PartialSuccess => style::tag_part(),
            crate::execute::JournalStatus::Skipped => style::tag_skip(),
            crate::execute::JournalStatus::Failed => style::tag_fail(),
        };
        let attempts = if e.attempts > 1 {
            format!(" x{}", e.attempts)
        } else {
            String::new()
        };
        println!(
            "  {:>3} {} {:<12} {:<24} {} {}",
            i + 1,
            tag,
            e.op,
            e.app_name,
            style::dim(&format!("{}ms{}", e.duration_ms, attempts)),
            e.message,
        );
    }
}
