use crate::{
    discover::find_json_tests,
    env::STATE_TEST_SUBDIR_ENV,
    fixtures::{EEST_ONLY_ENV, additional_tests_path, env_flag},
};
use libtest_mimic::{Arguments, Failed, Trial};
use std::{
    env,
    path::{Path, PathBuf},
    process::ExitCode,
};

#[cfg(feature = "jit")]
use crate::execution::CompiledMode;

const NEXTEST_ENV: &str = "NEXTEST";
const COMPILED_EEST_SUBSET_ENV: &str = "EVM2_COMPILED_EEST_SUBSET";
const IGNORE_LISTS_TRIAL: &str = "ignore_lists_match_fixtures";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CompiledEestSubset {
    All,
    CiAot,
    CiSmoke,
}

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

impl TestRoot {
    /// Whether this is the optional EEST `main` root: `statetests` or `blockchain_tests`, with an
    /// optional compiled-mode suffix. The names come from the root constructors in the `env`
    /// modules, so keep them in sync.
    fn is_main(&self) -> bool {
        let mut parts = self.name.split("::");
        matches!(parts.next(), Some("statetests" | "blockchain_tests"))
            && parts.all(|part| matches!(part, "jit" | "aot"))
    }
}

/// A JSON fixture test suite.
pub(crate) struct TestSuite {
    /// Human readable suite name.
    pub(crate) name: &'static str,
    /// Fixture roots included in this suite.
    pub(crate) roots: Vec<TestRoot>,
    /// Directory descent filter.
    pub(crate) should_descend: fn(&Path) -> bool,
    /// Lists of fixture-name patterns this suite skips.
    pub(crate) ignored: Vec<IgnoreList>,
    /// Test file runner.
    pub(crate) run_file: FileRunner,
    /// Optional batched file runner.
    pub(crate) run_files: Option<BatchRunner>,
}

impl TestSuite {
    /// Returns whether the fixture named `name` is skipped.
    fn ignores(&self, name: &str) -> bool {
        self.ignored.iter().any(|list| list.matches(name))
    }
}

/// A list of fixture-name patterns to skip. A pattern matches every fixture whose name contains it.
pub(crate) struct IgnoreList {
    patterns: &'static [&'static str],
    /// Whether the patterns name fixtures only the optional `main` roots contain.
    main_only: bool,
}

impl IgnoreList {
    /// A list of patterns that name fixtures in the default roots.
    pub(crate) const fn new(patterns: &'static [&'static str]) -> Self {
        Self { patterns, main_only: false }
    }

    /// A list of patterns that name fixtures only the optional `main` roots contain.
    pub(crate) const fn main_only(patterns: &'static [&'static str]) -> Self {
        Self { patterns, main_only: true }
    }

    fn matches(&self, name: &str) -> bool {
        self.patterns.iter().any(|pattern| name.contains(pattern))
    }
}

/// Runs cargo-nextest JSON fixture harnesses in one test binary.
pub(crate) fn run_json_harnesses(suites: Vec<TestSuite>) -> ExitCode {
    let mut args = Arguments::from_args();
    if !args.list && env::var_os(NEXTEST_ENV).is_none() {
        let suite_names = suites.iter().map(|suite| suite.name).collect::<Vec<_>>().join(", ");
        eprintln!("Skipping {suite_names} tests: run this target through cargo nextest.");
        return ExitCode::SUCCESS;
    }

    let trials = match collect_trials(&args, &suites) {
        Ok(trials) => trials,
        Err(err) => {
            eprintln!("{err}");
            return ExitCode::FAILURE;
        }
    };

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
                let ignored = suite.ignores(&name);
                let run_file = suite.run_file;
                trials.push(Trial::test(name, move || run_file(path)).with_ignored_flag(ignored));
            }
        }
    }
    trials.extend(ignore_lists_trial(suites)?);
    Ok(trials)
}

/// Builds a trial that fails when an ignore pattern matches no discovered fixture, so dead
/// entries cannot accumulate. The failure names each dead pattern and the roots searched.
///
/// Returns no trial when the roots are not the full default set: an explicit root, additional
/// tests, `SUBDIR`, or an execution-specs-only run leave patterns without fixtures. Patterns in a
/// main-only list are checked only when the owning suite has a `main` root.
fn ignore_lists_trial(suites: &[TestSuite]) -> Result<Option<Trial>, String> {
    let roots = || suites.iter().flat_map(|suite| &suite.roots);
    let subdir = env::var_os(STATE_TEST_SUBDIR_ENV).is_some_and(|subdir| !subdir.is_empty());
    if subdir
        || env_flag(EEST_ONLY_ENV)
        || additional_tests_path().is_some()
        || roots().any(|root| root.name.ends_with("::custom"))
    {
        return Ok(None);
    }

    let mut names = Vec::new();
    for suite in suites {
        for root in &suite.roots {
            let files = find_json_tests(std::slice::from_ref(&root.path), suite.should_descend)?;
            names.extend(files.iter().map(|path| test_name(&root.name, &root.path, path)));
        }
    }

    let mut dead = suites
        .iter()
        .flat_map(|suite| {
            let has_main = suite.roots.iter().any(TestRoot::is_main);
            suite.ignored.iter().filter(move |list| has_main || !list.main_only)
        })
        .flat_map(|list| list.patterns)
        .filter(|pattern| !names.iter().any(|name| name.contains(*pattern)))
        .collect::<Vec<_>>();
    dead.sort_unstable();
    dead.dedup();

    let mut searched = roots().map(|root| root.path.display().to_string()).collect::<Vec<_>>();
    searched.sort_unstable();
    searched.dedup();

    Ok(Some(Trial::test(IGNORE_LISTS_TRIAL, move || {
        if dead.is_empty() {
            return Ok(());
        }
        let dead = dead.iter().map(|pattern| format!("  {pattern}")).collect::<Vec<_>>();
        let searched = searched.iter().map(|root| format!("  {root}")).collect::<Vec<_>>();
        Err(format!(
            "ignore patterns that match no fixture; remove them:\n{}\nsearched roots:\n{}",
            dead.join("\n"),
            searched.join("\n")
        )
        .into())
    })))
}

fn exact_trial(suites: &[TestSuite], name: &str) -> Result<Option<Trial>, String> {
    if name == IGNORE_LISTS_TRIAL {
        return ignore_lists_trial(suites);
    }

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
        let ignored = suite.ignores(name);
        let run_file = suite.run_file;
        Trial::test(name.to_string(), move || run_file(path)).with_ignored_flag(ignored)
    }))
}

fn batched_trial(suite: &TestSuite) -> Result<Option<Trial>, String> {
    let Some(run_files) = suite.run_files else {
        return Ok(None);
    };

    let subset = explicit_compiled_eest_subset()?;
    let mut paths = Vec::new();
    for root in &suite.roots {
        let files = find_json_tests(std::slice::from_ref(&root.path), suite.should_descend)?;
        paths.extend(files.into_iter().filter(|path| {
            let name = test_name(&root.name, &root.path, path);
            !suite.ignores(&name) && compiled_eest_subset_allows(subset, &name)
        }));
    }

    Ok((!paths.is_empty()).then(|| Trial::test(suite.name.to_string(), move || run_files(paths))))
}

fn explicit_compiled_eest_subset() -> Result<Option<CompiledEestSubset>, String> {
    let Some(value) = env::var_os(COMPILED_EEST_SUBSET_ENV) else {
        return Ok(None);
    };
    let value = value.to_string_lossy();
    match value.as_ref() {
        "" | "all" => Ok(Some(CompiledEestSubset::All)),
        "ci-aot" => Ok(Some(CompiledEestSubset::CiAot)),
        "ci-smoke" => Ok(Some(CompiledEestSubset::CiSmoke)),
        _ => Err(format!(
            "unsupported {COMPILED_EEST_SUBSET_ENV}={value}; expected all, ci-aot, or ci-smoke"
        )),
    }
}

fn compiled_eest_subset_allows(subset: Option<CompiledEestSubset>, name: &str) -> bool {
    let subset = subset.unwrap_or_else(|| default_compiled_eest_subset(name));
    match subset {
        CompiledEestSubset::All => true,
        CompiledEestSubset::CiAot => CI_AOT_STATE_DIRS.iter().any(|path| name.contains(path)),
        CompiledEestSubset::CiSmoke => CI_SMOKE_STATE_FILES.iter().any(|path| name.ends_with(path)),
    }
}

fn default_compiled_eest_subset(name: &str) -> CompiledEestSubset {
    if name.contains("::aot") {
        CompiledEestSubset::CiAot
    } else if name.contains("::jit") {
        CompiledEestSubset::CiSmoke
    } else {
        CompiledEestSubset::All
    }
}

const CI_AOT_STATE_DIRS: &[&str] = &[
    "frontier/create/",
    "frontier/opcodes/",
    "homestead/coverage/",
    "byzantium/eip198_modexp_precompile/",
    "constantinople/eip145_bitwise_shift/",
    "istanbul/eip1344_chainid/",
    "berlin/eip2930_access_list/",
    "shanghai/eip3855_push0/",
    "cancun/eip5656_mcopy/",
    "prague/eip7702_set_code_tx/",
    "osaka/eip7939_count_leading_zeros/",
];

const CI_SMOKE_STATE_FILES: &[&str] = &[
    "frontier/create/test_create_suicide_store.json",
    "frontier/create/create_suicide_store/create_suicide_store.json",
    "frontier/opcodes/test_push.json",
    "frontier/opcodes/push/push.json",
    "berlin/eip2930_access_list/test_tx_intrinsic_gas.json",
    "berlin/eip2930_access_list/tx_intrinsic_gas/tx_intrinsic_gas.json",
    "shanghai/eip3855_push0/test_push0_contracts.json",
    "shanghai/eip3855_push0/push0/push0_contracts.json",
    "cancun/eip5656_mcopy/test_valid_mcopy_operations.json",
    "cancun/eip5656_mcopy/mcopy/valid_mcopy_operations.json",
    "prague/eip7702_set_code_tx/test_eip_7702.json",
    "prague/eip7702_set_code_tx/set_code_txs/address_from_set_code.json",
    "osaka/eip7939_count_leading_zeros/test_clz_gas_cost.json",
    "osaka/eip7939_count_leading_zeros/count_leading_zeros/clz_gas_cost.json",
];

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

    #[test]
    fn default_compiled_subset_smokes_jit_on_all_targets() {
        assert_eq!(
            default_compiled_eest_subset(
                "statetests::custom::jit::frontier/create/test_create_suicide_store.json",
            ),
            CompiledEestSubset::CiSmoke
        );
        assert_eq!(
            default_compiled_eest_subset(
                "statetests::custom::aot::frontier/create/test_create_suicide_store.json",
            ),
            CompiledEestSubset::CiAot
        );
        assert_eq!(
            default_compiled_eest_subset(
                "statetests::custom::frontier/create/test_create_suicide_store.json",
            ),
            CompiledEestSubset::All
        );
    }
}
