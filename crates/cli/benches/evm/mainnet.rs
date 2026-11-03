use criterion::{BatchSize, BenchmarkGroup, black_box, measurement::WallTime};
use evm2::evm::InMemoryDB;
use evm2_cli::evm_bench::BenchCase;
use evm2_eest::{
    BlockchainTestExecuteConfig, BlockchainTestExecutionMode, BlockchainTestNoopHook, NameFilter,
    blockchain_test_pre_state, blockchaintest::BlockchainTest, execute_blockchain_test_case_on,
    execute_blockchain_tests_suite,
};
use std::{
    borrow::Cow,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone)]
pub(crate) struct PreparedBench {
    name: Cow<'static, str>,
    path: PathBuf,
    suite: Arc<BlockchainTest>,
    test_filter: NameFilter,
    pre_state: InMemoryDB,
}

impl PreparedBench {
    pub(crate) fn load(bench: &BenchCase) -> Self {
        let path = workspace_path(bench.fixture_path);
        let suite = evm2_eest::read_blockchain_fixture(&path)
            .unwrap_or_else(|err| panic!("failed to read fixture {}: {err}", path.display()));
        let (case_name, case) = suite.0.first_key_value().expect("fixture must contain a case");
        let pre_state = blockchain_test_pre_state(&path, case_name, case)
            .unwrap_or_else(|err| panic!("failed to build {} pre-state: {err}", bench.name));
        Self {
            name: bench.name.clone(),
            path,
            suite: Arc::new(suite),
            test_filter: NameFilter::default(),
            pre_state,
        }
    }

    pub(crate) fn sanity_check(&self) {
        let mut hook = BlockchainTestNoopHook;
        let summary = execute_blockchain_tests_suite(
            &self.path,
            &self.suite,
            BlockchainTestExecuteConfig::default(),
            &self.test_filter,
            &mut hook,
        )
        .unwrap_or_else(|err| panic!("{} fixture sanity check failed: {err}", self.name));
        assert_eq!(summary.executed, 1);
        assert_eq!(summary.skipped, 0);
    }

    /// Times only block execution: building the pre-state database and dropping the final one
    /// stay outside the measurement.
    pub(crate) fn bench(&self, group: &mut BenchmarkGroup<'_, WallTime>) {
        let (case_name, case) =
            self.suite.0.first_key_value().expect("fixture must contain a case");
        for (suffix, mode) in modes() {
            self.bench_mode(group, case_name, case, suffix, mode);
        }
    }

    fn bench_mode(
        &self,
        group: &mut BenchmarkGroup<'_, WallTime>,
        case_name: &str,
        case: &evm2_eest::blockchaintest::BlockchainTestCase,
        suffix: &str,
        mode: BlockchainTestExecutionMode,
    ) {
        let config = BlockchainTestExecuteConfig { validate: false, mode, ..Default::default() };
        group.bench_function(format!("{}/replay{suffix}", self.name), |b| {
            b.iter_batched_ref(
                || self.pre_state.clone(),
                |database| {
                    black_box(
                        execute_blockchain_test_case_on(
                            &self.path,
                            case_name,
                            case,
                            database,
                            config,
                            &mut BlockchainTestNoopHook,
                        )
                        .unwrap_or_else(|err| panic!("{} fixture replay failed: {err}", self.name)),
                    )
                },
                BatchSize::PerIteration,
            );
        });
    }
}

/// The backends to replay with: the interpreter, and with `EVM2_BENCH_REPLAY_BACKENDS` set, every
/// backend this build has. Compiled backends stay warm across iterations.
fn modes() -> Vec<(&'static str, BlockchainTestExecutionMode)> {
    #[cfg_attr(not(any(feature = "jit", feature = "superinstr")), allow(unused_mut))]
    let mut modes = vec![("", BlockchainTestExecutionMode::Interpreter)];
    if std::env::var_os("EVM2_BENCH_REPLAY_BACKENDS").is_none() {
        return modes;
    }
    #[cfg(feature = "superinstr")]
    modes.push(("/superinstr", BlockchainTestExecutionMode::SuperInstr));
    #[cfg(feature = "jit")]
    if std::env::var_os("EVM2_EEST_SHARED_JIT").is_some() {
        modes.push(("/jit", BlockchainTestExecutionMode::Jit));
    }
    modes
}

fn workspace_path(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(path)
}
