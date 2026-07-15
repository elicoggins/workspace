use std::process::ExitCode;

use tracing_subscriber::EnvFilter;
use workspace::{cli::Cli, run};

fn main() -> ExitCode {
    let cli = Cli::parse_args();

    let filter = if cli.verbose {
        EnvFilter::new("workspace=debug")
    } else {
        EnvFilter::new("workspace=warn")
    };

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .without_time()
        .with_target(false)
        .init();

    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            use workspace::error::WorkspaceError;
            use workspace::style;
            eprintln!("{} {error}", style::red("error:"));
            // Helpful follow-up hints for common failures.
            match &error {
                WorkspaceError::NotFound(_) => {
                    eprintln!("       try: workspace list");
                }
                WorkspaceError::AlreadyExists(name) => {
                    eprintln!("       try: workspace save {name} --force");
                }
                WorkspaceError::AccessibilityPermissionRequired => {
                    eprintln!("       open: System Settings → Privacy & Security → Accessibility");
                }
                _ => {}
            }
            ExitCode::from(error.exit_code())
        }
    }
}
