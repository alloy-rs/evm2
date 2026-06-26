use crate::discover::find_json_tests;
#[cfg(feature = "jit")]
use crate::execution::CompiledMode;
use libtest_mimic::{Arguments, Failed, Trial};
use std::{
    env,
    path::{Path, PathBuf},
    process::ExitCode,
};

const NEXTEST_ENV: &str = "NEXTEST";

type FileRunner = fn(PathBuf) -> Result<(), Failed>;
type BatchRunner = fn(Vec<PathBuf>) -> Result<(), Failed>;

/// A named EEST fixture root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TestRoot {
    /// Stable root name used by the nextest harness.
    pub(crate) name: String,
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
    pub(crate) run_file: FileRunner,
    /// Optional batched file runner.
    pub(crate) run_files: Option<BatchRunner>,
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
        return exact_trial(suites, filter).map(|trial| trial.into_iter().collect());
    }

    let mut trials = Vec::new();
    for suite in suites {
        if let Some(trial) = batched_trial(suite)? {
            trials.push(trial);
            continue;
        }

        for root in &suite.roots {
            let files = find_json_tests(std::slice::from_ref(&root.path), suite.should_descend)?;
            for path in files {
                let name = test_name(&root.name, &root.path, &path);
                let ignored = (suite.should_ignore)(&name);
                let run_file = suite.run_file;
                trials.push(Trial::test(name, move || run_file(path)).with_ignored_flag(ignored));
            }
        }
    }
    Ok(trials)
}

fn exact_trial(suites: &[TestSuite], name: &str) -> Result<Option<Trial>, String> {
    if let Some(suite) = suites.iter().find(|suite| suite.run_files.is_some() && suite.name == name)
    {
        return batched_trial(suite);
    }

    let Some((suite, root, relative)) = suites
        .iter()
        .flat_map(|suite| suite.roots.iter().map(move |root| (suite, root)))
        .filter_map(|(suite, root)| {
            name.strip_prefix(root.name.as_str()).map(|relative| (suite, root, relative))
        })
        .filter_map(|(suite, root, relative)| {
            relative.strip_prefix("::").map(|relative| (suite, root, relative))
        })
        .max_by_key(|(_, root, _)| root.name.len())
    else {
        return Ok(None);
    };
    // A root pointing directly at a file has an empty relative; the file itself
    // is the only path it can resolve to.
    let path = if root.path.is_file() { root.path.clone() } else { root.path.join(relative) };
    Ok(path.is_file().then(|| {
        let ignored = (suite.should_ignore)(name);
        let run_file = suite.run_file;
        Trial::test(name.to_string(), move || run_file(path)).with_ignored_flag(ignored)
    }))
}

fn batched_trial(suite: &TestSuite) -> Result<Option<Trial>, String> {
    let Some(run_files) = suite.run_files else {
        return Ok(None);
    };

    let mut paths = Vec::new();
    for root in &suite.roots {
        let files = find_json_tests(std::slice::from_ref(&root.path), suite.should_descend)?;
        paths.extend(files.into_iter().filter(|path| {
            let name = test_name(&root.name, &root.path, path);
            !(suite.should_ignore)(&name)
        }));
    }

    Ok((!paths.is_empty()).then(|| Trial::test(suite.name.to_string(), move || run_files(paths))))
}

fn test_name(root_name: &str, root: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(root).unwrap_or(path);
    // When the root is the file itself the relative is empty; name it after the
    // file so the trial has a stable, matchable name.
    if relative.as_os_str().is_empty() {
        let file = path.file_name().map(|name| name.to_string_lossy()).unwrap_or_default();
        return format!("{root_name}::{file}");
    }
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
pub(crate) fn compiled_roots(roots: Vec<TestRoot>, mode: CompiledMode) -> Vec<TestRoot> {
    roots
        .into_iter()
        .map(|mut root| {
            root.name = format!("{}::{}", root.name, mode.suffix());
            root
        })
        .collect()
}

#[cfg(all(test, feature = "jit"))]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn compiled_roots_append_mode_suffix() {
        let roots = vec![TestRoot {
            name: "statetests::custom".to_string(),
            label: "custom",
            path: PathBuf::new(),
        }];

        assert_eq!(
            compiled_roots(roots.clone(), CompiledMode::Jit)[0].name,
            "statetests::custom::jit"
        );
        assert_eq!(compiled_roots(roots, CompiledMode::Aot)[0].name, "statetests::custom::aot");
    }
}
