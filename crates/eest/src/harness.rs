use crate::discover::find_json_tests;
use libtest_mimic::{Arguments, Failed, Trial};
#[cfg(feature = "jit")]
use std::{any::Any, thread};
use std::{
    env,
    path::{Path, PathBuf},
    process::ExitCode,
};

const NEXTEST_ENV: &str = "NEXTEST";
#[cfg(feature = "jit")]
pub(crate) const COMPILED_FIXTURE_STACK_SIZE: usize = 64 * 1024 * 1024;

/// A named EEST fixture root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TestRoot {
    /// Stable root name used by the nextest harness.
    pub(crate) name: &'static str,
    /// Human readable root label.
    pub(crate) label: &'static str,
    /// Directory containing JSON test files.
    pub(crate) path: PathBuf,
}

/// A JSON fixture test suite.
pub(crate) struct TestSuite {
    /// Human readable suite name.
    pub(crate) name: &'static str,
    /// Fixture roots included in this suite.
    pub(crate) roots: Vec<TestRoot>,
    /// Directory descent filter.
    pub(crate) should_descend: fn(&Path) -> bool,
    /// Test ignore filter.
    pub(crate) should_ignore: fn(&str) -> bool,
    /// Test file runner.
    pub(crate) run_file: fn(PathBuf) -> Result<(), Failed>,
}

/// Runs cargo-nextest JSON fixture harnesses in one test binary.
pub(crate) fn run_json_harnesses(suites: Vec<TestSuite>) -> ExitCode {
    let mut args = Arguments::from_args();
    if !args.list && env::var_os(NEXTEST_ENV).is_none() {
        let suite_names = suites.iter().map(|suite| suite.name).collect::<Vec<_>>().join(", ");
        eprintln!("Skipping {suite_names} tests: run this target through cargo nextest.");
        return ExitCode::SUCCESS;
    }

    let trials = collect_trials(&args, &suites).unwrap_or_else(|err| {
        eprintln!("{err}");
        Vec::new()
    });

    if trials.len() <= 1 {
        args.test_threads = Some(1);
    }

    libtest_mimic::run(&args, trials).exit_code()
}

fn collect_trials(args: &Arguments, suites: &[TestSuite]) -> Result<Vec<Trial>, String> {
    if suites.iter().all(|suite| suite.roots.is_empty()) {
        return Ok(Vec::new());
    }

    if args.exact
        && let Some(filter) = &args.filter
    {
        return Ok(exact_trial(suites, filter).into_iter().collect());
    }

    let mut trials = Vec::new();
    for suite in suites {
        for root in &suite.roots {
            let files = find_json_tests(std::slice::from_ref(&root.path), suite.should_descend)?;
            for path in files {
                let name = test_name(root.name, &root.path, &path);
                let ignored = (suite.should_ignore)(&name);
                let run_file = suite.run_file;
                trials.push(Trial::test(name, move || run_file(path)).with_ignored_flag(ignored));
            }
        }
    }
    Ok(trials)
}

fn exact_trial(suites: &[TestSuite], name: &str) -> Option<Trial> {
    let (suite, root, relative) = suites
        .iter()
        .flat_map(|suite| suite.roots.iter().map(move |root| (suite, root)))
        .filter_map(|(suite, root)| {
            name.strip_prefix(root.name).map(|relative| (suite, root, relative))
        })
        .filter_map(|(suite, root, relative)| {
            relative.strip_prefix("::").map(|relative| (suite, root, relative))
        })
        .max_by_key(|(_, root, _)| root.name.len())?;
    let path = root.path.join(relative);
    path.is_file().then(|| {
        let ignored = (suite.should_ignore)(name);
        let run_file = suite.run_file;
        Trial::test(name.to_string(), move || run_file(path)).with_ignored_flag(ignored)
    })
}

fn test_name(root_name: &str, root: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(root).unwrap_or(path);
    format!("{root_name}::{}", path_name(relative))
}

fn path_name(path: &Path) -> String {
    path.iter().map(|component| component.to_string_lossy()).collect::<Vec<_>>().join("/")
}

/// Descends into every directory.
pub(crate) const fn descend_all(_: &Path) -> bool {
    true
}

#[cfg(feature = "jit")]
pub(crate) fn run_with_stack<F>(name: &'static str, stack_size: usize, f: F) -> Result<(), Failed>
where
    F: FnOnce() -> Result<(), Failed> + Send + 'static,
{
    let handle = thread::Builder::new()
        .name(name.into())
        .stack_size(stack_size)
        .spawn(f)
        .map_err(|err| err.to_string())?;
    handle.join().map_err(panic_payload_message)?
}

#[cfg(feature = "jit")]
fn panic_payload_message(payload: Box<dyn Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&'static str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "test thread panicked".to_string()
    }
}
