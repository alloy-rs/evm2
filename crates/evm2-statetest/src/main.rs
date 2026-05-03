//! Command-line Ethereum state test runner for evm2.

use evm2_statetest::{RunConfig, find_json_tests, run_with_config, state_test_root_from_env};
use std::{env, path::PathBuf, process, thread};

fn main() {
    let args = match Args::parse() {
        Ok(args) => args,
        Err(err) => {
            eprintln!("{err}");
            process::exit(2);
        }
    };

    let files = match find_json_tests(&args.paths) {
        Ok(files) => files,
        Err(err) => {
            eprintln!("{err}");
            process::exit(2);
        }
    };

    let config = RunConfig {
        jobs: args.jobs,
        single_thread: args.single_thread,
        keep_going: args.keep_going,
        omit_progress: args.omit_progress,
        print_json_outcome: args.print_json_outcome,
        trace: args.trace,
    };
    if let Err(err) = run_with_config(files, config) {
        eprintln!("{err}");
        process::exit(1);
    }
}

#[derive(Clone, Debug)]
struct Args {
    paths: Vec<PathBuf>,
    jobs: Option<usize>,
    single_thread: bool,
    keep_going: bool,
    omit_progress: bool,
    print_json_outcome: bool,
    trace: bool,
}

impl Args {
    fn parse() -> Result<Self, String> {
        let mut paths = Vec::new();
        let mut jobs = Some(thread::available_parallelism().map_or(1, |jobs| jobs.get()).min(28));
        let mut single_thread = false;
        let mut keep_going = false;
        let mut omit_progress = false;
        let mut print_json_outcome = false;
        let mut trace = false;
        let mut args = env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "-j" | "--jobs" => {
                    let Some(value) = args.next() else {
                        return Err("-j requires a value".to_string());
                    };
                    jobs = Some(value.parse().map_err(|_| format!("invalid job count: {value}"))?);
                }
                _ if arg.starts_with("-j") && arg.len() > 2 => {
                    let value = &arg[2..];
                    jobs = Some(value.parse().map_err(|_| format!("invalid job count: {value}"))?);
                }
                _ if arg.starts_with("--jobs=") => {
                    let value = &arg["--jobs=".len()..];
                    jobs = Some(value.parse().map_err(|_| format!("invalid job count: {value}"))?);
                }
                "-s" | "--single-thread" => {
                    single_thread = true;
                    jobs = Some(1);
                }
                "--keep-going" => keep_going = true,
                "--no-fail-fast" => keep_going = true,
                "--omit-progress" => omit_progress = true,
                "--json" => print_json_outcome = true,
                "-o" | "--json-outcome" => print_json_outcome = true,
                "--trace" => {
                    trace = true;
                    print_json_outcome = true;
                    single_thread = true;
                }
                "-h" | "--help" => {
                    println!(
                        "usage: evm2-statetest [-j N] [-s] [--keep-going] [--json-outcome] [--omit-progress] <file-or-dir>...\n\
                         \n\
                         If no paths are provided, set EVM2_STATETEST_ROOT or ETHEREUM_TESTS."
                    );
                    process::exit(0);
                }
                _ if arg.starts_with('-') => return Err(format!("unknown option: {arg}")),
                _ => paths.push(PathBuf::from(arg)),
            }
        }
        if paths.is_empty() {
            let Some(path) = state_test_root_from_env() else {
                return Err(
                    "missing test path; pass <file-or-dir> or set EVM2_STATETEST_ROOT".to_string()
                );
            };
            paths.push(path);
        }
        Ok(Self {
            paths,
            jobs,
            single_thread,
            keep_going,
            omit_progress,
            print_json_outcome,
            trace,
        })
    }
}
