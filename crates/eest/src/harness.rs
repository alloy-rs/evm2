use crate::discover::find_json_tests;
use libtest_mimic::{Arguments, Failed, Trial};
use std::{
    env,
    path::{Path, PathBuf},
    process::ExitCode,
};

const NEXTEST_ENV: &str = "NEXTEST";

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

/// Runs a cargo-nextest JSON fixture harness.
pub(crate) fn run_json_harness(
    suite_name: &'static str,
    roots: Vec<TestRoot>,
    should_descend: fn(&Path) -> bool,
    should_ignore: fn(&str) -> bool,
    run_file: fn(PathBuf) -> Result<(), Failed>,
) -> ExitCode {
    let mut args = Arguments::from_args();
    if !args.list && env::var_os(NEXTEST_ENV).is_none() {
        eprintln!("Skipping {suite_name} tests: run this target through cargo nextest.");
        return ExitCode::SUCCESS;
    }

    let trials = collect_trials(&args, &roots, should_descend, should_ignore, run_file)
        .unwrap_or_else(|err| {
            eprintln!("{err}");
            Vec::new()
        });

    if trials.len() <= 1 {
        args.test_threads = Some(1);
    }

    libtest_mimic::run(&args, trials).exit_code()
}

fn collect_trials(
    args: &Arguments,
    roots: &[TestRoot],
    should_descend: fn(&Path) -> bool,
    should_ignore: fn(&str) -> bool,
    run_file: fn(PathBuf) -> Result<(), Failed>,
) -> Result<Vec<Trial>, String> {
    if roots.is_empty() {
        return Ok(Vec::new());
    }

    if args.exact
        && let Some(filter) = &args.filter
    {
        return Ok(exact_trial(roots, filter, should_ignore, run_file).into_iter().collect());
    }

    let mut trials = Vec::new();
    for root in roots {
        let files = find_json_tests(std::slice::from_ref(&root.path), should_descend)?;
        for path in files {
            let name = test_name(root.name, &root.path, &path);
            let ignored = should_ignore(&name);
            trials.push(Trial::test(name, move || run_file(path)).with_ignored_flag(ignored));
        }
    }
    Ok(trials)
}

fn exact_trial(
    roots: &[TestRoot],
    name: &str,
    should_ignore: fn(&str) -> bool,
    run_file: fn(PathBuf) -> Result<(), Failed>,
) -> Option<Trial> {
    let (root_name, relative) = name.split_once("::")?;
    let root = roots.iter().find(|root| root.name == root_name)?;
    let path = root.path.join(relative);
    path.is_file().then(|| {
        let ignored = should_ignore(name);
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

/// Does not ignore any test.
pub(crate) const fn ignore_none(_: &str) -> bool {
    false
}
