#![allow(missing_docs, unused_crate_dependencies)]

const CMD: &str = env!("CARGO_BIN_EXE_revmc");

fn main() {
    let code = evm2_jit_cli_tests::run_tests(CMD.as_ref());
    std::process::exit(code);
}
