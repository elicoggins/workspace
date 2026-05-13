use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "workspace")]
#[command(about = "Save and restore macOS desktop window workspaces", version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,

    #[arg(long, global = true, help = "Emit machine-readable JSON output")]
    pub json: bool,

    #[arg(short, long, global = true, help = "Enable verbose diagnostics")]
    pub verbose: bool,
}

impl Cli {
    pub fn parse_args() -> Self {
        Self::parse()
    }
}

#[derive(Debug, Subcommand)]
pub enum Command {
    #[command(about = "Capture the current visible window layout")]
    Save {
        #[arg(help = "Snapshot name")]
        name: String,

        #[arg(long, help = "Overwrite an existing snapshot")]
        force: bool,
    },

    #[command(about = "Restore a saved window layout")]
    Restore {
        #[arg(help = "Snapshot name")]
        name: String,

        #[arg(long, help = "Show planned changes without moving windows")]
        dry_run: bool,

        #[arg(
            long,
            help = "Protect the current development host from destructive lifecycle actions"
        )]
        dev_mode: bool,
    },

    #[command(about = "List saved snapshots")]
    List,

    #[command(about = "Delete a saved snapshot")]
    Delete {
        #[arg(help = "Snapshot name")]
        name: String,
    },

    #[command(about = "Inspect a saved snapshot")]
    Inspect {
        #[arg(help = "Snapshot name")]
        name: String,
    },

    #[command(about = "Enable or disable windows in a saved workspace")]
    Configure {
        #[arg(help = "Snapshot name")]
        name: String,

        #[arg(long, help = "List configurable windows without changing the snapshot")]
        list: bool,

        #[arg(
            long,
            value_name = "INDEX",
            help = "Enable a window by its configure index"
        )]
        enable: Vec<usize>,

        #[arg(
            long,
            value_name = "INDEX",
            help = "Disable a window by its configure index"
        )]
        disable: Vec<usize>,
    },
}
