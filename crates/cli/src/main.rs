//! User-facing `evm2` command-line tool.

mod args;
mod capture;
mod error;
mod ethereum;
mod fixture;
mod fuzzer;
mod list;
mod replay;
#[cfg(feature = "jit")]
mod run;
mod style;

use crate::{args::Args, error::Result};
use clap::Parser;
use std::{error::Error as _, process::ExitCode};

fn main() -> ExitCode {
    if let Err(error) = run() {
        let style = style::ERROR;
        eprintln!("{style}error{style:#}: {error}");
        let mut source = error.source();
        while let Some(error) = source {
            let muted = style::MUTED;
            eprintln!("{muted}caused by{muted:#}: {error}");
            source = error.source();
        }
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn run() -> Result<()> {
    #[cfg(feature = "jit")]
    if let std::ops::ControlFlow::Break(()) = evm2_jit_runtime::runtime::maybe_run_jit_helper()
        .map_err(|source| error::Error::JitHelper { source })?
    {
        return Ok(());
    }

    match Args::parse().command {
        args::Command::Capture(command) => capture::run(command),
        args::Command::Fuzzer(command) => fuzzer::run(command).map_err(error::Error::Fuzzer),
        args::Command::List(command) => list::run(command),
        args::Command::Replay(command) => replay::run(command),
        #[cfg(feature = "jit")]
        args::Command::Run(command) => command.run().map_err(|source| error::Error::Run { source }),
    }
}
