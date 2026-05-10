//! cargo-nextest state test entrypoint.

use std::process::ExitCode;

fn main() -> ExitCode {
    evm2_eest::run_statetests()
}
