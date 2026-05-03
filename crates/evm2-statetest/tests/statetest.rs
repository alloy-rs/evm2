//! cargo-nextest state test entrypoint.

use std::process::ExitCode;

fn main() -> ExitCode {
    evm2_statetest::run()
}
