//! Replays flattened `revm-oomph` block corpora with `evm2`.

mod cli;
mod corpus;
mod error;
mod ethereum;
mod execute;
mod input;
mod prepare;
mod report;
mod runner;

fn main() -> std::process::ExitCode {
    cli::main()
}
