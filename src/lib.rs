pub mod app_support;
pub mod capture;
pub mod cli;
pub mod configure;
pub mod error;
pub mod filter;
pub mod macos;
pub mod model;
pub mod output;
pub mod restore;
pub mod storage;

use cli::{Cli, Command};
use error::Result;
use storage::SnapshotStore;

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
                println!(
                    "saved '{}' with {} windows across {} displays\n{}",
                    snapshot.name,
                    snapshot.windows.len(),
                    snapshot.displays.len(),
                    path.display()
                );
            }
        }
        Command::Restore {
            name,
            dry_run,
            dev_mode,
        } => {
            let snapshot = store.load(&name)?;
            let report = restore::restore_workspace(
                &snapshot,
                restore::RestoreOptions { dry_run, dev_mode },
            )?;
            if cli.json {
                output::print_json(&report)?;
            } else {
                output::print_restore_report(&report);
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
                println!("deleted '{name}'");
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
    }

    Ok(())
}
