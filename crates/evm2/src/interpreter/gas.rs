//! EVM gas calculation utilities.

use super::{InstrStop, Result};
use core::hint::cold_path;

pub(crate) const ZERO: u32 = 0;
pub(crate) const BASE: u32 = 2;
pub(crate) const VERYLOW: u32 = 3;
pub(crate) const LOW: u32 = 5;
pub(crate) const MID: u32 = 8;
pub(crate) const HIGH: u32 = 10;
pub(crate) const JUMPDEST: u32 = 1;
pub(crate) const EXP: u32 = 10;
pub(crate) const MEMORY: u32 = 3;
pub(crate) const LOG: u32 = 375;
pub(crate) const LOGDATA: u32 = 8;
pub(crate) const LOGTOPIC: u32 = 375;
pub(crate) const KECCAK256: u32 = 30;
pub(crate) const KECCAK256WORD: u32 = 6;
pub(crate) const COPY: u32 = 3;
pub(crate) const BLOCKHASH: u32 = 20;
pub(crate) const CREATE: u32 = 32000;
pub(crate) const CALLVALUE: u32 = 9000;
pub(crate) const NEWACCOUNT: u32 = 25000;
pub(crate) const SELFDESTRUCT_REFUND: u32 = 24000;
pub(crate) const CODEDEPOSIT: u32 = 200;
pub(crate) const SSTORE_SET: u32 = 20000;
pub(crate) const SSTORE_RESET: u32 = 5000;
pub(crate) const REFUND_SSTORE_CLEARS: u32 = 15000;
pub(crate) const STANDARD_TOKEN_COST: u32 = 4;
pub(crate) const NON_ZERO_BYTE_MULTIPLIER: u32 = 17;
pub(crate) const NON_ZERO_BYTE_MULTIPLIER_ISTANBUL: u32 = 4;
pub(crate) const TOTAL_COST_FLOOR_PER_TOKEN: u32 = 10;
pub(crate) const TOTAL_COST_FLOOR_PER_TOKEN_AMSTERDAM: u32 = 16;
pub(crate) const INITCODE_WORD_COST: u32 = 2;
pub(crate) const CALL_STIPEND: u32 = 2300;
pub(crate) const ISTANBUL_SLOAD_GAS: u32 = 800;
pub(crate) const ACCESS_LIST_ADDRESS: u32 = 2400;
pub(crate) const ACCESS_LIST_STORAGE_KEY: u32 = 1900;
pub(crate) const COLD_SLOAD_COST: u32 = 2100;
pub(crate) const COLD_ACCOUNT_ACCESS_COST: u32 = 2600;
pub(crate) const COLD_ACCOUNT_ACCESS_COST_ADDITIONAL: u32 =
    COLD_ACCOUNT_ACCESS_COST - WARM_STORAGE_READ_COST;
pub(crate) const WARM_STORAGE_READ_COST: u32 = 100;
pub(crate) const WARM_SSTORE_RESET: u32 = SSTORE_RESET - COLD_SLOAD_COST;
pub(crate) const EIP7702_PER_AUTH_BASE_COST: u32 = 12500;
pub(crate) const EIP7702_PER_EMPTY_ACCOUNT_COST: u32 = 25000;

/// Tracks regular, state, and refunded gas.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct GasTracker {
    remaining: u64,
    gas_limit: u64,
    reservoir: u64,
    state_gas_spent: u64,
    refunded: i64,
}

impl GasTracker {
    /// Creates a gas tracker from its raw counters.
    #[inline]
    pub const fn new(gas_limit: u64, remaining: u64, reservoir: u64) -> Self {
        Self { remaining, gas_limit, reservoir, state_gas_spent: 0, refunded: 0 }
    }

    /// Creates a gas tracker from already used gas.
    #[inline]
    pub const fn new_used_gas(gas_limit: u64, used_gas: u64, reservoir: u64) -> Self {
        Self::new(gas_limit, gas_limit - used_gas, reservoir)
    }

    /// Returns remaining regular gas.
    #[inline]
    pub const fn remaining(&self) -> u64 {
        self.remaining
    }

    /// Sets remaining regular gas.
    #[inline]
    pub const fn set_remaining(&mut self, val: u64) {
        self.remaining = val;
    }

    /// Returns the gas limit.
    #[inline]
    pub const fn limit(&self) -> u64 {
        self.gas_limit
    }

    /// Sets the gas limit.
    #[inline]
    pub const fn set_limit(&mut self, val: u64) {
        self.gas_limit = val;
    }

    /// Returns available state gas reservoir.
    #[inline]
    pub const fn reservoir(&self) -> u64 {
        self.reservoir
    }

    /// Sets available state gas reservoir.
    #[inline]
    pub const fn set_reservoir(&mut self, val: u64) {
        self.reservoir = val;
    }

    /// Returns spent state gas.
    #[inline]
    pub const fn state_gas_spent(&self) -> u64 {
        self.state_gas_spent
    }

    /// Sets spent state gas.
    #[inline]
    pub const fn set_state_gas_spent(&mut self, val: u64) {
        self.state_gas_spent = val;
    }

    /// Returns gas refund.
    #[inline]
    pub const fn refunded(&self) -> i64 {
        self.refunded
    }

    /// Sets gas refund.
    #[inline]
    pub const fn set_refunded(&mut self, val: i64) {
        self.refunded = val;
    }

    /// Returns spent regular gas.
    #[inline]
    pub const fn spent(&self) -> u64 {
        self.gas_limit.saturating_sub(self.remaining)
    }

    /// Returns used gas after refund.
    #[inline]
    pub const fn used(&self) -> u64 {
        self.spent().saturating_sub(self.refunded as u64)
    }

    /// Sets spent regular gas.
    #[inline]
    pub const fn set_spent(&mut self, spent: u64) {
        self.remaining = self.gas_limit.saturating_sub(spent);
    }

    /// Spends regular gas.
    #[doc(alias = "record_cost")]
    #[doc(alias = "record_regular_cost")]
    #[doc(alias = "record_cost_unsafe")]
    #[doc(alias = "spend_unsafe")]
    #[inline]
    pub const fn spend(&mut self, cost: u64) -> Result {
        let remaining = self.remaining;
        self.remaining = remaining.wrapping_sub(cost);
        if remaining < cost {
            cold_path();
            Err(InstrStop::OutOfGas)
        } else {
            Ok(())
        }
    }

    /// Spends state gas.
    #[doc(alias = "record_state_cost")]
    #[inline]
    pub fn spend_state(&mut self, cost: u64) -> Result {
        if self.reservoir >= cost {
            self.state_gas_spent = self.state_gas_spent.saturating_add(cost);
            self.reservoir -= cost;
            return Ok(());
        }

        let spill = cost - self.reservoir;

        self.spend(spill)?;
        self.state_gas_spent = self.state_gas_spent.saturating_add(cost);
        self.reservoir = 0;
        Ok(())
    }

    /// Adds gas refund.
    #[inline]
    pub const fn record_refund(&mut self, refund: i64) {
        self.refunded += refund;
    }

    /// Returns gas to the remaining counter.
    #[inline]
    pub const fn erase_cost(&mut self, returned: u64) {
        self.remaining += returned;
    }

    /// Spends all remaining regular gas.
    #[inline]
    pub const fn spend_all(&mut self) {
        self.remaining = 0;
    }
}

/// Remaining regular gas threaded through dispatch calls.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub(crate) struct RemainingGas(u64);

#[allow(dead_code)]
impl RemainingGas {
    /// Creates a remaining gas counter.
    #[inline]
    pub(crate) const fn new(remaining: u64) -> Self {
        Self(remaining)
    }

    /// Returns remaining regular gas.
    #[inline]
    pub(crate) const fn get(self) -> u64 {
        self.0
    }

    /// Sets remaining regular gas.
    #[inline]
    pub(crate) const fn set(&mut self, remaining: u64) {
        self.0 = remaining;
    }

    /// Spends regular gas.
    #[inline(always)]
    pub(crate) const fn spend(&mut self, cost: u64) -> Result {
        let remaining = self.0;
        self.0 = remaining.wrapping_sub(cost);
        if remaining < cost {
            cold_path();
            Err(InstrStop::OutOfGas)
        } else {
            Ok(())
        }
    }
}

/// Interpreter gas state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(C)] // Puts `tracker`, and so `.remaining`, first.
pub struct Gas {
    tracker: GasTracker,
    memory: MemoryGas,
}

impl Gas {
    /// Creates gas with `limit` regular gas.
    #[inline]
    pub const fn new(limit: u64) -> Self {
        Self { tracker: GasTracker::new(limit, limit, 0), memory: MemoryGas::new() }
    }

    /// Creates gas with regular gas and a state gas reservoir.
    #[inline]
    pub const fn new_with_regular_gas_and_reservoir(limit: u64, reservoir: u64) -> Self {
        Self { tracker: GasTracker::new(limit, limit, reservoir), memory: MemoryGas::new() }
    }

    /// Creates spent gas with a state gas reservoir.
    #[inline]
    pub const fn new_spent_with_reservoir(limit: u64, reservoir: u64) -> Self {
        Self { tracker: GasTracker::new(limit, 0, reservoir), memory: MemoryGas::new() }
    }

    /// Returns the gas tracker.
    #[inline]
    pub const fn tracker(&self) -> &GasTracker {
        &self.tracker
    }

    /// Returns the mutable gas tracker.
    #[inline]
    pub const fn tracker_mut(&mut self) -> &mut GasTracker {
        &mut self.tracker
    }

    /// Returns memory gas state.
    #[inline]
    pub const fn memory(&self) -> &MemoryGas {
        &self.memory
    }

    /// Returns mutable memory gas state.
    #[inline]
    pub const fn memory_mut(&mut self) -> &mut MemoryGas {
        &mut self.memory
    }

    /// Returns remaining regular gas.
    #[inline]
    pub const fn remaining(&self) -> u64 {
        self.tracker.remaining()
    }

    /// Sets remaining regular gas.
    #[inline]
    pub const fn set_remaining(&mut self, remaining: u64) {
        self.tracker.set_remaining(remaining);
    }

    /// Returns the gas limit.
    #[inline]
    pub const fn limit(&self) -> u64 {
        self.tracker.limit()
    }

    /// Sets the gas limit.
    #[inline]
    pub const fn set_limit(&mut self, val: u64) {
        self.tracker.set_limit(val);
    }

    /// Returns available state gas reservoir.
    #[inline]
    pub const fn reservoir(&self) -> u64 {
        self.tracker.reservoir()
    }

    /// Sets available state gas reservoir.
    #[inline]
    pub const fn set_reservoir(&mut self, val: u64) {
        self.tracker.set_reservoir(val);
    }

    /// Returns spent state gas.
    #[inline]
    pub const fn state_gas_spent(&self) -> u64 {
        self.tracker.state_gas_spent()
    }

    /// Sets spent state gas.
    #[inline]
    pub const fn set_state_gas_spent(&mut self, val: u64) {
        self.tracker.set_state_gas_spent(val);
    }

    /// Returns gas refund.
    #[inline]
    pub const fn refunded(&self) -> i64 {
        self.tracker.refunded()
    }

    /// Sets gas refund.
    #[inline]
    pub const fn set_refunded(&mut self, val: i64) {
        self.tracker.set_refunded(val);
    }

    /// Sets gas refund.
    #[inline]
    pub const fn set_refund(&mut self, refund: i64) {
        self.set_refunded(refund);
    }

    /// Returns spent regular gas.
    #[inline]
    pub const fn spent(&self) -> u64 {
        self.tracker.spent()
    }

    /// Returns used gas after refund.
    #[inline]
    pub const fn used(&self) -> u64 {
        self.tracker.used()
    }

    /// Sets spent regular gas.
    #[inline]
    pub const fn set_spent(&mut self, spent: u64) {
        self.tracker.set_spent(spent);
    }

    /// Spends regular gas or returns out of gas.
    #[doc(alias = "record_cost")]
    #[doc(alias = "record_regular_cost")]
    #[doc(alias = "record_cost_unsafe")]
    #[doc(alias = "spend_unsafe")]
    #[inline(always)]
    pub const fn spend(&mut self, amount: u64) -> Result {
        self.tracker.spend(amount)
    }

    /// Spends state gas.
    #[doc(alias = "record_state_cost")]
    #[inline]
    pub fn spend_state(&mut self, cost: u64) -> Result {
        self.tracker.spend_state(cost)
    }

    /// Adds gas refund.
    #[inline]
    pub const fn record_refund(&mut self, refund: i64) {
        self.tracker.record_refund(refund);
    }

    /// Returns gas to the remaining counter.
    #[inline]
    pub const fn erase_cost(&mut self, returned: u64) {
        self.tracker.erase_cost(returned);
    }

    /// Spends all remaining regular gas.
    #[inline]
    pub const fn spend_all(&mut self) {
        self.tracker.spend_all();
    }
}

/// Memory gas accounting state.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq, Hash)]
pub struct MemoryGas {
    /// Current memory size in EVM words.
    pub words_num: usize,
    /// Current total expansion cost.
    pub expansion_cost: u64,
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
}

impl MemoryGas {
    /// Creates empty memory gas state.
    #[inline]
    pub const fn new() -> Self {
        Self { words_num: 0, expansion_cost: 0, _non_exhaustive: () }
    }

    /// Sets memory word count and returns the expansion cost delta.
    #[inline]
    pub const fn set_words_num(
        &mut self,
        words_num: usize,
        mut expansion_cost: u64,
    ) -> Option<u64> {
        self.words_num = words_num;
        core::mem::swap(&mut self.expansion_cost, &mut expansion_cost);
        self.expansion_cost.checked_sub(expansion_cost)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spend_state() {
        let mut gas = Gas::new_with_regular_gas_and_reservoir(1000, 500);
        assert!(gas.spend_state(200).is_ok());
        assert_eq!((gas.reservoir(), gas.remaining(), gas.state_gas_spent()), (300, 1000, 200));

        let mut gas = Gas::new_with_regular_gas_and_reservoir(1000, 500);
        assert!(gas.spend_state(500).is_ok());
        assert_eq!((gas.reservoir(), gas.remaining(), gas.state_gas_spent()), (0, 1000, 500));

        let mut gas = Gas::new_with_regular_gas_and_reservoir(1000, 300);
        assert!(gas.spend_state(500).is_ok());
        assert_eq!((gas.reservoir(), gas.remaining(), gas.state_gas_spent()), (0, 800, 500));

        let mut gas = Gas::new(1000);
        assert!(gas.spend_state(200).is_ok());
        assert_eq!((gas.reservoir(), gas.remaining(), gas.state_gas_spent()), (0, 800, 200));

        let mut gas = Gas::new_with_regular_gas_and_reservoir(100, 50);
        assert!(gas.spend_state(0).is_ok());
        assert_eq!((gas.reservoir(), gas.remaining(), gas.state_gas_spent()), (50, 100, 0));

        let mut gas = Gas::new_with_regular_gas_and_reservoir(100, 50);
        assert!(matches!(gas.spend_state(200), Err(InstrStop::OutOfGas)));

        let mut gas = Gas::new_with_regular_gas_and_reservoir(2000, 1000);
        assert!(gas.spend_state(100).is_ok());
        assert!(gas.spend_state(200).is_ok());
        assert!(gas.spend_state(150).is_ok());
        assert_eq!(gas.state_gas_spent(), 450);

        let mut gas = Gas::new_with_regular_gas_and_reservoir(500, 300);
        assert!(gas.spend_state(150).is_ok());
        assert_eq!((gas.reservoir(), gas.remaining()), (150, 500));
        assert!(gas.spend_state(200).is_ok());
        assert_eq!((gas.reservoir(), gas.remaining()), (0, 450));
        assert!(gas.spend_state(100).is_ok());
        assert_eq!((gas.reservoir(), gas.remaining(), gas.state_gas_spent()), (0, 350, 450));
    }

    #[test]
    fn test_spend_state_oog_does_not_inflate_state_gas_spent() {
        let mut gas = Gas::new(30);
        assert!(matches!(gas.spend_state(100), Err(InstrStop::OutOfGas)));
        assert_eq!(gas.state_gas_spent(), 0);

        let mut gas = Gas::new_with_regular_gas_and_reservoir(30, 20);
        assert!(matches!(gas.spend_state(100), Err(InstrStop::OutOfGas)));
        assert_eq!(gas.state_gas_spent(), 0);
        assert_eq!(gas.reservoir(), 20);
    }

    #[test]
    fn test_spend_state_zero_remaining_with_reservoir() {
        let mut gas = Gas::new_with_regular_gas_and_reservoir(0, 500);
        assert!(gas.spend_state(200).is_ok());
        assert_eq!((gas.reservoir(), gas.remaining(), gas.state_gas_spent()), (300, 0, 200));

        assert!(gas.spend_state(300).is_ok());
        assert_eq!((gas.reservoir(), gas.remaining(), gas.state_gas_spent()), (0, 0, 500));

        assert!(matches!(gas.spend_state(1), Err(InstrStop::OutOfGas)));
    }
}
