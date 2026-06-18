use criterion::{Criterion, black_box};
use evm2_eest::{
    BlockchainTestExecuteConfig, BlockchainTestNoopHook, EntryPoint, execute_blockchain_tests_suite,
};
use std::{path::Path, time::Duration};

const MAINNET_BLOCKS: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/mainnet-25347446-25347455.bin.zst");

pub(crate) fn bench(c: &mut Criterion) {
    let path = Path::new(MAINNET_BLOCKS);
    let suite = evm2_eest::read_blockchain_fixture(path)
        .unwrap_or_else(|err| panic!("failed to read fixture: {err}"));
    let entrypoint = EntryPoint::default();

    let mut hook = BlockchainTestNoopHook;
    let summary = execute_blockchain_tests_suite(
        path,
        &suite,
        BlockchainTestExecuteConfig::default(),
        &entrypoint,
        &mut hook,
    )
    .unwrap_or_else(|err| panic!("mainnet fixture sanity check failed: {err}"));
    assert_eq!(summary.executed, 1);
    assert_eq!(summary.skipped, 0);

    let mut group = c.benchmark_group("mainnet");
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(2));
    group.sample_size(10);

    group.bench_function("25347446_25347455/replay", |b| {
        b.iter(|| {
            let mut hook = BlockchainTestNoopHook;
            black_box(
                execute_blockchain_tests_suite(
                    path,
                    &suite,
                    BlockchainTestExecuteConfig {
                        validate_post_state: false,
                        ..Default::default()
                    },
                    &entrypoint,
                    &mut hook,
                )
                .unwrap_or_else(|err| panic!("mainnet fixture replay failed: {err}")),
            )
        });
    });

    group.finish();
}
