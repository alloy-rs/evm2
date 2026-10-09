//! Run with `cargo run --release -p evm2-cli --example bytecode_chunks`.
//! Optional arguments select named transaction fixtures from the benchmark catalog.

use evm2::{
    BaseEvmTypes, Evm, Precompiles, bytecode::chunks::CODE_CHUNK_SIZE,
    ethereum::ethereum_tx_registry, evm::inspector::NoopInspector,
};
use evm2_cli::evm_bench::{BENCHES, BenchKind};
use evm2_inspectors::code_chunks::CodeChunkInspector;
use std::{env, hint::black_box, time::Instant};

#[allow(dead_code, clippy::missing_const_for_fn)]
#[path = "../benches/evm/fixture.rs"]
mod fixture;

fn main() {
    let names: Vec<_> = env::args().skip(1).collect();
    let default_names = [
        "weth",
        "erc20_transfer",
        "usdc_proxy",
        "fiat_token",
        "uniswap_v2_pair",
        "univ2_router",
        "seaport",
        "fibonacci-calldata",
    ];
    let benches: Vec<_> = BENCHES
        .iter()
        .filter(|bench| {
            if names.is_empty() {
                default_names.contains(&bench.name)
            } else {
                names.iter().any(|name| name == bench.name)
            }
        })
        .collect();
    assert!(!benches.is_empty(), "no matching fixtures");
    let suites =
        fixture::Suites::load(benches.iter().filter_map(|bench| bench.transaction_fixture_path()));
    println!(
        "fixture,chunk_size,contracts,full_code_bytes,covered_bytes,chunk_payload_bytes,touched_chunks,total_chunks"
    );
    for bench in &benches {
        let BenchKind::Transaction { spec } = bench.kind else {
            panic!("select transaction fixtures")
        };
        let case = suites.get(bench.fixture_path).case(bench.name, spec);
        let db = case.state();
        let tx = case.tx(spec);
        let new_evm = || {
            Evm::<BaseEvmTypes>::new(
                spec,
                case.block(),
                ethereum_tx_registry(spec),
                db.clone(),
                Precompiles::base(spec),
            )
        };
        let baseline = new_evm().transact(&tx).unwrap().detach();
        eprintln!(
            "outcome,{},{:?},gas={}",
            bench.name,
            baseline.result.stop,
            baseline.result.tx_gas_used()
        );
        {
            let chunk_size = CODE_CHUNK_SIZE;
            let mut evm = new_evm();
            evm.set_inspector(CodeChunkInspector::new(chunk_size));
            let actual = evm.transact(&tx).unwrap().detach();
            assert_eq!(actual, baseline, "{}: tracing changed execution", bench.name);
            let trace = evm.clear_inspector_as::<CodeChunkInspector>().unwrap();
            let codes = trace.codes();
            let full: usize = codes.iter().map(|c| c.code_len).sum();
            let covered: usize = codes.iter().map(|c| c.covered_bytes(chunk_size)).sum();
            let touched: usize = codes.iter().map(|c| c.touched_chunks()).sum();
            let total: usize = codes.iter().map(|c| c.touched.len()).sum();
            println!(
                "{},{chunk_size},{},{full},{covered},{},{touched},{total}",
                bench.name,
                codes.len(),
                covered
            );
        }

        // Setup, DB clone, and inspector construction are outside the timed region.
        // Each iteration gets a fresh EVM; bytecode and jump analysis are shared/prewarmed.
        let pilot = Instant::now();
        for _ in 0..10 {
            let _ = black_box(new_evm().transact(&tx).unwrap().commit());
        }
        let iterations =
            (20_000_000 / (pilot.elapsed().as_nanos() / 10).max(1)).clamp(5, 2000) as usize;
        let mut timings = [Vec::new(), Vec::new(), Vec::new()];
        for sample in 0..7 {
            for rotation in 0..3 {
                let mode = (sample + rotation) % 3;
                let mut elapsed = 0;
                for _ in 0..iterations {
                    let mut evm = new_evm();
                    match mode {
                        1 => evm.set_inspector(NoopInspector::default()),
                        2 => evm.set_inspector(CodeChunkInspector::new(CODE_CHUNK_SIZE)),
                        _ => {}
                    }
                    let start = Instant::now();
                    let result = evm.transact(black_box(&tx)).unwrap().commit();
                    elapsed += start.elapsed().as_nanos();
                    assert_eq!(black_box(result), baseline.result);
                }
                timings[mode].push(elapsed as f64 / iterations as f64);
            }
        }
        for values in &mut timings {
            values.sort_by(f64::total_cmp);
        }
        eprintln!(
            "timing,{},{iterations},baseline_ns={:.0},noop_ns={:.0},trace_ns={:.0},trace_over_baseline={:.3},trace_over_noop={:.3},baseline_range_ns={:.0}..{:.0},trace_range_ns={:.0}..{:.0}",
            bench.name,
            timings[0][3],
            timings[1][3],
            timings[2][3],
            timings[2][3] / timings[0][3],
            timings[2][3] / timings[1][3],
            timings[0][0],
            timings[0][6],
            timings[2][0],
            timings[2][6]
        );
    }
}
