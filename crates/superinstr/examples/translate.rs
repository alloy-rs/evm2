//! Measures translation throughput over the contracts in the repository's benchmark fixtures.
//!
//! Usage: `cargo run -p evm2-superinstr --example translate --release [iterations]`

use alloy_primitives::Bytes;
use evm2::{BaseEvmTypes, ExecutionConfig, SpecId, bytecode::Bytecode};
use serde::Deserialize;
use std::{collections::BTreeMap, fs, time::Instant};

fn main() {
    let iterations: usize = std::env::args().nth(1).and_then(|n| n.parse().ok()).unwrap_or(20);
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../data");
    let mut codes = Vec::new();
    for entry in fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let text = fs::read_to_string(&path).unwrap();
        let fixture: Fixture = serde_json::from_str(&text)
            .unwrap_or_else(|err| panic!("failed to parse {}: {err}", path.display()));
        let accounts = fixture.into_values().flat_map(|unit| unit.pre.into_values());
        codes.extend(accounts.map(|account| account.code).filter(|code| !code.is_empty()));
    }
    codes.sort();
    codes.dedup();
    let config = ExecutionConfig::<BaseEvmTypes>::for_spec_and_version(
        SpecId::OSAKA,
        *evm2::Version::base(SpecId::OSAKA),
    );
    let bytecodes: Vec<_> = codes.iter().map(|code| Bytecode::new_raw(code.clone())).collect();
    let bytes: usize = codes.iter().map(|code| code.len()).sum();
    let mut records = 0;
    let start = Instant::now();
    for _ in 0..iterations {
        for bytecode in &bytecodes {
            records += evm2_superinstr::translate_records(bytecode, &config);
        }
    }
    let elapsed = start.elapsed();
    println!(
        "{} contracts, {bytes} bytes: {:.1} ns/byte, {:.2} records/byte",
        codes.len(),
        elapsed.as_nanos() as f64 / (bytes * iterations) as f64,
        records as f64 / (bytes * iterations) as f64,
    );
}

/// A state test fixture: its test units by name.
type Fixture = BTreeMap<String, Unit>;

/// The part of a state test unit this example reads.
#[derive(Deserialize)]
struct Unit {
    pre: BTreeMap<String, Account>,
}

#[derive(Deserialize)]
struct Account {
    code: Bytes,
}
