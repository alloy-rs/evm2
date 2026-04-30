//! EVM gas calculation utilities.

use super::{InstrErr, Result};
use core::hint::cold_path;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct GasTracker {
    gas_limit: u64,
    remaining: u64,
    reservoir: u64,
    state_gas_spent: u64,
    refunded: i64,
}

impl GasTracker {
    #[inline]
    pub const fn new(gas_limit: u64, remaining: u64, reservoir: u64) -> Self {
        Self { gas_limit, remaining, reservoir, state_gas_spent: 0, refunded: 0 }
    }

    #[inline]
    pub const fn new_used_gas(gas_limit: u64, used_gas: u64, reservoir: u64) -> Self {
        Self::new(gas_limit, gas_limit - used_gas, reservoir)
    }

    #[inline]
    pub const fn limit(&self) -> u64 {
        self.gas_limit
    }

    #[inline]
    pub const fn set_limit(&mut self, val: u64) {
        self.gas_limit = val;
    }

    #[inline]
    pub const fn remaining(&self) -> u64 {
        self.remaining
    }

    #[inline]
    pub const fn set_remaining(&mut self, val: u64) {
        self.remaining = val;
    }

    #[inline]
    pub const fn reservoir(&self) -> u64 {
        self.reservoir
    }

    #[inline]
    pub const fn set_reservoir(&mut self, val: u64) {
        self.reservoir = val;
    }

    #[inline]
    pub const fn state_gas_spent(&self) -> u64 {
        self.state_gas_spent
    }

    #[inline]
    pub const fn set_state_gas_spent(&mut self, val: u64) {
        self.state_gas_spent = val;
    }

    #[inline]
    pub const fn refunded(&self) -> i64 {
        self.refunded
    }

    #[inline]
    pub const fn set_refunded(&mut self, val: i64) {
        self.refunded = val;
    }

    #[inline]
    #[must_use = "In case of not enough gas, the interpreter should halt with an out-of-gas error"]
    pub const fn record_regular_cost(&mut self, cost: u64) -> bool {
        if let Some(new_remaining) = self.remaining.checked_sub(cost) {
            self.remaining = new_remaining;
            return true;
        }
        false
    }

    #[inline]
    #[must_use = "In case of not enough gas, the interpreter should halt with an out-of-gas error"]
    pub const fn record_state_cost(&mut self, cost: u64) -> bool {
        if self.reservoir >= cost {
            self.state_gas_spent = self.state_gas_spent.saturating_add(cost);
            self.reservoir -= cost;
            return true;
        }

        let spill = cost - self.reservoir;

        let success = self.record_regular_cost(spill);
        if success {
            self.state_gas_spent = self.state_gas_spent.saturating_add(cost);
            self.reservoir = 0;
        }
        success
    }

    #[inline]
    pub const fn record_refund(&mut self, refund: i64) {
        self.refunded += refund;
    }

    #[inline]
    pub const fn erase_cost(&mut self, returned: u64) {
        self.remaining += returned;
    }

    #[inline]
    pub const fn spend_all(&mut self) {
        self.remaining = 0;
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Gas {
    tracker: GasTracker,
    memory: MemoryGas,
}

pub type GasRef<'a> = &'a mut Gas;

impl Gas {
    #[inline]
    pub const fn new(limit: u64) -> Self {
        Self { tracker: GasTracker::new(limit, limit, 0), memory: MemoryGas::new() }
    }

    #[inline]
    pub const fn tracker(&self) -> &GasTracker {
        &self.tracker
    }

    #[inline]
    pub const fn tracker_mut(&mut self) -> &mut GasTracker {
        &mut self.tracker
    }

    #[inline]
    pub const fn new_with_regular_gas_and_reservoir(limit: u64, reservoir: u64) -> Self {
        Self { tracker: GasTracker::new(limit, limit, reservoir), memory: MemoryGas::new() }
    }

    #[inline]
    pub const fn new_spent_with_reservoir(limit: u64, reservoir: u64) -> Self {
        Self { tracker: GasTracker::new(limit, 0, reservoir), memory: MemoryGas::new() }
    }

    #[inline]
    pub const fn limit(&self) -> u64 {
        self.tracker.limit()
    }

    #[inline]
    pub const fn memory(&self) -> &MemoryGas {
        &self.memory
    }

    #[inline]
    pub const fn memory_mut(&mut self) -> &mut MemoryGas {
        &mut self.memory
    }

    #[inline]
    pub const fn refunded(&self) -> i64 {
        self.tracker.refunded()
    }

    #[inline]
    #[deprecated(
        since = "32.0.0",
        note = "After EIP-8037 gas is split on
    regular and state gas, this method is no longer valid.
    Use [`Gas::total_gas_spent`] instead"
    )]
    pub const fn spent(&self) -> u64 {
        self.tracker.limit().saturating_sub(self.tracker.remaining())
    }

    #[inline]
    pub const fn total_gas_spent(&self) -> u64 {
        self.tracker.limit().saturating_sub(self.tracker.remaining())
    }

    #[inline]
    pub const fn used(&self) -> u64 {
        self.total_gas_spent().saturating_sub(self.refunded() as u64)
    }

    #[inline]
    pub const fn spent_sub_refunded(&self) -> u64 {
        self.total_gas_spent().saturating_sub(self.tracker.refunded() as u64)
    }

    #[inline]
    pub const fn remaining(&self) -> u64 {
        self.tracker.remaining()
    }

    #[inline]
    pub const fn reservoir(&self) -> u64 {
        self.tracker.reservoir()
    }

    #[inline]
    pub const fn set_reservoir(&mut self, val: u64) {
        self.tracker.set_reservoir(val);
    }

    #[inline]
    pub const fn state_gas_spent(&self) -> u64 {
        self.tracker.state_gas_spent()
    }

    #[inline]
    pub const fn set_state_gas_spent(&mut self, val: u64) {
        self.tracker.set_state_gas_spent(val);
    }

    #[inline]
    pub const fn erase_cost(&mut self, returned: u64) {
        self.tracker.erase_cost(returned);
    }

    #[inline]
    pub const fn spend_all(&mut self) {
        self.tracker.spend_all();
    }

    #[inline]
    pub const fn record_refund(&mut self, refund: i64) {
        self.tracker.record_refund(refund);
    }

    #[inline]
    pub fn set_final_refund(&mut self, is_london: bool) {
        let max_refund_quotient = if is_london { 5 } else { 2 };
        let gas_used = self.total_gas_spent().saturating_sub(self.reservoir());
        self.tracker
            .set_refunded((self.refunded() as u64).min(gas_used / max_refund_quotient) as i64);
    }

    #[inline]
    pub const fn set_refund(&mut self, refund: i64) {
        self.tracker.set_refunded(refund);
    }

    #[inline]
    pub const fn set_remaining(&mut self, remaining: u64) {
        self.tracker.set_remaining(remaining);
    }

    #[inline]
    pub const fn set_spent(&mut self, spent: u64) {
        self.tracker.set_remaining(self.tracker.limit().saturating_sub(spent));
    }

    #[inline]
    #[must_use = "prefer using `gas!` instead to return an out-of-gas error on failure"]
    #[deprecated(since = "32.0.0", note = "use record_regular_cost instead")]
    pub const fn record_cost(&mut self, cost: u64) -> bool {
        self.record_regular_cost(cost)
    }

    #[inline(always)]
    #[must_use = "In case of not enough gas, the interpreter should halt with an out-of-gas error"]
    pub const fn record_cost_unsafe(&mut self, cost: u64) -> bool {
        let remaining = self.tracker.remaining();
        let oog = remaining < cost;
        self.tracker.set_remaining(remaining.wrapping_sub(cost));
        oog
    }

    #[inline]
    #[must_use = "In case of not enough gas, the interpreter should halt with an out-of-gas error"]
    pub const fn record_state_cost(&mut self, cost: u64) -> bool {
        self.tracker.record_state_cost(cost)
    }

    #[inline]
    #[must_use = "In case of not enough gas, the interpreter should halt with an out-of-gas error"]
    pub const fn record_regular_cost(&mut self, cost: u64) -> bool {
        self.tracker.record_regular_cost(cost)
    }

    #[inline(always)]
    pub fn spend(&mut self, amount: u64) -> Result {
        if !self.record_regular_cost(amount) {
            cold_path();
            Err(InstrErr::OutOfGas)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum MemoryExtensionResult {
    Extended,
    Same,
    OutOfGas,
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq, Hash)]
pub struct MemoryGas {
    pub words_num: usize,
    pub expansion_cost: u64,
}

impl MemoryGas {
    #[inline]
    pub const fn new() -> Self {
        Self { words_num: 0, expansion_cost: 0 }
    }

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
    fn test_record_state_cost() {
        let mut gas = Gas::new_with_regular_gas_and_reservoir(1000, 500);
        assert!(gas.record_state_cost(200));
        assert_eq!((gas.reservoir(), gas.remaining(), gas.state_gas_spent()), (300, 1000, 200));

        let mut gas = Gas::new_with_regular_gas_and_reservoir(1000, 500);
        assert!(gas.record_state_cost(500));
        assert_eq!((gas.reservoir(), gas.remaining(), gas.state_gas_spent()), (0, 1000, 500));

        let mut gas = Gas::new_with_regular_gas_and_reservoir(1000, 300);
        assert!(gas.record_state_cost(500));
        assert_eq!((gas.reservoir(), gas.remaining(), gas.state_gas_spent()), (0, 800, 500));

        let mut gas = Gas::new(1000);
        assert!(gas.record_state_cost(200));
        assert_eq!((gas.reservoir(), gas.remaining(), gas.state_gas_spent()), (0, 800, 200));

        let mut gas = Gas::new_with_regular_gas_and_reservoir(100, 50);
        assert!(gas.record_state_cost(0));
        assert_eq!((gas.reservoir(), gas.remaining(), gas.state_gas_spent()), (50, 100, 0));

        let mut gas = Gas::new_with_regular_gas_and_reservoir(100, 50);
        assert!(!gas.record_state_cost(200));

        let mut gas = Gas::new_with_regular_gas_and_reservoir(2000, 1000);
        assert!(gas.record_state_cost(100));
        assert!(gas.record_state_cost(200));
        assert!(gas.record_state_cost(150));
        assert_eq!(gas.state_gas_spent(), 450);

        let mut gas = Gas::new_with_regular_gas_and_reservoir(500, 300);
        assert!(gas.record_state_cost(150));
        assert_eq!((gas.reservoir(), gas.remaining()), (150, 500));
        assert!(gas.record_state_cost(200));
        assert_eq!((gas.reservoir(), gas.remaining()), (0, 450));
        assert!(gas.record_state_cost(100));
        assert_eq!((gas.reservoir(), gas.remaining(), gas.state_gas_spent()), (0, 350, 450));
    }

    #[test]
    fn test_record_state_cost_oog_inflates_state_gas_spent() {
        let mut gas = Gas::new(30);
        assert!(!gas.record_state_cost(100));
        assert_eq!(gas.state_gas_spent(), 0);

        let mut gas = Gas::new_with_regular_gas_and_reservoir(30, 20);
        assert!(!gas.record_state_cost(100));
        assert_eq!(gas.state_gas_spent(), 0);
        assert_eq!(gas.reservoir(), 20);
    }

    #[test]
    fn test_record_state_cost_zero_remaining_with_reservoir() {
        let mut gas = Gas::new_with_regular_gas_and_reservoir(0, 500);
        assert!(gas.record_state_cost(200));
        assert_eq!((gas.reservoir(), gas.remaining(), gas.state_gas_spent()), (300, 0, 200));

        assert!(gas.record_state_cost(300));
        assert_eq!((gas.reservoir(), gas.remaining(), gas.state_gas_spent()), (0, 0, 500));

        assert!(!gas.record_state_cost(1));
    }
}
