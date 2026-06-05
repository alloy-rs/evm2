//! Transaction execution lifecycle and result types.

use super::{BlockStateAccumulator, DbErrorCode, Evm, StateChangeSink, StateChanges};
use crate::{EvmTypes, interpreter::InstrStop};
use alloc::vec::Vec;
use alloy_primitives::{Bytes, Log};
use core::fmt;
use derive_where::derive_where;

/// Transaction execution outcome without an owned state diff.
///
/// This is the result-only half of transaction execution: status, gas used, output, stop reason,
/// logs, database error handle, and extension data. Logs live here because they are execution
/// output, not database state. Use [`ExecutedTx::detach`] only when an owned [`StateChanges`] value
/// is required.
#[derive_where(Clone, Debug, Default, PartialEq, Eq; T::TxResultExt)]
pub struct TxOutcome<T: EvmTypes = crate::BaseEvmTypes> {
    /// Whether execution succeeded.
    pub status: bool,
    /// Gas used by execution.
    pub gas_used: u64,
    /// Interpreter stop reason.
    pub stop: InstrStop,
    /// Return or revert output.
    pub output: Bytes,
    /// Logs emitted by the transaction.
    pub logs: Vec<Log>,
    /// Database error handle, if execution stopped on a database error.
    pub db_error_code: Option<DbErrorCode>,
    /// EVM type-specific extension data.
    pub ext: T::TxResultExt,
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
}

impl<T: EvmTypes> TxOutcome<T> {
    /// Returns the transaction gas-used value.
    #[inline]
    pub const fn gas_used(&self) -> u64 {
        self.gas_used
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingState {
    Present,
    Cleared,
}

/// A transaction whose post-finalization state is not resolved yet.
///
/// `ExecutedTx` borrows the EVM mutably until the caller chooses what to do with the
/// transaction scratch:
///
/// - [`Self::commit`] accepts the state into the internal accepted overlay;
/// - [`Self::discard`] drops the state and keeps only the outcome;
/// - [`Self::detach`] materializes an owned [`StateChanges`] value without committing it;
/// - [`Self::commit_to`] accepts the state and records it in a block accumulator;
/// - [`Self::commit_with`] accepts the state and first streams it to an external sink.
///
/// Dropping `ExecutedTx` without calling one of those methods is equivalent to [`Self::discard`].
#[must_use = "executed transaction state must be committed, discarded, or detached"]
pub struct ExecutedTx<'evm, T: EvmTypes = crate::BaseEvmTypes> {
    evm: &'evm mut Evm<T>,
    outcome: Option<TxOutcome<T>>,
    state: PendingState,
}

impl<T: EvmTypes> fmt::Debug for ExecutedTx<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExecutedTx")
            .field("has_pending_state", &self.has_pending_state())
            .finish_non_exhaustive()
    }
}

impl<'evm, T: EvmTypes> ExecutedTx<'evm, T> {
    #[inline]
    pub(crate) const fn from_outcome(
        evm: &'evm mut Evm<T>,
        outcome: TxOutcome<T>,
        has_pending_state: bool,
    ) -> Self {
        Self {
            evm,
            outcome: Some(outcome),
            state: if has_pending_state { PendingState::Present } else { PendingState::Cleared },
        }
    }

    #[inline]
    fn has_pending_state(&self) -> bool {
        self.state == PendingState::Present
    }

    #[inline]
    fn take_outcome(&mut self) -> TxOutcome<T> {
        match self.outcome.take() {
            Some(outcome) => outcome,
            None => unreachable!("executed transaction outcome was already taken"),
        }
    }

    #[inline]
    fn clear_pending_state(&mut self) {
        if self.has_pending_state() {
            self.evm.state.clear_transaction_state();
            self.state = PendingState::Cleared;
        }
    }

    #[inline]
    fn commit_pending_state(&mut self) {
        if self.has_pending_state() {
            self.evm.state.commit_transaction();
            self.evm.state.clear_transaction_state();
            self.state = PendingState::Cleared;
        }
    }

    #[inline]
    fn take_state_changes(&mut self) -> StateChanges {
        if self.has_pending_state() {
            let changes = self.evm.state.build_state_changes();
            self.evm.state.clear_transaction_state();
            self.state = PendingState::Cleared;
            changes
        } else {
            StateChanges::default()
        }
    }

    /// Returns the transaction outcome without resolving state changes.
    #[inline]
    pub fn outcome(&self) -> &TxOutcome<T> {
        match &self.outcome {
            Some(outcome) => outcome,
            None => unreachable!("executed transaction outcome was already taken"),
        }
    }

    /// Accepts the transaction state into the internal accepted overlay.
    ///
    /// This makes the transaction's state effects visible to later transactions executed by the
    /// same EVM. It clears transaction scratch and returns the result-only [`TxOutcome`].
    pub fn commit(mut self) -> TxOutcome<T> {
        self.commit_pending_state();
        self.take_outcome()
    }

    /// Accepts the transaction state and records its changes in a block accumulator.
    ///
    /// This streams transaction changes into `block_state`, commits them to the accepted overlay,
    /// and returns the result-only [`TxOutcome`]. No owned [`StateChanges`] is materialized.
    pub fn commit_to(mut self, block_state: &mut BlockStateAccumulator) -> TxOutcome<T> {
        if self.has_pending_state() {
            match self.evm.state.visit_transaction_changes(block_state) {
                Ok(()) => {}
                Err(err) => match err {},
            }
            self.commit_pending_state();
        }
        self.take_outcome()
    }

    /// Streams transaction changes into `sink`, then accepts the transaction.
    ///
    /// If the sink returns an error, the transaction is not committed and the executed handle is
    /// dropped, which discards the transaction scratch. Use infallible sinks on the block hot path.
    pub fn commit_with<S: StateChangeSink>(
        mut self,
        sink: &mut S,
    ) -> Result<TxOutcome<T>, S::Error> {
        if self.has_pending_state() {
            self.evm.state.visit_transaction_changes(sink)?;
            self.commit_pending_state();
        }
        Ok(self.take_outcome())
    }

    /// Discards the transaction state and returns the outcome.
    ///
    /// Discarding does not mutate the accepted overlay and does not materialize [`StateChanges`].
    /// This is the intended path for result-only execution such as `eth_call`.
    pub fn discard(mut self) -> TxOutcome<T> {
        self.clear_pending_state();
        self.take_outcome()
    }

    /// Detaches the transaction into an owned state diff without committing it.
    ///
    /// Detaching materializes [`StateChanges`], clears transaction scratch, and returns a
    /// [`TxResult`] that can be moved or stored. The detached state is not accepted into this EVM's
    /// internal overlay unless the caller commits it separately.
    pub fn detach(mut self) -> TxResult<T> {
        let state_changes = self.take_state_changes();
        let outcome = self.take_outcome();
        TxResult {
            status: outcome.status,
            gas_used: outcome.gas_used,
            stop: outcome.stop,
            output: outcome.output,
            logs: outcome.logs,
            state_changes,
            db_error_code: outcome.db_error_code,
            ext: outcome.ext,
            _non_exhaustive: (),
        }
    }
}

impl<T: EvmTypes> Drop for ExecutedTx<'_, T> {
    #[inline]
    fn drop(&mut self) {
        self.clear_pending_state();
    }
}

/// Result of executing a transaction with an owned state diff.
///
/// This is the materialized shape produced by [`ExecutedTx::detach`]. It
/// pairs [`TxOutcome`]-style execution output with an owned [`StateChanges`] value. Prefer
/// resolving [`Evm::transact`] with [`ExecutedTx::commit`] or [`ExecutedTx::discard`] when an owned
/// write-set is unnecessary.
#[derive_where(Clone, Debug, Default, PartialEq, Eq; T::TxResultExt)]
pub struct TxResult<T: EvmTypes = crate::BaseEvmTypes> {
    /// Whether execution succeeded.
    pub status: bool,
    /// Gas used by execution.
    pub gas_used: u64,
    /// Interpreter stop reason.
    pub stop: InstrStop,
    /// Return or revert output.
    pub output: Bytes,
    /// Logs emitted by the transaction.
    pub logs: Vec<Log>,
    /// State transition produced by this transaction.
    pub state_changes: StateChanges,
    /// Database error handle, if execution stopped on a database error.
    pub db_error_code: Option<DbErrorCode>,
    /// EVM type-specific extension data.
    pub ext: T::TxResultExt,
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
}
