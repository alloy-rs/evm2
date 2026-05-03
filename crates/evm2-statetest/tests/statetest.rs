//! cargo-nextest state test harness.

use evm2_statetest::{ExecuteConfig, execute_test_suite, find_json_tests, state_test_roots};
use libtest_mimic::{Arguments, Failed, Trial};
use std::{
    env,
    path::{Path, PathBuf},
    process::ExitCode,
    thread,
};

const NEXTEST_ENV: &str = "NEXTEST";
const STATE_TEST_STACK_SIZE: usize = 64 * 1024 * 1024;

fn main() -> ExitCode {
    let args = Arguments::from_args();
    if !args.list && env::var_os(NEXTEST_ENV).is_none() {
        eprintln!("Skipping state tests: run this target through cargo nextest.");
        return ExitCode::SUCCESS;
    }

    let trials = collect_trials(&args).unwrap_or_else(|err| {
        eprintln!("{err}");
        Vec::new()
    });

    libtest_mimic::run(&args, trials).exit_code()
}

fn collect_trials(args: &Arguments) -> Result<Vec<Trial>, String> {
    let roots = state_test_roots();
    if roots.is_empty() {
        return Ok(Vec::new());
    }

    if args.exact
        && let Some(filter) = &args.filter
    {
        return Ok(exact_trial(&roots, filter).into_iter().collect());
    }

    let mut trials = Vec::new();
    for root in roots {
        let files =
            find_json_tests(std::slice::from_ref(&root.path)).map_err(|err| err.to_string())?;
        for path in files {
            let name = test_name(root.name, &root.path, &path);
            trials.push(Trial::test(name, move || run_file(path)));
        }
    }
    Ok(trials)
}

fn exact_trial(roots: &[evm2_statetest::StateTestRoot], name: &str) -> Option<Trial> {
    let (root_name, relative) = name.split_once("::")?;
    let root = roots.iter().find(|root| root.name == root_name)?;
    let path = root.path.join(relative);
    path.is_file().then(|| Trial::test(name.to_string(), move || run_file(path)))
}

fn run_file(path: PathBuf) -> Result<(), Failed> {
    let thread_name =
        format!("statetest-{}", path_name(path.file_name().map(Path::new).unwrap_or(&path)));
    thread::Builder::new()
        .name(thread_name)
        .stack_size(STATE_TEST_STACK_SIZE)
        .spawn(move || {
            execute_test_suite(&path, ExecuteConfig::default())
                .map(|_| ())
                .map_err(|err| err.to_string())
        })
        .map_err(|err| format!("failed to spawn state test thread: {err}"))?
        .join()
        .map_err(|_| "state test thread panicked".to_string())?
        .map_err(Failed::from)
}

fn test_name(root_name: &str, root: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(root).unwrap_or(path);
    format!("{root_name}::{}", path_name(relative))
}

fn path_name(path: &Path) -> String {
    path.iter().map(|component| component.to_string_lossy()).collect::<Vec<_>>().join("/")
}
