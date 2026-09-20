use crate::fuzzer::case::EvmCase;
use alloy_primitives::{Address, B256, U256, keccak256, map::HashMap};
use core::convert::Infallible;
use evm2::evm::{
    AccountChangeRef, PendingState, StateChangeSink, StateChangeSource, StorageChange,
    registry::HandlerError,
};
use revm::{
    context_interface::result::{EVMError, InvalidTransaction},
    database::bal::EvmDatabaseError,
};
use std::{
    fmt,
    hash::{Hash, Hasher},
    mem::discriminant,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Outcome {
    pub(crate) kind: FuzzOutcomeKind,
    pub(crate) gas_used: Option<u64>,
    pub(crate) output: Option<Vec<u8>>,
    pub(crate) logs: Vec<CanonicalLog>,
    pub(crate) state: CanonicalState,
    pub(crate) error: Option<FuzzError>,
    pub(crate) receipts: Vec<TxReceipt>,
}

impl Outcome {
    pub(crate) fn from_receipts(receipts: Vec<TxReceipt>) -> Self {
        let Some(last) = receipts.last() else {
            return Self::error(FuzzError::EmptyTransactionSequence);
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

    pub(crate) fn error(error: FuzzError) -> Self {
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
    pub(crate) kind: FuzzOutcomeKind,
    pub(crate) gas_used: Option<u64>,
    pub(crate) output: Option<Vec<u8>>,
    pub(crate) logs: Vec<CanonicalLog>,
    pub(crate) state: CanonicalState,
    pub(crate) error: Option<FuzzError>,
}

impl TxReceipt {
    pub(crate) fn error(error: FuzzError) -> Self {
        Self {
            kind: FuzzOutcomeKind::Error,
            gas_used: None,
            output: None,
            logs: Vec::new(),
            state: CanonicalState::default(),
            error: Some(error),
        }
    }
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub(crate) enum FuzzOutcomeKind {
    Success,
    RevertOrHalt,
    Error,
}

impl fmt::Display for FuzzOutcomeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Success => "success",
            Self::RevertOrHalt => "revert_or_halt",
            Self::Error => "error",
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CanonicalState {
    pub(crate) accounts: HashMap<Address, Option<CanonicalAccount>>,
    pub(crate) storage: HashMap<(Address, U256), U256>,
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum FuzzError {
    EmptyTransactionSequence,
    IntrinsicGasTooLow,
    InsufficientFunds,
    InvalidNonce,
    NonceOverflow,
    UnsupportedTransactionType,
    Transaction(InvalidTransaction),
    Evm2(HandlerError),
    Revm(EVMError<EvmDatabaseError<Infallible>>),
}

impl Hash for FuzzError {
    fn hash<H: Hasher>(&self, state: &mut H) {
        discriminant(self).hash(state);
        match self {
            Self::Transaction(error) => error.hash(state),
            // Backend errors lack Hash; equality still compares their full payloads.
            Self::Evm2(error) => discriminant(error).hash(state),
            Self::Revm(error) => discriminant(error).hash(state),
            _ => {}
        }
    }
}

impl fmt::Display for FuzzError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transaction(error) => write!(f, "{error:?}"),
            _ => write!(f, "{self:?}"),
        }
    }
}

impl From<HandlerError> for FuzzError {
    fn from(error: HandlerError) -> Self {
        match error {
            HandlerError::IntrinsicGasTooLow { .. } => Self::IntrinsicGasTooLow,
            HandlerError::InsufficientFunds => Self::InsufficientFunds,
            HandlerError::InvalidNonce { .. } => Self::InvalidNonce,
            HandlerError::NonceOverflow => Self::NonceOverflow,
            HandlerError::UnsupportedTransactionType(_) => Self::UnsupportedTransactionType,
            HandlerError::MissingChainId => Self::Transaction(InvalidTransaction::MissingChainId),
            HandlerError::RejectCallerWithCode => {
                Self::Transaction(InvalidTransaction::RejectCallerWithCode)
            }
            HandlerError::EmptyAuthorizationList => {
                Self::Transaction(InvalidTransaction::EmptyAuthorizationList)
            }
            HandlerError::EmptyBlobs => Self::Transaction(InvalidTransaction::EmptyBlobs),
            HandlerError::TooManyBlobs { have, max } => {
                Self::Transaction(InvalidTransaction::TooManyBlobs { have, max })
            }
            HandlerError::BlobVersionNotSupported => {
                Self::Transaction(InvalidTransaction::BlobVersionNotSupported)
            }
            HandlerError::PriorityFeeGreaterThanMaxFee => {
                Self::Transaction(InvalidTransaction::PriorityFeeGreaterThanMaxFee)
            }
            HandlerError::TxGasLimitGreaterThanCap { gas_limit, cap } => {
                Self::Transaction(InvalidTransaction::TxGasLimitGreaterThanCap { gas_limit, cap })
            }
            error @ (HandlerError::Fatal(_)
            | HandlerError::External(_)
            | HandlerError::WrongTransactionType { .. }
            | HandlerError::InvalidChainId { .. }
            | HandlerError::GasLimitMoreThanBlock { .. }
            | HandlerError::CreateInitCodeSizeLimit { .. }
            | HandlerError::OutOfFunds
            | HandlerError::SignerRecoveryFailed
            | HandlerError::FeeCapLessThanBaseFee { .. }
            | HandlerError::BlobFeeCapLessThanBlobBaseFee { .. }
            | HandlerError::UnsupportedCaller(_)) => Self::Evm2(error),
        }
    }
}

impl From<EVMError<EvmDatabaseError<Infallible>>> for FuzzError {
    fn from(error: EVMError<EvmDatabaseError<Infallible>>) -> Self {
        match error {
            EVMError::Transaction(error) => match error {
                InvalidTransaction::CallGasCostMoreThanGasLimit { .. }
                | InvalidTransaction::GasFloorMoreThanGasLimit { .. } => Self::IntrinsicGasTooLow,
                InvalidTransaction::LackOfFundForMaxFee { .. } => Self::InsufficientFunds,
                InvalidTransaction::NonceTooHigh { .. }
                | InvalidTransaction::NonceTooLow { .. } => Self::InvalidNonce,
                InvalidTransaction::NonceOverflowInTransaction => Self::NonceOverflow,
                InvalidTransaction::Eip2930NotSupported
                | InvalidTransaction::Eip1559NotSupported
                | InvalidTransaction::Eip4844NotSupported
                | InvalidTransaction::Eip7702NotSupported
                | InvalidTransaction::Eip7873NotSupported => Self::UnsupportedTransactionType,
                error => Self::Transaction(error),
            },
            EVMError::Database(EvmDatabaseError::Database(error)) => match error {},
            error => Self::Revm(error),
        }
    }
}

pub(crate) fn state_from_evm2_changes(pending: &PendingState) -> CanonicalState {
    struct Collector(CanonicalState);

    impl StateChangeSink for Collector {
        type Error = Infallible;

        fn account(&mut self, change: AccountChangeRef<'_>) -> Result<(), Self::Error> {
            // A created-then-destroyed account (e.g. a CREATE whose init code selfdestructs) ends
            // the transaction absent with no transaction-boundary original, a net no-op. revm's
            // `state_from_revm` omits such an account, so drop the spurious `None` deletion here to
            // keep the two backends' diffs symmetric.
            if change.current.is_none() && change.original.is_none() {
                return Ok(());
            }
            let account = change.current.map(|info| CanonicalAccount {
                balance: info.balance,
                nonce: info.nonce,
                code_hash: info.code_hash,
            });
            self.0.accounts.insert(change.address, account);
            Ok(())
        }

        fn storage(&mut self, change: StorageChange) -> Result<(), Self::Error> {
            if !change.current.is_zero() {
                self.0.storage.insert((change.address, change.key), change.current);
            }
            Ok(())
        }
    }

    let mut collector = Collector(CanonicalState::default());
    let Ok(()) = pending.visit(&mut collector);
    collector.0
}

pub(crate) fn state_from_revm(
    state: revm::state::EvmState,
    original_accounts: &HashMap<Address, CanonicalAccount>,
) -> CanonicalState {
    let mut canonical = CanonicalState::default();
    for (address, account) in state {
        let changed_storage_slots = account.changed_storage_slots().collect::<Vec<_>>();
        if !account.is_touched()
            && !account.is_created()
            && !account.is_selfdestructed()
            && changed_storage_slots.is_empty()
        {
            continue;
        }

        let original = original_accounts.get(&address);
        let account_changed = original.map_or_else(
            || {
                account.info.balance != account.original_info().balance
                    || account.info.nonce != account.original_info().nonce
                    || account.info.code_hash != account.original_info().code_hash
            },
            |original| {
                account.info.balance != original.balance
                    || account.info.nonce != original.nonce
                    || account.info.code_hash != original.code_hash
            },
        );
        if account.is_selfdestructed() {
            if original.is_some() || !account.original_info().is_empty() {
                canonical.accounts.insert(address, None);
            }
            continue;
        }
        if account_changed || account.is_created() && original.is_none() {
            canonical.accounts.insert(
                address,
                Some(CanonicalAccount {
                    balance: account.info.balance,
                    nonce: account.info.nonce,
                    code_hash: account.info.code_hash,
                }),
            );
        }
        for (key, slot) in changed_storage_slots {
            if !slot.present_value().is_zero() {
                canonical.storage.insert((address, *key), slot.present_value());
            }
        }
    }
    canonical
}

pub(crate) fn canonical_accounts(case: &EvmCase) -> HashMap<Address, CanonicalAccount> {
    case.accounts
        .iter()
        .map(|account| {
            (
                account.address,
                CanonicalAccount {
                    balance: account.balance,
                    nonce: account.nonce,
                    code_hash: keccak256(&account.code),
                },
            )
        })
        .collect()
}

pub(crate) fn apply_account_changes(
    accounts: &mut HashMap<Address, CanonicalAccount>,
    state: &CanonicalState,
) {
    for (&address, account) in &state.accounts {
        match account {
            Some(account) => {
                accounts.insert(address, account.clone());
            }
            None => {
                accounts.remove(&address);
            }
        }
    }
}

pub(crate) fn canonical_log(log: &alloy_primitives::Log) -> CanonicalLog {
    CanonicalLog {
        address: log.address,
        topics: log.data.topics().to_vec(),
        data: log.data.data.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalized_error_categories() {
        for (evm2, revm) in [
            (
                HandlerError::IntrinsicGasTooLow { required: 21_000, got: 1 },
                InvalidTransaction::CallGasCostMoreThanGasLimit {
                    initial_gas: 21_000,
                    gas_limit: 1,
                },
            ),
            (
                HandlerError::IntrinsicGasTooLow { required: 66_200, got: 60_000 },
                InvalidTransaction::GasFloorMoreThanGasLimit {
                    gas_floor: 66_200,
                    gas_limit: 60_000,
                },
            ),
            (
                HandlerError::InsufficientFunds,
                InvalidTransaction::LackOfFundForMaxFee {
                    fee: Box::new(U256::from(10)),
                    balance: Box::new(U256::ZERO),
                },
            ),
            (
                HandlerError::InvalidNonce { expected: 1, got: 2 },
                InvalidTransaction::NonceTooHigh { tx: 2, state: 1 },
            ),
            (HandlerError::NonceOverflow, InvalidTransaction::NonceOverflowInTransaction),
            (HandlerError::UnsupportedTransactionType(4), InvalidTransaction::Eip7702NotSupported),
            (HandlerError::EmptyBlobs, InvalidTransaction::EmptyBlobs),
            (
                HandlerError::TooManyBlobs { have: 10, max: 6 },
                InvalidTransaction::TooManyBlobs { have: 10, max: 6 },
            ),
        ] {
            let evm2 = FuzzError::from(evm2);
            let revm = FuzzError::from(EVMError::Transaction(revm));
            assert_eq!(evm2, revm);
            let mut counts = HashMap::<_, _>::default();
            counts.insert(evm2, 1);
            assert_eq!(counts.get(&revm), Some(&1));
        }
    }

    #[test]
    fn unmatched_errors_keep_payloads() {
        assert_ne!(
            FuzzError::from(HandlerError::WrongTransactionType { expected: 1 }),
            FuzzError::from(HandlerError::WrongTransactionType { expected: 2 }),
        );
        assert_ne!(
            FuzzError::from(EVMError::Transaction(InvalidTransaction::TooManyBlobs {
                have: 7,
                max: 6
            })),
            FuzzError::from(EVMError::Transaction(InvalidTransaction::TooManyBlobs {
                have: 8,
                max: 6
            })),
        );
        assert_ne!(
            FuzzError::from(EVMError::Custom("first".into())),
            FuzzError::from(EVMError::Custom("second".into())),
        );
    }
}
