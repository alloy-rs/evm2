use crate::fuzzer::{
    case::{EvmCase, FuzzTxKind},
    features::FuzzFeatures,
    normalize::{FuzzError, FuzzOutcomeKind, Outcome},
};
use alloy_primitives::map::HashMap;
use evm2::SpecId;
use std::hash::Hash;

#[derive(Debug, Default)]
pub(crate) struct Coverage {
    cases: u64,
    forks: HashMap<SpecId, u64>,
    tx_kinds: HashMap<FuzzTxKind, u64>,
    txs_per_case: HashMap<usize, u64>,
    features: HashMap<FuzzFeatures, u64>,
    outcomes: HashMap<FuzzOutcomeKind, u64>,
    receipt_outcomes: HashMap<FuzzOutcomeKind, u64>,
    errors: HashMap<FuzzError, u64>,
}

impl Coverage {
    pub(crate) fn record_case(&mut self, case: &EvmCase) {
        self.cases += 1;
        inc(&mut self.forks, case.spec);
        let tx_count = case.txs().count();
        inc(&mut self.txs_per_case, tx_count);
        for tx in case.txs() {
            inc(&mut self.tx_kinds, tx.kind);
            if tx.is_create() {
                inc(&mut self.features, FuzzFeatures::TX_CREATE);
            }
            if let Some(precompile) = tx.direct_precompile() {
                inc(&mut self.features, FuzzFeatures::PRECOMPILE_DIRECT_TX);
                inc(&mut self.features, precompile.feature());
                inc(&mut self.features, tx.precompile_input_shape(precompile));
                if !precompile.is_enabled(case.spec) {
                    inc(&mut self.features, FuzzFeatures::PRECOMPILE_FUTURE_ADDRESS);
                }
            }
            if !tx.kind.is_enabled(case.spec) {
                inc(&mut self.features, FuzzFeatures::FORK_INVALID_TX);
            }
        }
        for feature in case.features.iter() {
            inc(&mut self.features, feature);
        }
    }

    pub(crate) fn record_outcome(&mut self, outcome: &Outcome) {
        inc(&mut self.outcomes, outcome.kind);
        for receipt in &outcome.receipts {
            inc(&mut self.receipt_outcomes, receipt.kind);
            if let Some(error) = &receipt.error {
                inc(&mut self.errors, error.clone());
            }
        }
    }

    pub(crate) fn merge(&mut self, other: Self) {
        self.cases += other.cases;
        merge_counts(&mut self.forks, other.forks);
        merge_counts(&mut self.tx_kinds, other.tx_kinds);
        merge_counts(&mut self.txs_per_case, other.txs_per_case);
        merge_counts(&mut self.features, other.features);
        merge_counts(&mut self.outcomes, other.outcomes);
        merge_counts(&mut self.receipt_outcomes, other.receipt_outcomes);
        merge_counts(&mut self.errors, other.errors);
    }

    pub(crate) fn print(&self) {
        if self.cases == 0 {
            return;
        }
        println!("coverage:");
        println!("  cases: {}", self.cases);
        print_counts("forks", &self.forks, |spec| format!("{spec:?}").to_lowercase());
        print_counts("tx kinds", &self.tx_kinds, ToString::to_string);
        print_counts("txs/case", &self.txs_per_case, ToString::to_string);
        print_counts("features", &self.features, ToString::to_string);
        print_counts("outcomes", &self.outcomes, ToString::to_string);
        print_counts("receipt outcomes", &self.receipt_outcomes, ToString::to_string);
        print_counts("errors", &self.errors, ToString::to_string);
    }
}

fn inc<K: Eq + Hash>(counts: &mut HashMap<K, u64>, key: K) {
    *counts.entry(key).or_default() += 1;
}

fn merge_counts<K: Eq + Hash>(counts: &mut HashMap<K, u64>, other: HashMap<K, u64>) {
    for (key, count) in other {
        *counts.entry(key).or_default() += count;
    }
}

fn print_counts<K>(label: &str, counts: &HashMap<K, u64>, name: impl Fn(&K) -> String) {
    if counts.is_empty() {
        return;
    }
    println!("  {label}:");
    let mut counts = counts.iter().map(|(key, count)| (name(key), count)).collect::<Vec<_>>();
    counts.sort_unstable_by(|(a, _), (b, _)| a.cmp(b));
    for (key, count) in counts {
        println!("    {key}: {count}");
    }
}
