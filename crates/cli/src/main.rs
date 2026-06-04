//! User-facing `evm2` command-line tool.

mod args;
mod capture;
mod error;
mod ethereum;
mod fixture;
mod list;
mod replay;

use crate::{args::Args, error::Result};
use clap::Parser;
use std::{error::Error as _, process::ExitCode};

fn main() -> ExitCode {
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

fn run() -> Result<()> {
    match Args::parse().command {
        args::Command::Capture(command) => capture::run(command),
        args::Command::List(command) => list::run(command),
        args::Command::Replay(command) => replay::run(command),
    }
}
