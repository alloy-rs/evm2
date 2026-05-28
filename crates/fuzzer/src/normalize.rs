use alloy_primitives::{Address, B256, U256};
use evm2::evm::StateChanges;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Outcome {
    pub(crate) kind: OutcomeKind,
    pub(crate) gas_used: Option<u64>,
    pub(crate) output: Option<Vec<u8>>,
    pub(crate) logs: Vec<CanonicalLog>,
    pub(crate) state: CanonicalState,
    pub(crate) error: Option<String>,
    pub(crate) receipts: Vec<TxReceipt>,
}

impl Outcome {
    pub(crate) fn from_receipts(receipts: Vec<TxReceipt>) -> Self {
        let Some(last) = receipts.last() else {
            return Self::error("empty transaction sequence".to_string());
        };
        Self {
            kind: last.kind,
            gas_used: last.gas_used,
            output: last.output.clone(),
            logs: receipts.iter().flat_map(|receipt| receipt.logs.clone()).collect(),
            state: last.state.clone(),
            error: last.error.clone(),
            receipts,
        }
    }

    pub(crate) fn error(error: String) -> Self {
        let receipt = TxReceipt::error(error);
        Self {
            kind: receipt.kind,
            gas_used: receipt.gas_used,
            output: receipt.output.clone(),
            logs: Vec::new(),
            state: CanonicalState::default(),
            error: receipt.error.clone(),
            receipts: vec![receipt],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TxReceipt {
    pub(crate) kind: OutcomeKind,
    pub(crate) gas_used: Option<u64>,
    pub(crate) output: Option<Vec<u8>>,
    pub(crate) logs: Vec<CanonicalLog>,
    pub(crate) state: CanonicalState,
    pub(crate) error: Option<String>,
}

impl TxReceipt {
    pub(crate) fn error(error: String) -> Self {
        Self {
            kind: OutcomeKind::Error,
            gas_used: None,
            output: None,
            logs: Vec::new(),
            state: CanonicalState::default(),
            error: Some(normalize_error(error)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OutcomeKind {
    Success,
    RevertOrHalt,
    Error,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CanonicalState {
    pub(crate) accounts: BTreeMap<Address, Option<CanonicalAccount>>,
    pub(crate) storage: BTreeMap<(Address, U256), U256>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CanonicalAccount {
    pub(crate) balance: U256,
    pub(crate) nonce: u64,
    pub(crate) code_hash: B256,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CanonicalLog {
    pub(crate) address: Address,
    pub(crate) topics: Vec<B256>,
    pub(crate) data: Vec<u8>,
}

fn normalize_error(error: String) -> String {
    let error = error
        .strip_prefix("Transaction(")
        .and_then(|error| error.strip_suffix(')'))
        .unwrap_or(&error);
    if error.starts_with("IntrinsicGasTooLow") || error.starts_with("CallGasCostMoreThanGasLimit") {
        return "IntrinsicGasTooLow".to_string();
    }
    if error.starts_with("LackOfFundForMaxFee") || error == "InsufficientFunds" {
        return "InsufficientFunds".to_string();
    }
    if error.starts_with("UnsupportedTransactionType")
        || error.ends_with("NotSupported") && error.starts_with("Eip")
    {
        return "UnsupportedTransactionType".to_string();
    }
    error.to_string()
}

pub(crate) fn state_from_evm2_changes(changes: &StateChanges) -> CanonicalState {
    let mut state = CanonicalState::default();
    for (&address, change) in &changes.accounts {
        let account = change.current.as_ref().map(|info| CanonicalAccount {
            balance: info.balance,
            nonce: info.nonce,
            code_hash: info.code_hash,
        });
        state.accounts.insert(address, account);
    }
    for (&address, storage) in &changes.storage {
        for (&key, change) in &storage.slots {
            if !change.current.is_zero() {
                state.storage.insert((address, key), change.current);
            }
        }
    }
    state
}

pub(crate) fn state_from_revm(state: revm::state::EvmState) -> CanonicalState {
    let mut canonical = CanonicalState::default();
    for (address, account) in state {
        let account_changed = account.info.balance != account.original_info.balance
            || account.info.nonce != account.original_info.nonce
            || account.info.code_hash != account.original_info.code_hash;
        if account.is_selfdestructed() {
            if !account.original_info.is_empty() {
                canonical.accounts.insert(address, None);
            }
            continue;
        }
        if account_changed || account.is_created() {
            canonical.accounts.insert(
                address,
                Some(CanonicalAccount {
                    balance: account.info.balance,
                    nonce: account.info.nonce,
                    code_hash: account.info.code_hash,
                }),
            );
        }
        for (key, slot) in account.changed_storage_slots() {
            if !slot.present_value().is_zero() {
                canonical.storage.insert((address, *key), slot.present_value());
            }
        }
    }
    canonical
}

pub(crate) fn canonical_log(log: &alloy_primitives::Log) -> CanonicalLog {
    CanonicalLog {
        address: log.address,
        topics: log.data.topics().to_vec(),
        data: log.data.data.to_vec(),
    }
}
