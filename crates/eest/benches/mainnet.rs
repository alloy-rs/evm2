#![allow(missing_docs)]

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use evm2_eest::{
    BlockchainTestExecuteConfig, BlockchainTestNoopHook, EntryPoint,
    blockchaintest::BlockchainTest, execute_blockchain_tests_suite,
};
use std::{fs, path::Path, time::Duration};

const MAINNET_100_BLOCKS: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/mainnet-17000000-17000099.json");

fn mainnet(c: &mut Criterion) {
    let path = Path::new(MAINNET_100_BLOCKS);
    let input =
        fs::read_to_string(path).unwrap_or_else(|err| panic!("failed to read fixture: {err}"));
    let suite: BlockchainTest =
        serde_json::from_str(&input).unwrap_or_else(|err| panic!("failed to parse fixture: {err}"));
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

    group.bench_function("17000000_17000099/replay", |b| {
        b.iter(|| {
            let mut hook = BlockchainTestNoopHook;
            black_box(
                execute_blockchain_tests_suite(
                    path,
                    &suite,
                    BlockchainTestExecuteConfig { validate_post_state: false },
                    &entrypoint,
                    &mut hook,
                )
                .unwrap_or_else(|err| panic!("mainnet fixture replay failed: {err}")),
            )
        });
    });

    group.finish();
}

criterion_group!(benches, mainnet);
criterion_main!(benches);
