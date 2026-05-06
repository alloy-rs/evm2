use crate::{
    discover::find_json_tests,
    env::{StateTestRoot, state_test_roots},
    execute::{ExecuteConfig, execute_test_suite},
};
use libtest_mimic::{Arguments, Failed, Trial};
use std::{
    env,
    path::{Path, PathBuf},
    process::ExitCode,
};

const NEXTEST_ENV: &str = "NEXTEST";

/// Runs the cargo-nextest state test harness.
pub fn run() -> ExitCode {
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
            let ignored = should_ignore(&name);
            trials.push(Trial::test(name, move || run_file(path)).with_ignored_flag(ignored));
        }
    }
    Ok(trials)
}

fn exact_trial(roots: &[StateTestRoot], name: &str) -> Option<Trial> {
    let (root_name, relative) = name.split_once("::")?;
    let root = roots.iter().find(|root| root.name == root_name)?;
    let path = root.path.join(relative);
    path.is_file().then(|| {
        let ignored = should_ignore(name);
        Trial::test(name.to_string(), move || run_file(path)).with_ignored_flag(ignored)
    })
}

fn run_file(path: PathBuf) -> Result<(), Failed> {
    execute_test_suite(&path, ExecuteConfig::default())
        .map(|_| ())
        .map_err(|err| err.to_string().into())
}

fn test_name(root_name: &str, root: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(root).unwrap_or(path);
    format!("{root_name}::{}", path_name(relative))
}

fn path_name(path: &Path) -> String {
    path.iter().map(|component| component.to_string_lossy()).collect::<Vec<_>>().join("/")
}

const SLOW_TESTS: &[&str] = &[
    "CALLBlake2f_MaxRounds.json",
    "loopExp",
    "loopMul.json",
    "stQuadraticComplexityTest/Call1MB1024Calldepth.json",
    "stQuadraticComplexityTest/Create1000",
    "stRecursiveCreate/recursiveCreate",
    "stRevertTest/LoopCallsDepthThenRevert",
    "stRevertTest/LoopDelegateCallsDepthThenRevert",
    "stSolidityTest/RecursiveCreateContracts",
    "stStaticCall/static_Call1MB1024Calldepth.json",
    "stStaticCall/static_Call50000_sha256",
    "stStaticCall/static_CallRecursiveBomb",
    "stStaticCall/static_LoopCallsDepthThenRevert",
    "stSystemOperationsTest/CallRecursiveBomb",
];

// EIP-7610 create-collision fixtures require storage-aware collision handling.
const EIP7610_CREATE_COLLISION_TESTS: &[&str] = &["eip7610_create_collision"];

// Init collision fixtures require treating pre-existing storage as a collision.
const INIT_COLLISION_TESTS: &[&str] = &["InitCollision.json", "InitCollisionParis.json"];

// Revert-in-create fixtures require preserving storage-only collision state.
const REVERT_IN_CREATE_TESTS: &[&str] =
    &["RevertInCreateInInit.json", "RevertInCreateInInit_Paris.json"];

// CREATE2 revert-in-create fixtures require storage-aware collision handling.
const REVERT_IN_CREATE2_TESTS: &[&str] =
    &["RevertInCreateInInitCreate2.json", "RevertInCreateInInitCreate2Paris.json"];

// CREATE2 storage collision fixtures require storage-aware collision handling.
const CREATE2_STORAGE_COLLISION_TESTS: &[&str] =
    &["create2collisionStorage.json", "create2collisionStorageParis.json"];

// Dynamic overwrite fixtures require storage-aware empty-account handling.
const DYNAMIC_OVERWRITE_TESTS: &[&str] =
    &["dynamicAccountOverwriteEmpty.json", "dynamicAccountOverwriteEmpty_Paris.json"];

fn should_ignore(name: &str) -> bool {
    [
        SLOW_TESTS,
        EIP7610_CREATE_COLLISION_TESTS,
        INIT_COLLISION_TESTS,
        REVERT_IN_CREATE_TESTS,
        REVERT_IN_CREATE2_TESTS,
        CREATE2_STORAGE_COLLISION_TESTS,
        DYNAMIC_OVERWRITE_TESTS,
    ]
    .into_iter()
    .flatten()
    .any(|pattern| name.contains(pattern))
}
