//! Benchmarks of the `evm2-bytecode` primitives over the bundled benchmark contracts and one large
//! mainnet contract.

#![allow(missing_docs, unexpected_cfgs, dead_code, clippy::missing_const_for_fn)]

use alloy_primitives::{Bytes, U256, hex};
use criterion::{Criterion, black_box, criterion_group, criterion_main};
use evm2::{
    SpecId,
    bytecode::{InstructionMap, Instructions, analyze_legacy},
};
use evm2_cli::evm_bench::{self, BenchKind};
use std::time::Duration;

#[path = "evm/fixture.rs"]
mod fixture;

const SPEC: SpecId = SpecId::OSAKA;

/// The Uniswap V3 Factory, deployed at 0x1F98431c8aD98523631AE4a59f267346ea31F984: 24,535 bytes,
/// 41 bytes under the code size limit.
const LARGE_NAME: &str = "uniswap_v3_factory";
const LARGE_HEX: &str = include_str!("bytecode/uniswap_v3_factory.hex");

/// Returns the entry contract of every bundled transaction benchmark.
fn bundled() -> Vec<(String, Bytes)> {
    let benches = evm_bench::BENCHES
        .iter()
        .filter(|bench| !matches!(bench.kind, BenchKind::BlockchainReplay))
        .collect::<Vec<_>>();
    let suites =
        fixture::Suites::load(benches.iter().filter_map(|bench| bench.transaction_fixture_path()));
    let mut codes = Vec::new();
    for bench in benches {
        match bench.kind {
            BenchKind::Transaction { spec } => {
                let code = suites.get(bench.fixture_path).case(bench.name, spec).entry_bytecode();
                codes.extend(code.map(|code| (bench.name.to_owned(), code)));
            }
            BenchKind::TransactionSuite { spec } => {
                let suite = suites.get(bench.fixture_path);
                for name in suite.case_names() {
                    let code = suite.case(name, spec).entry_bytecode();
                    codes.extend(code.map(|code| (name.to_owned(), code)));
                }
            }
            BenchKind::BlockchainReplay => {}
        }
    }
    codes
}

fn bytecode(c: &mut Criterion) {
    let mut inputs = bundled();
    let bundled_len = inputs.len();
    inputs.push((LARGE_NAME.to_owned(), hex::decode(LARGE_HEX.trim()).unwrap().into()));
    let large = inputs.pop().unwrap();
    let all = inputs.iter().map(|(_, code)| code.clone()).collect::<Vec<_>>();
    let all_bytes = all.iter().map(|code| code.len()).sum::<usize>();
    eprintln!(
        "{bundled_len} bundled contracts, {all_bytes} bytes; {LARGE_NAME}: {} bytes",
        large.1.len()
    );
    let large = large.1;

    let mut group = c.benchmark_group("bytecode");
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(2));
    group.sample_size(50);

    // `inputs` is `(label, contracts)`: every bundled contract, or the large one.
    let cases = [("bundled", all.as_slice()), (LARGE_NAME, std::slice::from_ref(&large))];
    for (label, codes) in cases {
        group.bench_function(format!("instructions/{label}"), |b| {
            b.iter(|| {
                let mut sum = 0usize;
                for code in codes {
                    for inst in Instructions::new(black_box(code), SPEC) {
                        sum += inst.pc + inst.immediate.len() + inst.opcode.get() as usize;
                    }
                }
                black_box(sum)
            })
        });

        group.bench_function(format!("analyze_legacy/{label}"), |b| {
            b.iter(|| {
                for code in codes {
                    black_box(analyze_legacy(black_box(code)));
                }
            })
        });

        group.bench_function(format!("instruction_map_new/{label}"), |b| {
            b.iter(|| {
                for code in codes {
                    black_box(InstructionMap::new(black_box(code), SPEC));
                }
            })
        });

        let maps = codes.iter().map(|code| InstructionMap::new(code, SPEC)).collect::<Vec<_>>();
        group.bench_function(format!("instruction_map_lookup/{label}"), |b| {
            b.iter(|| {
                let mut sum = 0usize;
                for (code, map) in codes.iter().zip(&maps) {
                    for pc in 0..code.len() {
                        sum += map.ic(black_box(pc)).unwrap_or(0);
                    }
                    for ic in 0..map.len() {
                        sum += map.pc(black_box(ic)).unwrap_or(0);
                    }
                }
                black_box(sum)
            })
        });

        group.bench_function(format!("push_value/{label}"), |b| {
            b.iter(|| {
                let mut sum = U256::ZERO;
                for code in codes {
                    for inst in Instructions::new(black_box(code), SPEC) {
                        if let Some(value) = inst.push_value() {
                            sum ^= value;
                        }
                    }
                }
                black_box(sum)
            })
        });
    }
    group.finish();
}

criterion_group!(benches, bytecode);
criterion_main!(benches);
