//! Measure code chunk coverage and tracing overhead on the bundled ten-block mainnet replay.

use alloy_primitives::{Address, B256};
use evm2::{
    BaseEvmTypes, Evm, TxResult, bytecode::chunks::CODE_CHUNK_SIZE, evm::inspector::NoopInspector,
};
use evm2_eest::{
    BlockchainTestExecuteConfig, NameFilter,
    blockchaintest::{BlockFinished, Hook},
    execute_blockchain_tests_suite, read_blockchain_fixture,
};
use evm2_inspectors::code_chunks::{CodeChunkInspector, CodeCoverage};
use std::{collections::BTreeMap, hint::black_box, path::Path, time::Instant};

#[derive(Default)]
struct ReplayHook {
    mode: usize,
    chunk_size: usize,
    collect: bool,
    outcomes: Vec<TxResult>,
    block_codes: BTreeMap<(Address, B256), CodeCoverage>,
    // Sum of transaction-local coverage; unlike block union, repeated contracts count per tx.
    tx_full: usize,
    tx_payload: usize,
    transactions: usize,
    successes: usize,
}

impl Hook for ReplayHook {
    fn evm_transaction_start(&mut self, evm: &mut Evm<'static, BaseEvmTypes>) {
        match self.mode {
            1 => evm.set_inspector(NoopInspector::default()),
            2 => evm.set_inspector(CodeChunkInspector::new(self.chunk_size)),
            _ => {}
        }
    }

    fn evm_transaction_end(&mut self, evm: &mut Evm<'static, BaseEvmTypes>, result: &TxResult) {
        if self.collect {
            self.outcomes.push(result.clone());
            self.transactions += 1;
            self.successes += usize::from(result.status);
        }
        if self.mode == 2 {
            let trace = evm.clear_inspector_as::<CodeChunkInspector>().unwrap();
            if self.collect {
                for code in trace.codes() {
                    self.tx_full += code.code_len;
                    self.tx_payload += code.covered_bytes(self.chunk_size);
                    self.block_codes
                        .entry((code.address, code.code_hash))
                        .and_modify(|existing| {
                            assert_eq!(existing.code_len, code.code_len);
                            for (dst, src) in existing.touched.iter_mut().zip(&code.touched) {
                                *dst |= src;
                            }
                        })
                        .or_insert_with(|| code.clone());
                }
            }
        } else if self.mode == 1 {
            evm.clear_inspector();
        }
    }

    fn block_finished(&mut self, event: BlockFinished) {
        if self.collect && self.mode == 2 {
            let full: usize = self.block_codes.values().map(|c| c.code_len).sum();
            let covered: usize =
                self.block_codes.values().map(|c| c.covered_bytes(self.chunk_size)).sum();
            let touched: usize = self.block_codes.values().map(|c| c.touched_chunks()).sum();
            let total: usize = self.block_codes.values().map(|c| c.touched.len()).sum();
            println!(
                "{},{},{},{},{},{},{},{},{},{},{},{}",
                event.block_number.unwrap(),
                self.chunk_size,
                self.transactions,
                self.successes,
                self.block_codes.len(),
                full,
                covered,
                covered,
                touched,
                total,
                self.tx_full,
                self.tx_payload
            );
        }
        self.block_codes.clear();
        self.tx_full = 0;
        self.tx_payload = 0;
        self.transactions = 0;
        self.successes = 0;
    }
}

fn main() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/mainnet-25347446-25347455.bin.zst");
    let suite = read_blockchain_fixture(&path).unwrap();
    let replay = |hook: &mut ReplayHook| {
        let summary = execute_blockchain_tests_suite(
            &path,
            &suite,
            BlockchainTestExecuteConfig { compare_receipt_root: true, ..Default::default() },
            &NameFilter::default(),
            hook,
        )
        .unwrap();
        assert_eq!(summary.executed, 1);
        assert_eq!(summary.skipped, 0);
    };
    let mut baseline = ReplayHook { collect: true, ..Default::default() };
    replay(&mut baseline);
    println!(
        "block,chunk_size,transactions,successful,contracts,full_code_bytes,covered_bytes,chunk_payload_bytes,touched_chunks,total_chunks,tx_sum_full_bytes,tx_sum_payload_bytes"
    );
    {
        let chunk_size = CODE_CHUNK_SIZE;
        let mut traced = ReplayHook { mode: 2, chunk_size, collect: true, ..Default::default() };
        replay(&mut traced);
        assert_eq!(traced.outcomes, baseline.outcomes, "chunk trace changed replay outcomes");
    }
    let mut timings = [Vec::new(), Vec::new(), Vec::new()];
    for sample in 0..7 {
        for rotation in 0..3 {
            let mode = (sample + rotation) % 3;
            let mut hook = ReplayHook { mode, chunk_size: CODE_CHUNK_SIZE, ..Default::default() };
            let start = Instant::now();
            replay(&mut hook);
            timings[mode].push(start.elapsed().as_secs_f64() * 1000.0);
            black_box(hook);
        }
    }
    for samples in &mut timings {
        samples.sort_by(f64::total_cmp);
    }
    eprintln!(
        "mainnet,transactions={},baseline_ms={:.3},noop_ms={:.3},trace_ms={:.3},trace_over_baseline={:.3},trace_over_noop={:.3},baseline_range_ms={:.3}..{:.3},trace_range_ms={:.3}..{:.3}",
        baseline.outcomes.len(),
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
