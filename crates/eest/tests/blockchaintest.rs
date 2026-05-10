//! cargo-nextest blockchain test entrypoint.

use std::process::ExitCode;

fn main() -> ExitCode {
    evm2_eest::run_blockchaintests()
}
