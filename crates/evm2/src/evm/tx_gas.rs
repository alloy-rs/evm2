//! Alternate gas parameters for a subset of transactions.
//!
//! A [`TxGasSchedule`] gives the transactions it selects, such as the transactions of one lane,
//! a precomputed gas table instead of the EVM's own. Around each selected transaction the EVM swaps
//! the two tables in place and swaps them back afterwards, so the opcode loop keeps reading gas
//! parameters inline from the execution config.

use super::{Evm, config::EvmTypesHost};
use crate::{
    EvmTypes,
    version::{GasId, GasParams},
};

/// Gas parameters for the transactions [`Self::select`] picks. See [`Evm::set_tx_gas_schedule`].
pub struct TxGasSchedule<T: EvmTypesHost> {
    /// Returns whether a transaction runs with [`Self::gas_params`].
    ///
    /// Called before validating or dispatching each transaction, so it should be cheap, such as
    /// reading a flag the transaction environment caches.
    pub select: fn(&T::Tx) -> bool,
    /// The gas parameters selected transactions run with, precomputed for the EVM's current
    /// execution config.
    pub gas_params: GasParams,
}

impl<T: EvmTypesHost> Clone for TxGasSchedule<T> {
    #[inline]
    fn clone(&self) -> Self {
        Self { select: self.select, gas_params: self.gas_params }
    }
}

impl<T: EvmTypesHost> core::fmt::Debug for TxGasSchedule<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("TxGasSchedule").finish_non_exhaustive()
    }
}

/// An installed [`TxGasSchedule`].
pub(super) struct TxGasState<T: EvmTypesHost> {
    select: fn(&T::Tx) -> bool,
    /// The table that is not installed: the schedule's while the EVM's own is installed, and the
    /// EVM's own while the schedule's is.
    other: GasParams,
    /// The interpreter runner's verdict for [`Self::other`].
    other_runner_supported: bool,
    /// Whether the schedule's table is installed.
    swapped: bool,
}

/// Puts back the table installed before a swap when dropped, including on unwind.
pub(super) struct TxGasGuard<'guard, 'evm, T: EvmTypes> {
    pub(super) evm: &'guard mut Evm<'evm, T>,
    /// Whether the schedule's table was installed before the swap, or `None` when nothing was
    /// swapped.
    restore: Option<bool>,
}

impl<T: EvmTypes> Drop for TxGasGuard<'_, '_, T> {
    #[inline]
    fn drop(&mut self) {
        if let Some(swapped) = self.restore {
            self.evm.set_tx_gas_swapped(swapped);
        }
    }
}

impl<'a, T: EvmTypes> Evm<'a, T> {
    /// Installs or removes gas parameters for the transactions a schedule selects.
    ///
    /// Every transaction [`TxGasSchedule::select`] picks runs with [`TxGasSchedule::gas_params`]
    /// instead of the EVM's own gas parameters, which come back afterwards, including when the
    /// transaction unwinds. This prices transactions of one block differently, such as by the lane
    /// they run in, without rebuilding the EVM, and applies to every caller of
    /// [`Self::transact`], [`Self::transact_with_inspector`] and [`Self::validate_tx`], so
    /// validation charges the same intrinsic gas execution does. Only gas parameters change: the
    /// rest of the EVM's [`Version`](crate::Version) applies to every transaction.
    ///
    /// The schedule's table is precomputed for the current execution config, so changing the
    /// execution config removes the schedule: install it again for the new config. Changing the
    /// interpreter runner keeps it. System calls, including [`Self::execute_system_call`] from a
    /// handler, run with the EVM's own gas parameters. A transaction a handler dispatches while a
    /// selected transaction runs keeps the schedule's gas parameters.
    ///
    /// # Panics
    ///
    /// Panics if called during execution, or if the schedule's gas parameters have a zero
    /// [`GasId::MaxRefundQuotient`].
    pub fn set_tx_gas_schedule(&mut self, schedule: Option<TxGasSchedule<T>>) {
        self.assert_execution_config_mutable();
        self.set_tx_gas_swapped(false);
        self.tx_gas = schedule.map(|schedule| {
            assert!(
                schedule.gas_params[GasId::MaxRefundQuotient] != 0,
                "schedule max refund quotient must be non-zero"
            );
            TxGasState {
                select: schedule.select,
                other: schedule.gas_params,
                other_runner_supported: false,
                swapped: false,
            }
        });
        self.refresh_tx_gas_runner_support();
    }

    /// Records the interpreter runner's verdict for the table that is not installed.
    pub(super) fn refresh_tx_gas_runner_support(&mut self) {
        if self.tx_gas.is_none() {
            return;
        }
        self.toggle_tx_gas();
        self.runner_supported = self
            .interpreter_runner
            .as_ref()
            .is_some_and(|runner| runner.supports(&self.execution_config));
        self.toggle_tx_gas();
    }

    /// Swaps in the schedule's table if it selects `tx`, returning a guard that swaps back.
    #[inline]
    pub(super) fn tx_gas_guard(&mut self, tx: &T::Tx) -> TxGasGuard<'_, 'a, T> {
        let restore = match &self.tx_gas {
            Some(state) if !state.swapped && (state.select)(tx) => {
                self.toggle_tx_gas();
                Some(false)
            }
            _ => None,
        };
        TxGasGuard { evm: self, restore }
    }

    /// Runs `f` with the EVM's own gas parameters, swapping the schedule's back in afterwards.
    #[inline]
    pub(crate) fn with_own_gas_params<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        let restore = match &self.tx_gas {
            Some(state) if state.swapped => {
                self.toggle_tx_gas();
                Some(true)
            }
            _ => None,
        };
        let guard = TxGasGuard { evm: self, restore };
        f(guard.evm)
    }

    /// Installs the schedule's table when `swapped`, else the EVM's own.
    #[inline]
    fn set_tx_gas_swapped(&mut self, swapped: bool) {
        if self.tx_gas.as_ref().is_some_and(|state| state.swapped != swapped) {
            self.toggle_tx_gas();
        }
    }

    /// Swaps the installed table and its runner verdict with the other one.
    #[inline]
    const fn toggle_tx_gas(&mut self) {
        let Some(state) = &mut self.tx_gas else { return };
        core::mem::swap(&mut self.execution_config.version.gas_params, &mut state.other);
        core::mem::swap(&mut self.runner_supported, &mut state.other_runner_supported);
        state.swapped = !state.swapped;
    }
}
