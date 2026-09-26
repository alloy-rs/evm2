use alloy_primitives::Bytes;
use criterion::{Criterion, Throughput, black_box};
use evm2::bytecode::Bytecode;
use std::path::{Path, PathBuf};

/// Account that holds the contract in most fixtures.
const TARGET: &str = "0xcccccccccccccccccccccccccccccccccccccccc";

/// Fixture files and accounts whose code to analyze.
const CONTRACTS: &[(&str, &str, &str)] = &[
    ("push0_proxy", "push0_proxy.json", TARGET),
    ("eip2935", "eip2935.json", TARGET),
    ("hash_10k", "hash_10k.json", TARGET),
    ("counter", "counter.json", TARGET),
    ("usdc_proxy", "usdc_proxy.json", TARGET),
    ("weth", "weth.json", TARGET),
    ("erc20", "erc20_transfer.json", TARGET),
    ("curve", "curve-stableswap-2pool.json", TARGET),
    ("snailtracer", "snailtracer.json", TARGET),
    ("uniswap_v2_pair", "uniswap_v2_pair.json", TARGET),
    ("univ2_router", "univ2_router.json", TARGET),
    ("fiat_token", "fiat_token.json", TARGET),
    ("seaport", "seaport.json", TARGET),
    ("burntpix", "burntpix.json", "0x49206861766520746f6f206d7563682074696d65"),
    ("onchain_lm_data", "onchain-lm-v2.json", "0x17178489592e2d8cf1146bc43304e91f0719325c"),
];

pub(crate) fn analysis(c: &mut Criterion) {
    let mut group = c.benchmark_group("analysis");
    for &(name, file, address) in CONTRACTS {
        let code = load(file, address);
        group.throughput(Throughput::Bytes(code.len() as u64));
        group.bench_function(name, |b| b.iter(|| Bytecode::new_legacy(black_box(code.clone()))));
    }
    group.finish();
}

fn load(file: &str, address: &str) -> Bytes {
    let path = workspace_path("data").join(file);
    let json: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let (_, test) = json.as_object().unwrap().iter().next().unwrap();
    serde_json::from_value(test["pre"][address]["code"].clone()).unwrap()
}

fn workspace_path(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(path)
}
