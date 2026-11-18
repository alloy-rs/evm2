//! Alternate gas schedules for a subset of transactions.
//!
//! A [`TxGasSchedule`] gives some transactions, such as the transactions of one lane, different gas
//! parameters from the rest of the block. The EVM derives the alternate parameters once per
//! execution config, keeps only the entries that differ, and swaps those entries in place around
//! each selected transaction. A transaction that keeps the EVM's own parameters costs one call to
//! [`TxGasSchedule::select`], and a selected one costs two swaps of the differing entries: the
//! opcode loop keeps reading gas parameters inline from the execution config.

use super::{Evm, config::EvmTypesHost};
use crate::{
    EvmTypes,
    version::{GasId, GasParams},
};
use alloc::vec::Vec;

/// Alternate gas parameters for the transactions [`Self::select`] picks.
///
/// Only gas parameters change: the rest of the EVM's [`Version`](crate::Version), such as its
/// chain ID, gas caps, memory limit and features, applies to every transaction. See
/// [`Evm::set_tx_gas_schedule`].
pub struct TxGasSchedule<T: EvmTypesHost> {
    /// Derives the alternate gas parameters from the EVM's own for its runtime specification ID,
    /// or returns `None` when every transaction runs with the EVM's own.
    ///
    /// Called when the schedule is installed and whenever the EVM's execution config changes,
    /// never per transaction.
    pub derive: fn(T::SpecId, &GasParams) -> Option<GasParams>,
    /// Returns whether a transaction runs with the alternate gas parameters.
    ///
    /// Called before validating or dispatching each transaction, so it should be cheap, such as
    /// reading a flag the transaction environment caches.
    pub select: fn(&T::Tx) -> bool,
}

impl<T: EvmTypesHost> Clone for TxGasSchedule<T> {
    #[inline]
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: EvmTypesHost> Copy for TxGasSchedule<T> {}

impl<T: EvmTypesHost> core::fmt::Debug for TxGasSchedule<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("TxGasSchedule").finish_non_exhaustive()
    }
}

/// An installed [`TxGasSchedule`] and the entries it changes.
pub(super) struct TxGasState<T: EvmTypesHost> {
    schedule: TxGasSchedule<T>,
    /// The entries where the alternate parameters differ, each with the value of the table that is
    /// not installed: swapping them toggles the installed table.
    diff: Vec<(GasId, u32)>,
    /// Whether the alternate parameters are installed.
    alternate: bool,
    /// The interpreter runner's verdict for the EVM's own parameters.
    own_runner_supported: bool,
    /// The interpreter runner's verdict for the alternate parameters.
    alternate_runner_supported: bool,
    /// Changes whenever the diff is derived again, so a stale [`TxGasRestore`] restores nothing.
    epoch: u32,
}

impl<T: EvmTypesHost> TxGasState<T> {
    /// Returns how many gas parameters each switch swaps.
    #[cfg(test)]
    pub(super) const fn swapped_entries(&self) -> usize {
        self.diff.len()
    }

    #[inline]
    fn toggle(&mut self, gas_params: &mut GasParams) {
        for (id, value) in &mut self.diff {
            core::mem::swap(&mut gas_params[*id], value);
        }
        self.alternate = !self.alternate;
    }
}

/// Undoes one switch of the installed gas parameters.
#[derive(Clone, Copy)]
pub(super) struct TxGasRestore {
    epoch: u32,
}

/// Undoes a switch of the installed gas parameters when dropped, including on unwind.
pub(super) struct TxGasGuard<'guard, 'evm, T: EvmTypes> {
    pub(super) evm: &'guard mut Evm<'evm, T>,
    restore: Option<TxGasRestore>,
}

impl<T: EvmTypes> Drop for TxGasGuard<'_, '_, T> {
    #[inline]
    fn drop(&mut self) {
        if let Some(restore) = self.restore.take() {
            self.evm.toggle_tx_gas(restore);
        }
    }
}

impl<'a, T: EvmTypes> Evm<'a, T> {
    /// Returns the installed alternate gas schedule, if any.
    #[inline]
    pub fn tx_gas_schedule(&self) -> Option<TxGasSchedule<T>> {
        self.tx_gas.as_ref().map(|state| state.schedule)
    }

    /// Installs or removes an alternate gas schedule for some transactions.
    ///
    /// The EVM derives the alternate gas parameters now and whenever its execution config changes,
    /// and runs every transaction [`TxGasSchedule::select`] picks with them, restoring its own
    /// afterwards, including when the transaction unwinds. This prices transactions of one block
    /// differently, such as by the lane they run in, without rebuilding the EVM, and applies to
    /// every caller of [`Self::transact`], [`Self::transact_with_inspector`] and
    /// [`Self::validate_tx`], so validation charges the same intrinsic gas execution does.
    ///
    /// A transaction a handler dispatches while another runs is selected on its own. System calls,
    /// including [`Self::execute_system_call`] from a handler, run with the EVM's own parameters.
    /// If a handler replaces the execution config, the EVM keeps the new config's parameters.
    ///
    /// # Panics
    ///
    /// Panics if called during execution, or if [`TxGasSchedule::derive`] returns gas parameters
    /// with a zero [`GasId::MaxRefundQuotient`], here or when the execution config changes.
    pub fn set_tx_gas_schedule(&mut self, schedule: Option<TxGasSchedule<T>>) {
        self.assert_execution_config_mutable();
        self.restore_own_gas_params();
        self.tx_gas = schedule.map(|schedule| TxGasState {
            schedule,
            diff: Vec::new(),
            alternate: false,
            own_runner_supported: false,
            alternate_runner_supported: false,
            epoch: 0,
        });
        self.derive_tx_gas();
    }

    /// Puts the EVM's own gas parameters back if the alternate ones are installed.
    fn restore_own_gas_params(&mut self) {
        if let Some(state) = &mut self.tx_gas
            && state.alternate
        {
            state.toggle(&mut self.execution_config.version.gas_params);
            self.runner_supported = state.own_runner_supported;
        }
    }

    /// Derives the alternate gas parameters from the installed config, whose gas parameters are
    /// the EVM's own.
    pub(super) fn derive_tx_gas(&mut self) {
        let Some(state) = &mut self.tx_gas else { return };
        state.epoch = state.epoch.wrapping_add(1);
        state.alternate = false;
        state.diff.clear();
        let own = &self.execution_config.version.gas_params;
        if let Some(alternate) = (state.schedule.derive)(self.spec_id, own) {
            assert!(
                alternate.get(GasId::MaxRefundQuotient) != 0,
                "alternate max refund quotient must be non-zero"
            );
            state.diff.extend(
                (0..GasId::COUNT)
                    .filter_map(GasId::from_usize)
                    .filter(|&id| alternate[id] != own[id])
                    .map(|id| (id, alternate[id])),
            );
        }
        self.refresh_tx_gas_runner_support();
    }

    /// Records the interpreter runner's verdict for both gas parameter tables.
    pub(super) fn refresh_tx_gas_runner_support(&mut self) {
        let installed = self.runner_supported;
        let Some(state) = &mut self.tx_gas else { return };
        let other = if state.diff.is_empty() {
            installed
        } else {
            state.toggle(&mut self.execution_config.version.gas_params);
            let other = self
                .interpreter_runner
                .as_ref()
                .is_some_and(|runner| runner.supports(&self.execution_config));
            state.toggle(&mut self.execution_config.version.gas_params);
            other
        };
        (state.own_runner_supported, state.alternate_runner_supported) =
            if state.alternate { (other, installed) } else { (installed, other) };
    }

    /// Installs the gas parameters `tx` runs with, returning a guard that restores the previous
    /// ones.
    #[inline]
    pub(super) fn tx_gas_guard(&mut self, tx: &T::Tx) -> TxGasGuard<'_, 'a, T> {
        let restore = match &self.tx_gas {
            Some(state) => {
                let alternate = (state.schedule.select)(tx);
                self.switch_tx_gas(alternate)
            }
            None => None,
        };
        TxGasGuard { evm: self, restore }
    }

    /// Runs `f` with the EVM's own gas parameters, restoring the installed ones afterwards.
    #[inline]
    pub(crate) fn with_own_gas_params<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        let restore = self.switch_tx_gas(false);
        let guard = TxGasGuard { evm: self, restore };
        f(guard.evm)
    }

    /// Installs the alternate or the EVM's own gas parameters, returning how to undo it, or `None`
    /// when they are already installed.
    #[inline]
    fn switch_tx_gas(&mut self, alternate: bool) -> Option<TxGasRestore> {
        let state = self.tx_gas.as_mut()?;
        if state.alternate == alternate || state.diff.is_empty() {
            return None;
        }
        let restore = TxGasRestore { epoch: state.epoch };
        self.toggle_tx_gas(restore);
        Some(restore)
    }

    /// Toggles the installed gas parameters, unless they were derived again since `restore`.
    #[inline]
    fn toggle_tx_gas(&mut self, restore: TxGasRestore) {
        let Some(state) = &mut self.tx_gas else { return };
        if state.epoch != restore.epoch {
            return;
        }
        state.toggle(&mut self.execution_config.version.gas_params);
        self.runner_supported = if state.alternate {
            state.alternate_runner_supported
        } else {
            state.own_runner_supported
        };
    }
}
