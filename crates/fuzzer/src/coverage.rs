use crate::{
    case::EvmCase,
    normalize::{Outcome, OutcomeKind},
};
use evm2::SpecId;
use std::collections::BTreeMap;

#[derive(Debug, Default)]
pub(crate) struct Coverage {
    cases: u64,
    forks: BTreeMap<&'static str, u64>,
    tx_kinds: BTreeMap<&'static str, u64>,
    txs_per_case: BTreeMap<usize, u64>,
    features: BTreeMap<String, u64>,
    outcomes: BTreeMap<&'static str, u64>,
    receipt_outcomes: BTreeMap<&'static str, u64>,
    errors: BTreeMap<String, u64>,
}

impl Coverage {
    pub(crate) fn record_case(&mut self, case: &EvmCase) {
        self.cases += 1;
        inc(&mut self.forks, spec_name(case.spec));
        let tx_count = case.txs().count();
        inc(&mut self.txs_per_case, tx_count);
        for tx in case.txs() {
            inc(&mut self.tx_kinds, tx.kind.name());
            if tx.is_create() {
                inc_string(&mut self.features, "tx_create");
            }
            if let Some(precompile) = tx.direct_precompile() {
                inc_string(&mut self.features, "precompile_direct_tx");
                inc_string(&mut self.features, precompile.feature());
                inc_string(
                    &mut self.features,
                    format!("precompile_input_{}", tx.precompile_input_shape(precompile)),
                );
                if !precompile.is_enabled(case.spec) {
                    inc_string(&mut self.features, "precompile_future_address");
                }
            }
            if !tx.kind.is_enabled(case.spec) {
                inc_string(&mut self.features, "fork_invalid_tx");
            }
        }
        for feature in &case.features {
            inc_string(&mut self.features, feature);
        }
    }

    pub(crate) fn record_outcome(&mut self, outcome: &Outcome) {
        inc(&mut self.outcomes, outcome_kind_name(outcome.kind));
        for receipt in &outcome.receipts {
            inc(&mut self.receipt_outcomes, outcome_kind_name(receipt.kind));
            if let Some(error) = &receipt.error {
                inc_string(&mut self.errors, error);
            }
        }
    }

    pub(crate) fn print(&self) {
        if self.cases == 0 {
            return;
        }
        println!("coverage:");
        println!("  cases: {}", self.cases);
        print_counts("forks", &self.forks);
        print_counts("tx kinds", &self.tx_kinds);
        print_counts("txs/case", &self.txs_per_case);
        print_counts("features", &self.features);
        print_counts("outcomes", &self.outcomes);
        print_counts("receipt outcomes", &self.receipt_outcomes);
        print_counts("errors", &self.errors);
    }
}

fn inc<K: Ord>(counts: &mut BTreeMap<K, u64>, key: K) {
    *counts.entry(key).or_default() += 1;
}

fn inc_string(counts: &mut BTreeMap<String, u64>, key: impl AsRef<str>) {
    *counts.entry(key.as_ref().to_string()).or_default() += 1;
}

fn print_counts<K: std::fmt::Display>(label: &str, counts: &BTreeMap<K, u64>) {
    if counts.is_empty() {
        return;
    }
    println!("  {label}:");
    for (key, count) in counts {
        println!("    {key}: {count}");
    }
}

const fn outcome_kind_name(kind: OutcomeKind) -> &'static str {
    match kind {
        OutcomeKind::Success => "success",
        OutcomeKind::RevertOrHalt => "revert_or_halt",
        OutcomeKind::Error => "error",
    }
}

const fn spec_name(spec: SpecId) -> &'static str {
    match spec {
        SpecId::FRONTIER => "frontier",
        SpecId::HOMESTEAD => "homestead",
        SpecId::TANGERINE => "tangerine",
        SpecId::SPURIOUS_DRAGON => "spurious_dragon",
        SpecId::BYZANTIUM => "byzantium",
        SpecId::ISTANBUL => "istanbul",
        SpecId::BERLIN => "berlin",
        SpecId::LONDON => "london",
        SpecId::SHANGHAI => "shanghai",
        SpecId::CANCUN => "cancun",
        SpecId::PRAGUE => "prague",
        SpecId::OSAKA => "osaka",
        SpecId::AMSTERDAM => "amsterdam",
        _ => "other",
    }
}
