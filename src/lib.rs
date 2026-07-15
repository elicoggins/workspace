pub mod app_support;
pub mod capture;
pub mod cli;
pub mod configure;
pub mod error;
pub mod execute;
pub mod filter;
pub mod macos;
pub mod model;
pub mod output;
pub mod plan;
pub mod selftest;
pub mod storage;
pub mod style;
pub mod verify;
pub mod world;

use cli::{Cli, Command, ModeArg};
use error::Result;
use storage::SnapshotStore;

fn resolve_mode(mode: ModeArg, destructive: bool) -> plan::RestoreMode {
    if destructive {
        plan::RestoreMode::Destructive
    } else {
        mode.into()
    }
}

pub fn run(cli: Cli) -> Result<()> {
    let store = SnapshotStore::open_default()?;

    match cli.command {
        Command::Save { name, force } => {
            let snapshot = capture::capture_workspace(&name)?;
            let path = store.save(&snapshot, force)?;
            if cli.json {
                output::print_json(&serde_json::json!({
                    "saved": snapshot.name,
                    "path": path,
                    "display_count": snapshot.displays.len(),
                    "window_count": snapshot.windows.len()
                }))?;
            } else {
                println!("{} {}", style::green("saved"), style::bold(&snapshot.name));
                println!("  windows   {}", snapshot.windows.len());
                println!("  displays  {}", snapshot.displays.len());
                println!("  path      {}", style::dim(&path.display().to_string()));
            }
        }
        Command::Plan {
            name,
            dev_mode,
            mode,
            destructive,
        } => {
            let snapshot = store.load(&name)?;
            let resolved_mode = resolve_mode(mode, destructive);
            let plan = world::build_plan(&snapshot, resolved_mode, dev_mode)?;
            if cli.json {
                output::print_json(&plan)?;
            } else {
                output::print_restore_plan(&plan);
            }
        }
        Command::Verify { name } => {
            let snapshot = store.load(&name)?;
            let report = world::verify_workspace(&snapshot)?;
            if cli.json {
                output::print_json(&report)?;
            } else {
                output::print_verify_report(&report);
            }
        }
        Command::Doctor => {
            let report = world::doctor()?;
            if cli.json {
                output::print_json(&report)?;
            } else {
                output::print_doctor_report(&report);
            }
        }
        Command::Selftest { live } => {
            let report = selftest::run(live)?;
            if cli.json {
                output::print_json(&report)?;
            } else {
                output::print_selftest_report(&report);
            }
            let failed = report.failed();
            if failed > 0 {
                return Err(error::WorkspaceError::SelftestFailed {
                    failed,
                    total: report.checks.len(),
                });
            }
        }
        Command::List => {
            let entries = store.list()?;
            if cli.json {
                output::print_json(&entries)?;
            } else {
                output::print_snapshot_list(&entries);
            }
        }
        Command::Delete { name } => {
            store.delete(&name)?;
            if cli.json {
                output::print_json(&serde_json::json!({ "deleted": name }))?;
            } else {
                println!("{} {}", style::yellow("deleted"), style::bold(&name));
            }
        }
        Command::Inspect { name } => {
            let snapshot = store.load(&name)?;
            if cli.json {
                output::print_json(&snapshot)?;
            } else {
                output::print_snapshot_summary(&snapshot);
            }
        }
        Command::Configure {
            name,
            list,
            enable,
            disable,
        } => {
            let mut snapshot = store.load(&name)?;
            let changed = configure::configure_snapshot(
                &mut snapshot,
                configure::ConfigureRequest {
                    list,
                    enable,
                    disable,
                },
            )?;
            let report = configure::report(&snapshot);
            let path = if changed {
                Some(store.save(&snapshot, true)?)
            } else {
                None
            };
            if cli.json {
                output::print_json(&serde_json::json!({
                    "configured": report.snapshot,
                    "path": path,
                    "enabled": report.enabled,
                    "disabled": report.disabled,
                    "changed": changed,
                    "windows": report.windows
                }))?;
            } else {
                output::print_configure_report(&report, changed);
                if let Some(path) = path {
                    println!("{}", path.display());
                }
            }
        }
        Command::Restore {
            name,
            dry_run,
            dev_mode,
            mode,
            destructive,
            converge,
        } => {
            run_restore(
                &store,
                &name,
                dry_run,
                dev_mode,
                mode,
                destructive,
                converge,
                cli.json,
            )?;
        }
        Command::Diff {
            name,
            dev_mode,
            mode,
            destructive,
        } => {
            let snapshot = store.load(&name)?;
            let resolved_mode = resolve_mode(mode, destructive);
            let plan = world::build_plan(&snapshot, resolved_mode, dev_mode)?;
            let verify = world::verify_workspace(&snapshot)?;
            if cli.json {
                output::print_json(&serde_json::json!({
                    "plan": plan,
                    "verify": verify,
                }))?;
            } else {
                output::print_restore_plan(&plan);
                println!();
                output::print_verify_report(&verify);
            }
        }
        Command::Completions { shell } => {
            use clap::CommandFactory;
            let mut cmd = Cli::command();
            let bin_name = cmd.get_name().to_string();
            clap_complete::generate(shell, &mut cmd, bin_name, &mut std::io::stdout());
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_restore(
    store: &SnapshotStore,
    name: &str,
    dry_run: bool,
    dev_mode: bool,
    mode: ModeArg,
    destructive: bool,
    converge: u32,
    json: bool,
) -> Result<()> {
    let snapshot = store.load(name)?;
    let resolved_mode = resolve_mode(mode, destructive);

    // Fail fast with a clear error instead of a journal full of cryptic AX
    // failures when the permission is missing.
    #[cfg(target_os = "macos")]
    if !dry_run {
        macos::accessibility::ensure_trusted()?;
    }

    let max_iters = converge.max(1);
    let mut journals: Vec<execute::ExecutionJournal> = Vec::new();
    let mut final_verify = None;

    for iter in 0..max_iters {
        let plan = world::build_plan(&snapshot, resolved_mode, dev_mode)?;
        let actionable = plan
            .operations
            .iter()
            .any(|op| !matches!(op.kind, plan::OperationKind::Skip { .. }));
        let options = execute::ExecuteOptions { dry_run };

        let journal = if dry_run {
            // Dry-run: use the planner output but never touch macOS. Drive a
            // SimulatedExecutor seeded with the observed world so the journal
            // shows the would-be ops without mutating state.
            let world = world::observe_world()?;
            let mut sim = execute::SimulatedExecutor::new(world);
            execute::execute_plan(&snapshot, &plan, &mut sim, options)
        } else {
            #[cfg(target_os = "macos")]
            {
                let world = world::observe_world()?;
                let mut exec = execute::MacOsExecutor::new(world);
                execute::execute_plan(&snapshot, &plan, &mut exec, options)
            }
            #[cfg(not(target_os = "macos"))]
            {
                let world = world::observe_world()?;
                let mut sim = execute::SimulatedExecutor::new(world);
                execute::execute_plan(&snapshot, &plan, &mut sim, options)
            }
        };

        journals.push(journal);
        let verify = world::verify_workspace(&snapshot)?;
        let accuracy = verify.accuracy;
        final_verify = Some(verify);

        // Converge: stop early on 100% match, or when the plan had nothing
        // actionable — re-planning an all-skip world can never change it.
        if !actionable || accuracy >= 0.999 {
            break;
        }
        if iter + 1 >= max_iters {
            break;
        }
    }

    // Replay saved stacking order once geometry has settled.
    if !dry_run {
        world::replay_z_order(&snapshot);
    }

    if json {
        output::print_json(&serde_json::json!({
            "snapshot": name,
            "iterations": journals.len(),
            "journals": journals,
            "verify": final_verify,
        }))?;
    } else {
        for (i, j) in journals.iter().enumerate() {
            if journals.len() > 1 {
                println!(
                    "{}",
                    style::bold(&format!("iteration {}/{}", i + 1, journals.len()))
                );
            }
            output::print_journal(j);
            println!();
        }
        if let Some(v) = &final_verify {
            output::print_verify_report(v);
        }
    }
    Ok(())
}
