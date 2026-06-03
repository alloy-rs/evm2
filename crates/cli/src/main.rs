//! Command line tools for evm2.

use std::{env, ffi::OsString, process::ExitCode};

fn main() -> ExitCode {
    let mut args = env::args_os().collect::<Vec<_>>();
    let command = parse_command(&mut args);
    match command {
        Command::All => evm2_eest::run_direct_from_iter(args),
        Command::State => evm2_eest::run_statetests_direct_from_iter(args),
        Command::Blockchain => evm2_eest::run_blockchaintests_direct_from_iter(args),
    }
}

#[derive(Clone, Copy, Debug)]
enum Command {
    All,
    State,
    Blockchain,
}

fn parse_command(args: &mut Vec<OsString>) -> Command {
    let Some(command) = args.get(1).and_then(|arg| arg.to_str()) else {
        return Command::All;
    };
    let command = match command {
        "all" | "eest" => Command::All,
        "state" | "statetest" | "statetests" => Command::State,
        "blockchain" | "blockchaintest" | "blockchaintests" | "blockchain_tests" => {
            Command::Blockchain
        }
        _ => return Command::All,
    };
    args.remove(1);
    command
}
