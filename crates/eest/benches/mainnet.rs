#![allow(missing_docs)]

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use evm2_eest::{
    BlockchainTestExecuteConfig, BlockchainTestNoopHook, EntryPoint,
    blockchaintest::BlockchainTest, execute_blockchain_tests_suite,
};
use std::{path::Path, time::Duration};

const MAINNET_BLOCKS: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/mainnet-25346511-25346520.json.gz");

fn mainnet(c: &mut Criterion) {
    let path = Path::new(MAINNET_BLOCKS);
    let input = evm2_eest::read_fixture_text(path)
        .unwrap_or_else(|err| panic!("failed to read fixture: {err}"));
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

    group.bench_function("25346511_25346520/replay", |b| {
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

criterion_group!(benches, mainnet);
criterion_main!(benches);
