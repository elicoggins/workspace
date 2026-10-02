use clap::{Parser, Subcommand, ValueEnum};
use clap_complete::Shell;

use crate::plan::RestoreMode;

const TOP_AFTER_HELP: &str = "\
Examples:
  workspace save coding              Capture the current window layout
  workspace restore coding           Restore the saved layout
  workspace restore coding --dry-run Preview restore operations
  workspace diff coding              Compare the snapshot with current windows
  workspace list                     Show all saved workspaces
  workspace doctor                   Check permissions and environment

Run 'workspace <COMMAND> --help' for command-specific options.";

const RESTORE_AFTER_HELP: &str = "\
Examples:
  workspace restore coding
  workspace restore coding --converge 3          Allow up to 3 restore passes
  workspace restore coding --mode reconcile      Minimize extra windows
  workspace restore coding --dry-run --json      Preview operations as JSON";

#[derive(Debug, Parser)]
#[command(
    name = "workspace",
    about = "Save and restore macOS window layouts",
    long_about = "Save named window layouts, including displays and Chromium browser tabs.\n\n\
Restoring requires Accessibility permission. Screen Recording exposes window titles \
for matching and tab capture.",
    after_help = TOP_AFTER_HELP,
    version,
    arg_required_else_help = true,
    disable_help_subcommand = true,
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,

    /// Emit machine-readable JSON output
    #[arg(long, global = true)]
    pub json: bool,

    /// Write debug logs to stderr
    #[arg(short, long, global = true)]
    pub verbose: bool,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, ValueEnum)]
pub enum ModeArg {
    /// Only reposition, launch, and create windows. Never minimize or close.
    Safe,
    /// May minimize extra windows of apps being restored.
    Reconcile,
    /// May close extra windows of apps being restored.
    Destructive,
}

impl From<ModeArg> for RestoreMode {
    fn from(value: ModeArg) -> Self {
        match value {
            ModeArg::Safe => RestoreMode::Safe,
            ModeArg::Reconcile => RestoreMode::Reconcile,
            ModeArg::Destructive => RestoreMode::Destructive,
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Capture the current visible window layout
    Save {
        /// Snapshot name (letters, digits, '.', '_', '-')
        name: String,

        /// Overwrite if a snapshot with this name already exists
        #[arg(long)]
        force: bool,
    },

    /// Restore a saved window layout
    #[command(after_help = RESTORE_AFTER_HELP, alias = "apply")]
    Restore {
        /// Snapshot name
        name: String,

        /// Preview restore operations without changing windows
        #[arg(long)]
        dry_run: bool,

        /// Skip launching or cleaning up VS Code and Cursor
        #[arg(long)]
        dev_mode: bool,

        /// Restore policy (safe | reconcile | destructive)
        #[arg(long, value_enum, default_value = "safe")]
        mode: ModeArg,

        /// Shortcut for --mode destructive
        #[arg(long)]
        destructive: bool,

        /// Maximum number of restore passes
        #[arg(long, value_name = "N", default_value_t = 1)]
        converge: u32,
    },

    /// Compare current windows with a snapshot and show the restore plan
    Diff {
        /// Snapshot name
        name: String,

        /// Skip launching or cleaning up VS Code and Cursor
        #[arg(long)]
        dev_mode: bool,

        /// Restore policy (safe | reconcile | destructive)
        #[arg(long, value_enum, default_value = "safe")]
        mode: ModeArg,

        /// Shortcut for --mode destructive
        #[arg(long)]
        destructive: bool,
    },

    /// Show the restore plan without executing it
    Plan {
        /// Snapshot name
        name: String,

        /// Skip launching or cleaning up VS Code and Cursor
        #[arg(long)]
        dev_mode: bool,

        /// Restore policy (safe | reconcile | destructive)
        #[arg(long, value_enum, default_value = "safe")]
        mode: ModeArg,

        /// Shortcut for --mode destructive
        #[arg(long)]
        destructive: bool,
    },

    /// Check window matches, visibility, and geometry against a snapshot
    Verify {
        /// Snapshot name
        name: String,
    },

    /// List saved snapshots
    List,

    /// Print a snapshot's contents
    Inspect {
        /// Snapshot name
        name: String,
    },

    /// Delete a snapshot
    Delete {
        /// Snapshot name
        name: String,
    },

    /// Enable or disable specific windows in a snapshot
    Configure {
        /// Snapshot name
        name: String,

        /// List windows with their configure indexes; don't modify
        #[arg(long)]
        list: bool,

        /// Enable a window by its configure index (repeatable)
        #[arg(long, value_name = "INDEX")]
        enable: Vec<usize>,

        /// Disable a window by its configure index (repeatable)
        #[arg(long, value_name = "INDEX")]
        disable: Vec<usize>,
    },

    /// Check environment: data dir, displays, Accessibility permission
    Doctor,

    /// Check capture, planning, and verification on this Mac
    Selftest {
        /// Also move a window briefly, then restore the snapshot
        #[arg(long)]
        live: bool,
    },

    /// Print a shell completion script
    Completions {
        /// Target shell (bash | zsh | fish | powershell | elvish)
        #[arg(value_enum)]
        shell: Shell,
    },
}
