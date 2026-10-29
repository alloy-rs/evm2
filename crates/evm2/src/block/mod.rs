//! Block execution.
//!
//! [`BlockExecutor`] executes the transactions of one block on an [`Evm`](crate::Evm), applies
//! the block's pre- and post-execution changes, and collects receipts and block state.
//! [`ethereum::EthBlockExecutor`] implements it with Ethereum's block rules.
//!
//! Block state goes to a [`BlockStateSink`] chosen by the caller, so nodes can accumulate state
//! in their own representation. [`BlockStateAccumulator`] is the default.

use crate::{
    BlockStateAccumulator, EvmTypes, EvmTypesHost, TxResult, TxResultWithState,
    evm::{AccountChanges, Bal, NoopChangeSink},
};
use alloc::{sync::Arc, vec::Vec};
use alloy_consensus::transaction::{Recovered, TransactionEnvelope};
use alloy_eip7928::BlockAccessIndex;
use alloy_eips::eip7685::Requests;
use alloy_primitives::Log;
use core::borrow::Borrow;

mod error;
pub use error::{
    BlockExecutionError, BlockValidationError, InternalBlockExecutionError, InvalidTxError,
    map_database_error, map_handler_error,
};

pub mod ethereum;

/// The transaction environment executed by a [`BlockExecutor`].
pub type EvmTxOf<E> = <<E as BlockExecutor>::EvmTypes as EvmTypesHost>::Tx;

/// Gas used by a committed transaction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GasOutput {
    tx_gas_used: u64,
    regular_gas_used: u64,
    state_gas_used: u64,
}

impl GasOutput {
    /// Creates a gas output where regular gas equals receipt gas.
    pub const fn new(tx_gas_used: u64, state_gas_used: u64) -> Self {
        Self { tx_gas_used, regular_gas_used: tx_gas_used, state_gas_used }
    }

    /// Creates a gas output with independent receipt, regular, and state gas.
    pub const fn new_with_regular(
        tx_gas_used: u64,
        regular_gas_used: u64,
        state_gas_used: u64,
    ) -> Self {
        Self { tx_gas_used, regular_gas_used, state_gas_used }
    }

    /// Returns the gas used for receipt accounting.
    pub const fn tx_gas_used(&self) -> u64 {
        self.tx_gas_used
    }

    /// Returns the regular gas used for Amsterdam block-capacity accounting.
    pub const fn regular_gas_used(&self) -> u64 {
        self.regular_gas_used
    }

    /// Returns the state gas used for state-capacity accounting.
    pub const fn state_gas_used(&self) -> u64 {
        self.state_gas_used
    }
}

/// Whether a block executor commits an executed transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommitChanges {
    /// Commit the transaction into the block.
    Yes,
    /// Discard the transaction.
    No,
}

impl CommitChanges {
    /// Returns whether the transaction is committed.
    pub const fn should_commit(self) -> bool {
        matches!(self, Self::Yes)
    }
}

/// Context for building a receipt from a transaction result.
#[derive(Clone, Debug)]
pub struct ReceiptBuilderCtx<TxType, Result> {
    /// Transaction type of the receipt.
    pub tx_type: TxType,
    /// Transaction execution result.
    pub result: Result,
    /// Gas used by this and all earlier transactions in the block.
    pub cumulative_gas_used: u64,
}

/// Builds chain-specific receipts from transaction results.
#[auto_impl::auto_impl(&, Arc)]
pub trait ReceiptBuilder {
    /// Consensus transaction type accepted by the executor.
    type Transaction: TransactionEnvelope;
    /// Receipt produced by this builder.
    type Receipt: alloy_consensus::TxReceipt<Log = Log>;

    /// Builds the receipt of one transaction.
    fn build_receipt<T: EvmTypes>(
        &self,
        ctx: ReceiptBuilderCtx<<Self::Transaction as TransactionEnvelope>::TxType, TxResult<T>>,
    ) -> Self::Receipt;
}

/// Receives the state changes a block executor commits.
///
/// Every committed unit (a transaction, a system call, or a post-block balance change) reports
/// its changed accounts through [`Self::commit_account_changes`], then calls
/// [`Self::end_commit`].
pub trait BlockStateSink {
    /// Records one account's committed changes.
    fn commit_account_changes(&mut self, changes: AccountChanges<'_>);

    /// Marks the end of one committed unit.
    fn end_commit(&mut self) {}
}

impl BlockStateSink for BlockStateAccumulator {
    fn commit_account_changes(&mut self, changes: AccountChanges<'_>) {
        if changes.is_changed() {
            let Ok(()) = changes.visit(self);
        }
    }
}

impl BlockStateSink for NoopChangeSink {
    fn commit_account_changes(&mut self, _changes: AccountChanges<'_>) {}
}

impl<S: BlockStateSink + ?Sized> BlockStateSink for &mut S {
    fn commit_account_changes(&mut self, changes: AccountChanges<'_>) {
        (**self).commit_account_changes(changes);
    }

    fn end_commit(&mut self) {
        (**self).end_commit();
    }
}

/// A transaction a [`BlockExecutor`] can execute: the EVM transaction environment with its
/// recovered signer, and the consensus transaction it was built from.
pub trait ExecutableTx<E: BlockExecutor> {
    /// Consensus transaction accessor.
    type Tx: Borrow<E::Transaction>;

    /// Splits the transaction into its EVM environment and consensus transaction.
    fn into_parts(self) -> (Recovered<EvmTxOf<E>>, Self::Tx);
}

impl<E, Tx> ExecutableTx<E> for (Recovered<EvmTxOf<E>>, Tx)
where
    E: BlockExecutor,
    Tx: Borrow<E::Transaction>,
{
    type Tx = Tx;

    fn into_parts(self) -> (Recovered<EvmTxOf<E>>, Tx) {
        self
    }
}

impl<'a, E> ExecutableTx<E> for &'a Recovered<E::Transaction>
where
    E: BlockExecutor<Transaction: Clone>,
    EvmTxOf<E>: From<E::Transaction>,
{
    type Tx = &'a E::Transaction;

    fn into_parts(self) -> (Recovered<EvmTxOf<E>>, &'a E::Transaction) {
        let tx = Recovered::new_unchecked(self.inner().clone().into(), self.signer());
        (tx, self.inner())
    }
}

/// A transaction result detached from the executor's state, with the metadata to commit it.
pub trait BlockTransactionResult<T: EvmTypes> {
    /// Returns the transaction result and its detached state.
    fn result(&self) -> &TxResultWithState<T>;
}

/// Receipts, requests, and gas of an executed block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockExecutionResult<R> {
    /// Receipts of the block's transactions.
    pub receipts: Vec<R>,
    /// EIP-7685 requests of the block.
    pub requests: Requests,
    /// Gas used by the block.
    pub gas_used: u64,
    /// Blob gas used by the block.
    pub blob_gas_used: u64,
}

impl<R> Default for BlockExecutionResult<R> {
    fn default() -> Self {
        Self { receipts: Vec::new(), requests: Requests::default(), gas_used: 0, blob_gas_used: 0 }
    }
}

/// The output of a finished block execution.
#[derive(Debug)]
pub struct BlockExecutionOutput<R, S> {
    /// Receipts, requests, and gas.
    pub result: BlockExecutionResult<R>,
    /// Block state collected by the executor's [`BlockStateSink`].
    pub state: S,
    /// Block access list built during execution, when its builder was enabled.
    pub block_access_list: Option<Bal>,
}

/// Executes the transactions of one block.
pub trait BlockExecutor: Sized {
    /// Consensus transaction type.
    type Transaction;
    /// Receipt type.
    type Receipt;
    /// EVM type family.
    type EvmTypes: EvmTypes;
    /// EVM used by this executor.
    type Evm;
    /// Block state collected while executing.
    type State;
    /// Transaction result detached from the executor's state.
    type TransactionResultWithState: BlockTransactionResult<Self::EvmTypes>;

    /// Returns the EVM.
    fn evm(&self) -> &Self::Evm;

    /// Returns the EVM mutably.
    fn evm_mut(&mut self) -> &mut Self::Evm;

    /// Attaches an EIP-7928 block access list for indexed state reads.
    fn set_block_access_list(&mut self, block_access_list: Arc<Bal>);

    /// Sets the EIP-7928 block access index used by state reads and the BAL builder.
    fn set_block_access_index(&mut self, index: BlockAccessIndex);

    /// Enables building an EIP-7928 block access list from committed changes.
    fn enable_block_access_list_builder(&mut self);

    /// Takes the block access list built so far.
    fn take_block_access_list(&mut self) -> Option<Bal>;

    /// Applies the block's pre-execution changes.
    fn apply_pre_execution_changes(&mut self) -> Result<(), BlockExecutionError>;

    /// Checks a transaction's gas limit against the gas left in the block.
    fn validate_transaction_gas_limit(&mut self, gas_limit: u64)
    -> Result<(), BlockExecutionError>;

    /// Executes a transaction without committing it.
    fn execute_transaction_without_commit(
        &mut self,
        tx: impl ExecutableTx<Self>,
    ) -> Result<Self::TransactionResultWithState, BlockExecutionError>;

    /// Commits a detached transaction and records its receipt and gas.
    fn commit_transaction(
        &mut self,
        output: Self::TransactionResultWithState,
    ) -> Result<GasOutput, BlockExecutionError>;

    /// Executes a transaction and commits it when `commit` returns [`CommitChanges::Yes`].
    fn execute_transaction_with_commit_condition(
        &mut self,
        tx: impl ExecutableTx<Self>,
        commit: impl FnOnce(&TxResult<Self::EvmTypes>) -> CommitChanges,
    ) -> Result<Option<GasOutput>, BlockExecutionError> {
        let output = self.execute_transaction_without_commit(tx)?;
        if commit(&output.result().result).should_commit() {
            self.commit_transaction(output).map(Some)
        } else {
            Ok(None)
        }
    }

    /// Executes and commits a transaction.
    fn execute_transaction(
        &mut self,
        tx: impl ExecutableTx<Self>,
    ) -> Result<GasOutput, BlockExecutionError> {
        self.execute_transaction_with_commit_condition(tx, |_| CommitChanges::Yes)
            .map(Option::unwrap_or_default)
    }

    /// Returns the receipts collected so far.
    fn receipts(&self) -> &[Self::Receipt];

    /// Returns the block state collected so far, without post-execution changes.
    fn into_state(self) -> Self::State;

    /// Applies the block's post-execution changes and returns the block's output.
    fn finish(
        self,
    ) -> Result<BlockExecutionOutput<Self::Receipt, Self::State>, BlockExecutionError>;
}
