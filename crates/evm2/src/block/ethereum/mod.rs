//! Ethereum block execution.

use crate::{
    BaseEvmTypes, BlockStateAccumulator, Evm, EvmFeatures, EvmTypes, TxResult, TxResultWithState,
    block::{
        BlockExecutionError, BlockExecutionOutput, BlockExecutionResult, BlockExecutor,
        BlockStateSink, BlockTransactionResult, BlockValidationError, CommitChanges, ExecutableTx,
        GasOutput, ReceiptBuilder, ReceiptBuilderCtx,
    },
    evm::{Bal, PendingState},
    interpreter::Host,
};
use alloc::{borrow::Cow, sync::Arc, vec::Vec};
use alloy_consensus::{
    Eip658Value, EthereumTxEnvelope, Header, Receipt, ReceiptEnvelope, Transaction, TxEip4844,
    TxType,
    transaction::{TransactionEnvelope, TxHashRef},
};
use alloy_eip7928::BlockAccessIndex;
use alloy_eips::{eip2718::Typed2718, eip4895::Withdrawal};
use alloy_hardforks::{EthereumHardfork, EthereumHardforks};
use alloy_primitives::{Address, B256};
use core::borrow::Borrow;

mod dao_fork;

mod execution;
use execution::{BalanceChanges, BlockSystemCalls};

#[cfg(test)]
mod tests;

/// Block inputs of [`EthBlockExecutor`] that are not in the EVM's block environment.
#[derive(Clone, Debug, Default)]
pub struct EthBlockExecutionCtx<'a> {
    /// Expected number of transactions, to size the receipts.
    pub tx_count_hint: Option<usize>,
    /// Parent block hash.
    pub parent_hash: B256,
    /// Parent beacon block root.
    pub parent_beacon_block_root: Option<B256>,
    /// Ommer headers.
    pub ommers: &'a [Header],
    /// Withdrawals.
    pub withdrawals: Option<Cow<'a, [Withdrawal]>>,
}

/// Chain rules of [`EthBlockExecutor`] that depend on the block number.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EthBlockRules {
    /// Block reward before the merge.
    pub base_block_reward: Option<u128>,
    /// Whether the block is the DAO fork block.
    pub dao_fork_transition: bool,
    /// EIP-6110 deposit contract, Ethereum mainnet's when `None`.
    pub deposit_contract_address: Option<Address>,
}

impl EthBlockRules {
    /// Reads the rules of block `block_number` from a chain's hardforks.
    pub fn new<C: EthereumHardforks + ?Sized>(
        chain: &C,
        block_number: u64,
        deposit_contract_address: Option<Address>,
    ) -> Self {
        Self {
            base_block_reward: execution::base_block_reward(chain, block_number),
            dao_fork_transition: chain
                .ethereum_fork_activation(EthereumHardfork::Dao)
                .transitions_at_block(block_number),
            deposit_contract_address,
        }
    }
}

/// Builds [`ReceiptEnvelope`]s for Ethereum transactions without blob sidecars.
#[derive(Clone, Copy, Debug, Default)]
#[non_exhaustive]
pub struct EthReceiptBuilder;

impl ReceiptBuilder for EthReceiptBuilder {
    type Transaction = EthereumTxEnvelope<TxEip4844>;
    type Receipt = ReceiptEnvelope;

    fn build_receipt<T: EvmTypes>(
        &self,
        ctx: ReceiptBuilderCtx<TxType, TxResult<T>>,
    ) -> ReceiptEnvelope {
        let ReceiptBuilderCtx { tx_type, result, cumulative_gas_used } = ctx;
        let receipt = Receipt {
            status: Eip658Value::Eip658(result.status),
            cumulative_gas_used,
            logs: result.logs,
        };
        ReceiptEnvelope::from_typed(tx_type, receipt.with_bloom())
    }
}

/// A detached transaction result with the metadata to commit its receipt.
#[derive(Debug)]
pub struct EthTransactionResultWithState<T: EvmTypes = BaseEvmTypes, Ty = TxType> {
    result: TxResultWithState<T>,
    tx_type: Ty,
    blob_gas_used: u64,
}

impl<T: EvmTypes, Ty> EthTransactionResultWithState<T, Ty> {
    /// Creates a detached transaction result.
    pub const fn new(result: TxResultWithState<T>, tx_type: Ty, blob_gas_used: u64) -> Self {
        Self { result, tx_type, blob_gas_used }
    }
}

impl<T: EvmTypes, Ty> BlockTransactionResult<T> for EthTransactionResultWithState<T, Ty> {
    fn result(&self) -> &TxResultWithState<T> {
        &self.result
    }
}

/// Executes Ethereum blocks.
///
/// Block state goes to `S`, a [`BlockStateSink`].
#[expect(missing_debug_implementations)]
pub struct EthBlockExecutor<'a, T = BaseEvmTypes, R = EthReceiptBuilder, S = BlockStateAccumulator>
where
    T: EvmTypes,
    R: ReceiptBuilder,
{
    /// EVM with the block's state overlay.
    pub evm: Evm<'a, T>,
    ctx: EthBlockExecutionCtx<'a>,
    rules: EthBlockRules,
    receipt_builder: R,
    spec_id: crate::SpecId,
    /// Block state collected so far.
    pub state: S,
    /// Receipts in execution order.
    pub receipts: Vec<R::Receipt>,
    /// Receipt gas used by the block so far.
    pub cumulative_gas_used: u64,
    block_regular_gas_used: u64,
    block_state_gas_used: u64,
    separate_block_gas: bool,
    /// Blob gas used by the block so far.
    pub blob_gas_used: u64,
}

impl<'a, T, R, S> EthBlockExecutor<'a, T, R, S>
where
    T: EvmTypes<Tx: Typed2718>,
    R: ReceiptBuilder<Transaction: Transaction + TxHashRef>,
    S: BlockStateSink,
{
    /// Creates an executor for the block in `evm`'s block environment.
    pub fn new(
        evm: Evm<'a, T>,
        ctx: EthBlockExecutionCtx<'a>,
        rules: EthBlockRules,
        receipt_builder: R,
        state: S,
    ) -> Self {
        // Custom type families may expose a richer spec ID that converts into evm2's.
        #[allow(clippy::useless_conversion)]
        let spec_id = evm.spec_id().into();
        let separate_block_gas = evm.version().feature(EvmFeatures::EIP8037);
        let receipts = Vec::with_capacity(ctx.tx_count_hint.unwrap_or_default());
        Self {
            evm,
            ctx,
            rules,
            receipt_builder,
            spec_id,
            state,
            receipts,
            cumulative_gas_used: 0,
            block_regular_gas_used: 0,
            block_state_gas_used: 0,
            separate_block_gas,
            blob_gas_used: 0,
        }
    }

    /// Returns the block execution context.
    pub const fn context(&self) -> &EthBlockExecutionCtx<'a> {
        &self.ctx
    }

    /// Returns the chain rules of the block.
    pub const fn rules(&self) -> &EthBlockRules {
        &self.rules
    }

    /// Sets the Ethereum spec used for system calls and requests.
    pub const fn set_spec_id(&mut self, spec_id: crate::SpecId) {
        self.spec_id = spec_id;
    }

    /// Commits detached state without a receipt or gas.
    pub fn commit_pending_state(&mut self, state: &PendingState) {
        execution::commit_pending_state(&mut self.evm, &mut self.state, state);
    }

    fn block_number(&mut self) -> u64 {
        self.evm.block_env().number.to::<u64>()
    }

    const fn set_transaction_block_access_index(&mut self) {
        if self.evm.state().bal_builder().is_some() {
            let index = self.receipts.len() as u64 + 1;
            self.evm.state_mut().set_bal_index(BlockAccessIndex::new(index));
        }
    }

    fn record_receipt(
        &mut self,
        tx_type: <R::Transaction as TransactionEnvelope>::TxType,
        blob_gas_used: u64,
        result: TxResult<T>,
    ) -> GasOutput {
        let tx_gas_used = result.tx_gas_used();
        let regular_gas_used = result.execution_gas_spent();
        let state_gas_used = result.state_gas_spent();
        self.block_regular_gas_used = self.block_regular_gas_used.saturating_add(regular_gas_used);
        self.block_state_gas_used = self.block_state_gas_used.saturating_add(state_gas_used);
        self.cumulative_gas_used += tx_gas_used;
        self.blob_gas_used += blob_gas_used;
        self.receipts.push(self.receipt_builder.build_receipt::<T>(ReceiptBuilderCtx {
            tx_type,
            result,
            cumulative_gas_used: self.cumulative_gas_used,
        }));
        GasOutput::new_with_regular(tx_gas_used, regular_gas_used, state_gas_used)
    }

    const fn block_gas_used(&self) -> u64 {
        if !self.separate_block_gas {
            self.cumulative_gas_used
        } else if self.block_regular_gas_used > self.block_state_gas_used {
            self.block_regular_gas_used
        } else {
            self.block_state_gas_used
        }
    }
}

impl<'a, T, R, S> BlockExecutor for EthBlockExecutor<'a, T, R, S>
where
    T: EvmTypes<Tx: Typed2718>,
    R: ReceiptBuilder<Transaction: Transaction + TxHashRef>,
    S: BlockStateSink,
{
    type Transaction = R::Transaction;
    type Receipt = R::Receipt;
    type EvmTypes = T;
    type Evm = Evm<'a, T>;
    type State = S;
    type TransactionResultWithState =
        EthTransactionResultWithState<T, <R::Transaction as TransactionEnvelope>::TxType>;

    fn evm(&self) -> &Self::Evm {
        &self.evm
    }

    fn evm_mut(&mut self) -> &mut Self::Evm {
        &mut self.evm
    }

    fn set_block_access_list(&mut self, block_access_list: Arc<Bal>) {
        self.evm.state_mut().set_bal(block_access_list);
    }

    fn set_block_access_index(&mut self, index: BlockAccessIndex) {
        self.evm.state_mut().set_bal_index(index);
    }

    fn enable_block_access_list_builder(&mut self) {
        self.evm.state_mut().enable_bal_builder();
        self.evm.state_mut().reset_bal_index();
    }

    fn take_block_access_list(&mut self) -> Option<Bal> {
        self.evm.state_mut().take_bal_builder()
    }

    fn apply_pre_execution_changes(&mut self) -> Result<(), BlockExecutionError> {
        let block_number = self.block_number();
        let system_calls = BlockSystemCalls {
            parent_hash: self.ctx.parent_hash,
            parent_beacon_block_root: self.ctx.parent_beacon_block_root,
        };
        execution::pre_execution_system_calls(
            &mut self.evm,
            &mut self.state,
            self.spec_id,
            block_number,
            system_calls,
        )
    }

    fn validate_transaction_gas_limit(
        &mut self,
        gas_limit: u64,
    ) -> Result<(), BlockExecutionError> {
        let block_gas_limit = self.evm.block_env().gas_limit.to::<u64>();
        let regular_limit = gas_limit.min(self.evm.version().tx_gas_limit_cap);
        let available = if self.separate_block_gas {
            let regular_available = block_gas_limit.saturating_sub(self.block_regular_gas_used);
            let state_available = block_gas_limit.saturating_sub(self.block_state_gas_used);
            if regular_limit > regular_available {
                Some(regular_available)
            } else {
                (gas_limit > state_available).then_some(state_available)
            }
        } else {
            let available = block_gas_limit.saturating_sub(self.cumulative_gas_used);
            (regular_limit > available).then_some(available)
        };
        if let Some(block_available_gas) = available {
            return Err(BlockValidationError::TransactionGasLimitMoreThanAvailableBlockGas {
                transaction_gas_limit: gas_limit,
                block_available_gas,
            }
            .into());
        }
        Ok(())
    }

    fn execute_transaction_with_commit_condition(
        &mut self,
        tx: impl ExecutableTx<Self>,
        commit: impl FnOnce(&TxResult<T>) -> CommitChanges,
    ) -> Result<Option<GasOutput>, BlockExecutionError> {
        let (tx_env, tx) = tx.into_parts();
        let tx = tx.borrow();
        self.set_transaction_block_access_index();
        self.validate_transaction_gas_limit(tx.gas_limit())?;
        let Some(result) = execution::execute_transaction_with_condition(
            &mut self.evm,
            &mut self.state,
            &tx_env,
            commit,
        )
        .map_err(|error| execution::map_transaction_error(error, *tx.tx_hash()))?
        else {
            return Ok(None);
        };
        let blob_gas_used = tx.blob_gas_used().unwrap_or_default();
        Ok(Some(self.record_receipt(tx.tx_type(), blob_gas_used, result)))
    }

    fn execute_transaction_without_commit(
        &mut self,
        tx: impl ExecutableTx<Self>,
    ) -> Result<Self::TransactionResultWithState, BlockExecutionError> {
        let (tx_env, tx) = tx.into_parts();
        let tx = tx.borrow();
        self.set_transaction_block_access_index();
        self.validate_transaction_gas_limit(tx.gas_limit())?;
        let result = self
            .evm
            .transact(&tx_env)
            .map(|executed| executed.detach())
            .map_err(|error| execution::map_transaction_error(error, *tx.tx_hash()))?;
        Ok(EthTransactionResultWithState::new(
            result,
            tx.tx_type(),
            tx.blob_gas_used().unwrap_or_default(),
        ))
    }

    fn commit_transaction(
        &mut self,
        output: Self::TransactionResultWithState,
    ) -> Result<GasOutput, BlockExecutionError> {
        let EthTransactionResultWithState { result, tx_type, blob_gas_used } = output;
        self.set_transaction_block_access_index();
        let result = execution::commit_detached_transaction(&mut self.evm, &mut self.state, result);
        Ok(self.record_receipt(tx_type, blob_gas_used, result))
    }

    fn receipts(&self) -> &[Self::Receipt] {
        &self.receipts
    }

    fn into_state(self) -> S {
        self.state
    }

    fn finish(mut self) -> Result<BlockExecutionOutput<R::Receipt, S>, BlockExecutionError> {
        self.set_transaction_block_access_index();
        let mut requests = execution::block_requests_from_receipts(
            self.spec_id,
            self.rules.deposit_contract_address,
            &self.receipts,
        )?;
        execution::post_execution_system_calls(
            &mut self.evm,
            &mut self.state,
            self.spec_id,
            &mut requests,
        )?;

        let block_number = self.block_number();
        let beneficiary = self.evm.block_env().beneficiary;
        let withdrawals = self
            .ctx
            .withdrawals
            .as_deref()
            .filter(|_| self.spec_id.enables(crate::SpecId::SHANGHAI));
        execution::post_block_balance_changes(
            &mut self.evm,
            &mut self.state,
            BalanceChanges {
                base_block_reward: self.rules.base_block_reward,
                dao_fork_transition: self.rules.dao_fork_transition,
                block_number,
                beneficiary,
                ommers: self.ctx.ommers,
                withdrawals,
            },
        )?;

        let block_access_list = self.evm.state_mut().take_bal_builder();
        let gas_used = self.block_gas_used();
        Ok(BlockExecutionOutput {
            result: BlockExecutionResult {
                receipts: self.receipts,
                requests,
                gas_used,
                blob_gas_used: self.blob_gas_used,
            },
            state: self.state,
            block_access_list,
        })
    }
}
