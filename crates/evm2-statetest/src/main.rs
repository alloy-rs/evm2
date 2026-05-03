//! Command-line Ethereum state test runner for evm2.

use evm2_statetest::{find_json_tests, run, state_test_root_from_env};
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

    if let Err(err) = run(files, args.jobs, args.keep_going) {
        eprintln!("{err}");
        process::exit(1);
    }
}

#[derive(Clone, Debug)]
struct Args {
    paths: Vec<PathBuf>,
    jobs: usize,
    keep_going: bool,
}

impl Args {
    fn parse() -> Result<Self, String> {
        let mut paths = Vec::new();
        let mut jobs = thread::available_parallelism().map_or(1, |jobs| jobs.get()).min(28);
        let mut keep_going = false;
        let mut args = env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "-j" | "--jobs" => {
                    let Some(value) = args.next() else {
                        return Err("-j requires a value".to_string());
                    };
                    jobs = value.parse().map_err(|_| format!("invalid job count: {value}"))?;
                }
                _ if arg.starts_with("-j") && arg.len() > 2 => {
                    let value = &arg[2..];
                    jobs = value.parse().map_err(|_| format!("invalid job count: {value}"))?;
                }
                _ if arg.starts_with("--jobs=") => {
                    let value = &arg["--jobs=".len()..];
                    jobs = value.parse().map_err(|_| format!("invalid job count: {value}"))?;
                }
                "--keep-going" => keep_going = true,
                "-h" | "--help" => {
                    println!(
                        "usage: evm2-statetest [-j N] [--keep-going] <file-or-dir>...\n\
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
        Ok(Self { paths, jobs: jobs.max(1), keep_going })
    }
}
