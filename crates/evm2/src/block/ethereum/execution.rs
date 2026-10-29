//! Ethereum block execution steps.

use super::dao_fork;
use crate::{
    Evm, EvmTypes, SpecId, TxResult, TxResultWithState,
    block::{
        BlockExecutionError, BlockStateSink, BlockValidationError, CommitChanges,
        InternalBlockExecutionError, InvalidTxError, map_database_error, map_handler_error,
    },
    evm::{
        AccountChangeRef, AccountInfo, BEACON_ROOTS_ADDRESS, BUILDER_DEPOSIT_REQUEST_ADDRESS,
        BUILDER_EXIT_REQUEST_ADDRESS, CONSOLIDATION_REQUEST_ADDRESS, HISTORY_STORAGE_ADDRESS,
        PendingState, StateChangeSink, SystemTx, WITHDRAWAL_REQUEST_ADDRESS,
    },
    interpreter::InstrStop,
    registry::HandlerError,
};
use alloc::{boxed::Box, string::ToString, vec::Vec};
use alloy_consensus::{
    BlockHeader, Header, TxReceipt, constants::ETH_TO_WEI, transaction::Recovered,
};
use alloy_eips::{
    eip2718::Typed2718,
    eip4895::Withdrawal,
    eip6110::{DEPOSIT_REQUEST_TYPE, MAINNET_DEPOSIT_CONTRACT_ADDRESS},
    eip7002::WITHDRAWAL_REQUEST_TYPE,
    eip7251::CONSOLIDATION_REQUEST_TYPE,
    eip7685::Requests,
};
use alloy_primitives::{Address, B256, Bytes, Log, U256, map::AddressMap};
use alloy_sol_types::{SolEvent, sol};
use core::any::Any;

const DEPOSIT_BYTES_SIZE: usize = 48 + 32 + 8 + 96 + 8;
const BUILDER_DEPOSIT_REQUEST_TYPE: u8 = 0x03;
const BUILDER_EXIT_REQUEST_TYPE: u8 = 0x04;

sol! {
    #[allow(missing_docs)]
    event DepositEvent(
        bytes pubkey,
        bytes withdrawal_credentials,
        bytes amount,
        bytes signature,
        bytes index
    );
}

/// Ethereum transaction validation error returned by evm2 handlers.
#[derive(Debug, Clone, PartialEq, Eq)]
struct EthInvalidTxError(HandlerError);

impl core::fmt::Display for EthInvalidTxError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.0.fmt(f)
    }
}

impl core::error::Error for EthInvalidTxError {}

impl InvalidTxError for EthInvalidTxError {
    fn is_nonce_too_low(&self) -> bool {
        matches!(self.0, HandlerError::InvalidNonce { expected, got } if got < expected)
    }

    fn is_gas_limit_too_high(&self) -> bool {
        matches!(
            self.0,
            HandlerError::GasLimitMoreThanBlock { .. }
                | HandlerError::TxGasLimitGreaterThanCap { .. }
        )
    }

    fn is_gas_limit_too_low(&self) -> bool {
        matches!(self.0, HandlerError::IntrinsicGasTooLow { .. })
    }

    fn as_any(&self) -> &(dyn Any + 'static) {
        &self.0
    }
}

impl TryFrom<HandlerError> for EthInvalidTxError {
    type Error = HandlerError;

    fn try_from(error: HandlerError) -> Result<Self, Self::Error> {
        match error {
            HandlerError::Database(_)
            | HandlerError::Fatal(_)
            | HandlerError::WrongTransactionType { .. } => Err(error),
            error => Ok(Self(error)),
        }
    }
}

/// Inputs of Ethereum's pre-block system calls.
#[derive(Clone, Copy, Debug)]
pub(super) struct BlockSystemCalls {
    /// Parent block hash, for EIP-2935 history storage.
    pub(super) parent_hash: B256,
    /// Parent beacon block root, for EIP-4788 beacon roots.
    pub(super) parent_beacon_block_root: Option<B256>,
}

/// Converts a transaction's handler error into a block error.
pub(super) fn map_transaction_error(error: HandlerError, hash: B256) -> BlockExecutionError {
    let error = match EthInvalidTxError::try_from(error) {
        Ok(error) => {
            return BlockValidationError::InvalidTx { hash, error: Box::new(error) }.into();
        }
        Err(error) => error,
    };
    if let HandlerError::Database(database_error) = &error
        && !database_error.is_fatal()
    {
        BlockValidationError::EVM { hash, error: Box::new(error) }.into()
    } else {
        InternalBlockExecutionError::EVM { hash, error: Box::new(error) }.into()
    }
}

/// Executes a transaction and commits it when `commit` returns [`CommitChanges::Yes`].
pub(super) fn execute_transaction_with_condition<T: EvmTypes, S: BlockStateSink>(
    evm: &mut Evm<'_, T>,
    sink: &mut S,
    tx: &Recovered<T::Tx>,
    commit: impl FnOnce(&TxResult<T>) -> CommitChanges,
) -> Result<Option<TxResult<T>>, HandlerError>
where
    T::Tx: Typed2718,
{
    let executed = evm.transact(tx)?;
    if !commit(executed.result()).should_commit() {
        let _ = executed.discard();
        return Ok(None);
    }
    for changes in executed.account_changes() {
        sink.commit_account_changes(changes);
    }
    sink.end_commit();
    Ok(Some(executed.commit()))
}

/// Commits a detached transaction into the EVM and the block state.
pub(super) fn commit_detached_transaction<T: EvmTypes, S: BlockStateSink>(
    evm: &mut Evm<'_, T>,
    sink: &mut S,
    output: TxResultWithState<T>,
) -> TxResult<T> {
    let TxResultWithState { result, pending_state, .. } = output;
    accumulate_pending_state(sink, &pending_state);
    // Reattach the pending state so evm2 reuses its allocations for the next transaction.
    evm.state_mut().set_pending_state(pending_state);
    evm.state_mut().commit_transaction();
    result
}

/// Commits detached state into the EVM and the block state, without a receipt.
pub(super) fn commit_pending_state<T: EvmTypes, S: BlockStateSink>(
    evm: &mut Evm<'_, T>,
    sink: &mut S,
    pending_state: &PendingState,
) {
    accumulate_pending_state(sink, pending_state);
    evm.overlay_db_mut().commit_pending(pending_state);
}

fn accumulate_pending_state<S: BlockStateSink>(sink: &mut S, pending_state: &PendingState) {
    for changes in pending_state.account_changes() {
        sink.commit_account_changes(changes);
    }
    sink.end_commit();
}

/// Runs the EIP-2935 and EIP-4788 system calls.
pub(super) fn pre_execution_system_calls<T: EvmTypes, S: BlockStateSink>(
    evm: &mut Evm<'_, T>,
    sink: &mut S,
    spec_id: SpecId,
    block_number: u64,
    system_calls: BlockSystemCalls,
) -> Result<(), BlockExecutionError> {
    if spec_id.enables(SpecId::PRAGUE) && block_number != 0 {
        let _ = execute_system_call(
            evm,
            sink,
            HISTORY_STORAGE_ADDRESS,
            system_calls.parent_hash.0.into(),
        )?;
    }

    if spec_id.enables(SpecId::CANCUN) {
        let parent_beacon_block_root = system_calls
            .parent_beacon_block_root
            .ok_or(BlockValidationError::MissingParentBeaconBlockRoot)?;

        if block_number == 0 {
            if parent_beacon_block_root != B256::ZERO {
                return Err(BlockValidationError::CancunGenesisParentBeaconBlockRootNotZero {
                    parent_beacon_block_root,
                }
                .into());
            }
        } else {
            let _ = execute_system_call(
                evm,
                sink,
                BEACON_ROOTS_ADDRESS,
                parent_beacon_block_root.0.into(),
            )?;
        }
    }

    Ok(())
}

/// Collects the block's EIP-6110 deposit requests from its receipts.
pub(super) fn block_requests_from_receipts<R>(
    spec_id: SpecId,
    deposit_contract_address: Option<Address>,
    receipts: &[R],
) -> Result<Requests, BlockExecutionError>
where
    R: TxReceipt<Log = Log>,
{
    let mut requests = Requests::default();
    if !spec_id.enables(SpecId::PRAGUE) {
        return Ok(requests);
    }

    let deposit_requests = parse_deposit_requests_from_receipts(
        deposit_contract_address.unwrap_or(MAINNET_DEPOSIT_CONTRACT_ADDRESS),
        receipts,
    )?;
    requests.push_request_with_type(DEPOSIT_REQUEST_TYPE, deposit_requests);

    Ok(requests)
}

fn parse_deposit_requests_from_receipts<R>(
    deposit_contract_address: Address,
    receipts: &[R],
) -> Result<Vec<u8>, BlockExecutionError>
where
    R: TxReceipt<Log = Log>,
{
    let mut out = Vec::new();
    for log in receipts.iter().flat_map(TxReceipt::logs) {
        if log.address != deposit_contract_address
            || log.topics().first() != Some(&DepositEvent::SIGNATURE_HASH)
        {
            continue;
        }

        let decoded = DepositEvent::decode_log(log)
            .map_err(|err| BlockValidationError::DepositRequestDecode(err.to_string()))?;
        out.reserve(DEPOSIT_BYTES_SIZE);
        out.extend_from_slice(decoded.pubkey.as_ref());
        out.extend_from_slice(decoded.withdrawal_credentials.as_ref());
        out.extend_from_slice(decoded.amount.as_ref());
        out.extend_from_slice(decoded.signature.as_ref());
        out.extend_from_slice(decoded.index.as_ref());
    }

    Ok(out)
}

/// Runs the EIP-7002, EIP-7251 and EIP-8282 request system calls.
pub(super) fn post_execution_system_calls<T: EvmTypes, S: BlockStateSink>(
    evm: &mut Evm<'_, T>,
    sink: &mut S,
    spec_id: SpecId,
    requests: &mut Requests,
) -> Result<(), BlockExecutionError> {
    if !spec_id.enables(SpecId::PRAGUE) {
        return Ok(());
    }

    let mut request_types = Vec::with_capacity(4);
    request_types.push((WITHDRAWAL_REQUEST_ADDRESS, WITHDRAWAL_REQUEST_TYPE));
    request_types.push((CONSOLIDATION_REQUEST_ADDRESS, CONSOLIDATION_REQUEST_TYPE));
    if spec_id.enables(SpecId::AMSTERDAM) {
        request_types.push((BUILDER_DEPOSIT_REQUEST_ADDRESS, BUILDER_DEPOSIT_REQUEST_TYPE));
        request_types.push((BUILDER_EXIT_REQUEST_ADDRESS, BUILDER_EXIT_REQUEST_TYPE));
    }
    for (address, request_type) in request_types {
        let result = execute_system_call(evm, sink, address, Bytes::new())?;
        requests.push_request_with_type(request_type, result.output.iter().copied());
    }

    Ok(())
}

fn execute_system_call<T: EvmTypes, S: BlockStateSink>(
    evm: &mut Evm<'_, T>,
    sink: &mut S,
    address: Address,
    data: Bytes,
) -> Result<TxResult<T>, BlockExecutionError> {
    let executed = evm.system_call(SystemTx::new(address, data)).map_err(map_handler_error)?;
    // History and beacon-root calls accept reverts and halts, unlike request-producing calls.
    if !executed.result().status
        && !matches!(address, BEACON_ROOTS_ADDRESS | HISTORY_STORAGE_ADDRESS)
    {
        let reason = executed.result().stop;
        let _ = executed.discard();
        return Err(match address {
            WITHDRAWAL_REQUEST_ADDRESS => BlockValidationError::WithdrawalRequestsContractCall {
                message: alloc::format!("{reason:?}"),
            },
            CONSOLIDATION_REQUEST_ADDRESS => {
                BlockValidationError::ConsolidationRequestsContractCall {
                    message: alloc::format!("{reason:?}"),
                }
            }
            _ => BlockValidationError::other(SystemCallFailed { address, reason }),
        }
        .into());
    }
    let outcome = executed.detach();
    Ok(commit_detached_transaction(evm, sink, outcome))
}

fn commit_state_changes<T: EvmTypes, S: BlockStateSink>(
    evm: &mut Evm<'_, T>,
    sink: &mut S,
    changes: &[(Address, Option<AccountInfo>, Option<AccountInfo>)],
) {
    let mut pending_state = PendingState::default();
    for (address, original, current) in changes {
        let change = AccountChangeRef {
            address: *address,
            original: original.as_ref(),
            current: current.as_ref(),
            created: false,
            selfdestructed: false,
        };
        let Ok(()) = evm.overlay_db_mut().account(change);
        pending_state.insert_account(*address, original.clone(), current.clone());
    }
    accumulate_pending_state(sink, &pending_state);
}

/// Post-block balance changes: block and ommer rewards, the DAO fork drain, and withdrawals.
#[derive(Clone, Copy, Debug)]
pub(super) struct BalanceChanges<'a> {
    pub(super) base_block_reward: Option<u128>,
    pub(super) dao_fork_transition: bool,
    pub(super) block_number: u64,
    pub(super) beneficiary: Address,
    pub(super) ommers: &'a [Header],
    pub(super) withdrawals: Option<&'a [Withdrawal]>,
}

/// Applies the post-block balance changes.
pub(super) fn post_block_balance_changes<T: EvmTypes, S: BlockStateSink>(
    evm: &mut Evm<'_, T>,
    sink: &mut S,
    block: BalanceChanges<'_>,
) -> Result<(), BlockExecutionError> {
    let mut balance_increments = AddressMap::<U256>::default();

    if let Some(base_block_reward) = block.base_block_reward {
        for ommer in block.ommers {
            *balance_increments.entry(ommer.beneficiary()).or_default() +=
                U256::from(ommer_reward(base_block_reward, block.block_number, ommer.number()));
        }
        *balance_increments.entry(block.beneficiary).or_default() +=
            U256::from(block_reward(base_block_reward, block.ommers.len()));
    }

    for withdrawal in block.withdrawals.into_iter().flatten() {
        *balance_increments.entry(withdrawal.address).or_default() += withdrawal.amount_wei();
    }

    let mut changes = Vec::new();

    if block.dao_fork_transition {
        let mut drained_balance = U256::ZERO;
        // Absent accounts stay absent, and an account left empty by the drain is removed, even
        // before EIP-161.
        for address in dao_fork::DAO_HARDFORK_ACCOUNTS {
            let original = evm.read_account_info(&address).map_err(map_database_error)?;
            let Some(original) = original else { continue };

            drained_balance = drained_balance.saturating_add(original.balance);
            let mut current = original.clone();
            current.balance = U256::ZERO;
            let current = (!current.is_empty()).then_some(current);
            if current.as_ref() == Some(&original) {
                continue;
            }
            changes.push((address, Some(original), current));
        }

        *balance_increments.entry(dao_fork::DAO_HARDFORK_BENEFICIARY).or_default() +=
            drained_balance;
        // Reward recipients may also be drained accounts: read their balances after the drain.
        commit_state_changes(evm, sink, &changes);
        changes.clear();
    }

    if balance_increments.is_empty() {
        return Ok(());
    }

    for (address, increment) in balance_increments {
        let original = evm.read_account_info(&address).map_err(map_database_error)?;
        let current = if increment.is_zero() && original.as_ref().is_none_or(AccountInfo::is_empty)
        {
            None
        } else {
            let mut current = original.clone().unwrap_or_else(AccountInfo::empty);
            current.balance = current.balance.saturating_add(increment);
            Some(current)
        };
        if original == current {
            // Zero-value withdrawals still count as account accesses in the BAL.
            let Ok(()) = evm.overlay_db_mut().account_read(address, original.as_ref());
            continue;
        }
        changes.push((address, original, current));
    }

    commit_state_changes(evm, sink, &changes);

    Ok(())
}

const fn block_reward(base_block_reward: u128, ommers: usize) -> u128 {
    base_block_reward + (base_block_reward >> 5) * ommers as u128
}

fn ommer_reward(base_block_reward: u128, block_number: u64, ommer_block_number: u64) -> u128 {
    let distance = 8u64.saturating_add(ommer_block_number).saturating_sub(block_number);
    (u128::from(distance) * base_block_reward) >> 3
}

/// Returns the base block reward at `block_number`, or `None` after the merge.
pub(super) fn base_block_reward<C>(chain: &C, block_number: u64) -> Option<u128>
where
    C: alloy_hardforks::EthereumHardforks + ?Sized,
{
    if chain.is_paris_active_at_block(block_number) {
        None
    } else if chain.is_constantinople_active_at_block(block_number) {
        Some(ETH_TO_WEI * 2)
    } else if chain.is_byzantium_active_at_block(block_number) {
        Some(ETH_TO_WEI * 3)
    } else {
        Some(ETH_TO_WEI * 5)
    }
}

/// A system contract reverted or halted.
#[derive(Debug)]
struct SystemCallFailed {
    address: Address,
    reason: InstrStop,
}

impl core::fmt::Display for SystemCallFailed {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "EVM system call to {} failed: {:?}", self.address, self.reason)
    }
}

impl core::error::Error for SystemCallFailed {}
