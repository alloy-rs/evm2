//! Gas-cost-estimator benchmark, ported from revm's `revme bench gas-cost-estimator`.
//!
//! Each row of [`SAMPLE_CSV`] is a standalone program that repeats a single opcode a fixed number
//! of times. Benchmarking every sample produces a per-opcode wall-clock signal that the upstream
//! [gas-cost-estimator] project uses to estimate the marginal cost of each instruction.
//!
//! Run all samples with `cargo bench --bench gas_cost_estimator`, or a subset with Criterion's
//! built-in name filter, for example `cargo bench --bench gas_cost_estimator -- 'SSTORE|SLOAD'`.
//!
//! [gas-cost-estimator]: https://github.com/imapp-pl/gas-cost-estimator

#![allow(missing_docs, clippy::missing_const_for_fn)]

use alloy_consensus::{TxLegacy, transaction::Recovered};
use alloy_primitives::{Address, Bytes, TxKind, U256, address, hex};
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use evm2::{
    BaseEvmTypes, Evm, ExecutedTx, Precompiles, SpecId, Version,
    bytecode::Bytecode,
    env::BlockEnv,
    ethereum::{RecoveredTxEnvelope, ethereum_tx_registry},
    evm::{AccountInfo, InMemoryDB},
};
use std::io::Cursor;

/// Contract address that holds the sample bytecode.
const BENCH_TARGET: Address = address!("0xffffffffffffffffffffffffffffffffffffffff");
/// Caller that invokes the sample contract.
const BENCH_CALLER: Address = address!("0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee");
/// Balance seeded for both bench accounts.
const BENCH_BALANCE: U256 = U256::from_limbs([10_000_000_000_000_000, 0, 0, 0]);

/// CSV of gas-cost-estimator sample programs: `program_id,opcode,op_count,bytecode`.
const SAMPLE_CSV: &str = include_str!("gas_cost_estimator_sample.csv");

/// Spec the samples are executed against.
const SPEC: SpecId = SpecId::OSAKA;

/// Benchmarks every gas-cost-estimator sample. Criterion applies any name filter passed after `--`.
fn gas_cost_estimator(c: &mut Criterion) {
    let mut group = c.benchmark_group("gas_cost_estimator");
    let mut reader = csv::Reader::from_reader(Cursor::new(SAMPLE_CSV));
    for record in reader.records() {
        let record = record.expect("failed to read sample record");
        let name = record[0].trim();
        let bytecode_hex = record[3].trim();
        let Ok(bytes) = hex::decode(bytecode_hex) else {
            continue;
        };
        let sample = PreparedSample::new(name.to_string(), Bytes::from(bytes));
        sample.sanity_check();

        group.bench_function(&sample.name, |b| {
            b.iter_batched(
                || (sample.new_evm(), sample.tx.clone()),
                |(mut evm, tx)| sample.execute(&mut evm, &tx),
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

/// A single sample program with the database and transaction needed to execute it.
struct PreparedSample {
    name: String,
    db: InMemoryDB,
    tx: RecoveredTxEnvelope,
}

impl PreparedSample {
    fn new(name: String, bytecode: Bytes) -> Self {
        let mut db = InMemoryDB::default();

        let target = AccountInfo::default()
            .with_code(Bytecode::new_legacy(bytecode))
            .with_balance(BENCH_BALANCE);
        db.insert_account_info(&BENCH_TARGET, target);

        let caller = AccountInfo::default().with_balance(BENCH_BALANCE);
        db.insert_account_info(&BENCH_CALLER, caller);

        // `gas_price` is zero so the caller never needs balance to cover the fee, and the gas limit
        // is capped to the spec's transaction limit so every sample has plenty of headroom.
        let tx = TxLegacy {
            chain_id: None,
            nonce: 0,
            gas_price: 0,
            gas_limit: Version::base(SPEC).tx_gas_limit_cap,
            to: TxKind::Call(BENCH_TARGET),
            value: U256::ZERO,
            input: Bytes::new(),
        };
        let tx = RecoveredTxEnvelope::Legacy(Recovered::new_unchecked(tx, BENCH_CALLER));

        Self { name, db, tx }
    }

    /// Executes the sample transaction and commits its state, returning the gas used. Panics on a
    /// handler error so a broken sample never produces silent, meaningless timings.
    fn execute(&self, evm: &mut Evm<BaseEvmTypes>, tx: &RecoveredTxEnvelope) -> u64 {
        evm.transact(tx)
            .map(ExecutedTx::commit)
            .unwrap_or_else(|err| panic!("sample {} must execute: {err:?}", self.name))
            .gas_used
    }

    /// Runs the sample once and asserts it executes successfully, catching out-of-gas or reverting
    /// programs before they are benchmarked.
    fn sanity_check(&self) {
        let mut evm = self.new_evm();
        let result = evm
            .transact(&self.tx)
            .map(ExecutedTx::commit)
            .unwrap_or_else(|err| panic!("sample {} must execute: {err:?}", self.name));
        assert!(
            result.status,
            "sample {} must succeed, stopped with {:?}",
            self.name, result.stop
        );
    }

    /// Builds a fresh EVM over a clone of the sample database so each iteration starts from the
    /// same nonce and storage state.
    fn new_evm(&self) -> Evm<BaseEvmTypes> {
        Evm::new(
            SPEC,
            BlockEnv::default(),
            ethereum_tx_registry(SPEC),
            self.db.clone(),
            Precompiles::base(SPEC),
        )
    }
}

criterion_group!(benches, gas_cost_estimator);
criterion_main!(benches);
