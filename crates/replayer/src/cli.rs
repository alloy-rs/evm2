use crate::{error::Result, input, runner};
use std::{error::Error as _, path::PathBuf, process::ExitCode};

pub(crate) fn main() -> ExitCode {
    if let Err(error) = run() {
        eprintln!("{error}");
        let mut source = error.source();
        while let Some(error) = source {
            eprintln!("  caused by: {error}");
            source = error.source();
        }
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

#[derive(Debug, clap::Parser)]
#[command(name = "evm2-replayer", version, about = "Replay revm-oomph block corpora with evm2.")]
struct Cli {
    /// Prepare all blocks before executing them.
    #[arg(long)]
    preload: bool,
    /// Replay corpus directory, blocks directory, or single block file.
    #[arg(value_name = "PATH")]
    path: PathBuf,
}

fn run() -> Result<()> {
    let cli = <Cli as clap::Parser>::parse();
    let plan = input::plan_from_path(cli.path)?;
    if cli.preload { runner::run_preloaded(plan) } else { runner::run_streaming(plan) }
}
