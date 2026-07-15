use clap::{Parser, Subcommand, ValueEnum};
use clap_complete::Shell;

use crate::plan::RestoreMode;

const TOP_AFTER_HELP: &str = "\
Examples:
  workspace save coding              Capture the current window layout
  workspace apply coding             Restore it (planner-driven)
  workspace apply coding --dry-run   Preview without touching the system
  workspace diff coding              See what's different and what would change
  workspace list                     Show all saved workspaces
  workspace doctor                   Check permissions and environment

Run 'workspace <COMMAND> --help' for command-specific options.";

const APPLY_AFTER_HELP: &str = "\
Examples:
  workspace apply coding
  workspace apply coding --converge 3            Retry up to 3 times
  workspace apply coding --mode reconcile        Minimize extras
  workspace apply coding --dry-run --json        Inspect the plan as JSON";

#[derive(Debug, Parser)]
#[command(
    name = "workspace",
    about = "Save and restore macOS desktop window workspaces",
    long_about = "Capture the current window layout (apps, geometry, Chrome tabs, displays) to a \
JSON snapshot, then restore it deterministically. Requires macOS Accessibility permission for \
restore commands.",
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

    /// Enable verbose tracing (RUST_LOG=workspace=debug)
    #[arg(short, long, global = true)]
    pub verbose: bool,
}

impl Cli {
    pub fn parse_args() -> Self {
        Self::parse()
    }
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

    /// Apply a snapshot using the planner + executor (recommended)
    #[command(after_help = APPLY_AFTER_HELP)]
    Apply {
        /// Snapshot name
        name: String,

        /// Show the plan without touching the system
        #[arg(long)]
        dry_run: bool,

        /// Protect VS Code / Cursor from destructive lifecycle actions
        #[arg(long)]
        dev_mode: bool,

        /// Restore policy (safe | reconcile | destructive)
        #[arg(long, value_enum, default_value = "safe")]
        mode: ModeArg,

        /// Shortcut for --mode destructive
        #[arg(long)]
        destructive: bool,

        /// Max plan→execute→verify iterations (1 = no retry)
        #[arg(long, value_name = "N", default_value_t = 1)]
        converge: u32,
    },

    /// Show what would change AND how far the world is from the snapshot
    Diff {
        /// Snapshot name
        name: String,

        /// Protect VS Code / Cursor from destructive lifecycle actions
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

        /// Protect VS Code / Cursor from destructive lifecycle actions
        #[arg(long)]
        dev_mode: bool,

        /// Restore policy (safe | reconcile | destructive)
        #[arg(long, value_enum, default_value = "safe")]
        mode: ModeArg,

        /// Shortcut for --mode destructive
        #[arg(long)]
        destructive: bool,
    },

    /// Compare the live world to a snapshot and report accuracy
    Verify {
        /// Snapshot name
        name: String,
    },

    /// Legacy single-pass restore (equivalent to `apply --converge 1`)
    Restore {
        /// Snapshot name
        name: String,

        /// Show planned changes without moving windows
        #[arg(long)]
        dry_run: bool,

        /// Protect VS Code / Cursor from destructive lifecycle actions
        #[arg(long)]
        dev_mode: bool,

        /// Restore policy (safe | reconcile | destructive)
        #[arg(long, value_enum, default_value = "safe")]
        mode: ModeArg,

        /// Shortcut for --mode destructive
        #[arg(long)]
        destructive: bool,
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

    /// Print a shell completion script
    Completions {
        /// Target shell (bash | zsh | fish | powershell | elvish)
        #[arg(value_enum)]
        shell: Shell,
    },
}
