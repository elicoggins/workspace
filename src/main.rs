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
            eprintln!("workspace: {error}");
            ExitCode::from(error.exit_code())
        }
    }
}
