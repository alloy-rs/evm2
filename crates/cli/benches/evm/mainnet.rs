use criterion::{BatchSize, BenchmarkGroup, black_box, measurement::WallTime};
use evm2::evm::InMemoryDB;
use evm2_cli::evm_bench::BenchCase;
use evm2_eest::{
    BlockchainTestExecuteConfig, BlockchainTestNoopHook, NameFilter, blockchain_test_pre_state,
    blockchaintest::BlockchainTest, execute_blockchain_test_case_on,
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
        let config =
            BlockchainTestExecuteConfig { validate_post_state: false, ..Default::default() };
        group.bench_function(format!("{}/replay", self.name), |b| {
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

fn workspace_path(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(path)
}
