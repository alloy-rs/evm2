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
pub(crate) const EIP2930_ACCESS_LIST_ADDRESS: u32 = 2400;
pub(crate) const EIP2930_ACCESS_LIST_STORAGE_KEY: u32 = 1900;
pub(crate) const EIP7981_ACCESS_LIST_DATA_COST_PER_BYTE: u32 = 64;
pub(crate) const EIP7981_ACCESS_LIST_FLOOR_BYTE_MULTIPLIER: u32 = 4;
pub(crate) const COLD_SLOAD_COST: u32 = 2100;
pub(crate) const COLD_ACCOUNT_ACCESS_COST: u32 = 2600;
pub(crate) const COLD_ACCOUNT_ACCESS_COST_ADDITIONAL: u32 =
    COLD_ACCOUNT_ACCESS_COST - WARM_STORAGE_READ_COST;
pub(crate) const WARM_STORAGE_READ_COST: u32 = 100;
pub(crate) const WARM_SSTORE_RESET: u32 = SSTORE_RESET - COLD_SLOAD_COST;
pub(crate) const EIP7702_PER_AUTH_BASE_COST: u32 = 12500;
pub(crate) const EIP7702_PER_EMPTY_ACCOUNT_COST: u32 = 25000;

// EIP-8038: State-access gas cost update. Increases the gas cost of state-access
// operations to reflect Ethereum's larger state. Values are the parameters
// proposed in ethereum/EIPs#11802 (still an open draft — preliminary). Active
// alongside EIP-8037 starting at the Amsterdam hardfork.
/// Touch of an already-warm account or storage slot. Unchanged by EIP-8038.
pub(crate) const EIP8038_WARM_ACCESS: u32 = 100;
/// Cold touch of an account (was 2,600 pre-EIP-8038).
pub(crate) const EIP8038_COLD_ACCOUNT_ACCESS: u32 = 3000;
/// Cold touch of a storage slot (was 2,100 pre-EIP-8038).
pub(crate) const EIP8038_COLD_STORAGE_ACCESS: u32 = 3000;
/// First-time account-write surcharge (was 6,700 pre-EIP-8038).
pub(crate) const EIP8038_ACCOUNT_WRITE: u32 = 8000;
/// First-time storage-write surcharge (was 2,800 pre-EIP-8038).
pub(crate) const EIP8038_STORAGE_WRITE: u32 = 10000;
/// Refund for clearing a storage slot (was 4,800 pre-EIP-8038).
///
/// Derived per the spec as `(STORAGE_WRITE + COLD_STORAGE_ACCESS) * 4800 / 5000`
/// = 12,480.
pub(crate) const EIP8038_STORAGE_CLEAR_REFUND: u32 =
    (EIP8038_STORAGE_WRITE + EIP8038_COLD_STORAGE_ACCESS) * 4800 / 5000;
/// State-access cost for contract deployment (was 7,000 pre-EIP-8038).
///
/// Per the spec, `CREATE_ACCESS = ACCOUNT_WRITE + COLD_STORAGE_ACCESS` = 11,000.
/// This does not match the legacy decomposition (`GAS_CREATE - GAS_NEW_ACCOUNT`);
/// the EIP keeps that discrepancy rather than reconciling it.
pub(crate) const EIP8038_CREATE_ACCESS: u32 = EIP8038_ACCOUNT_WRITE + EIP8038_COLD_STORAGE_ACCESS;
/// Access-list per-address base cost, `COLD_ACCOUNT_ACCESS` (was 2,400 pre-EIP-8038).
pub(crate) const EIP8038_ACCESS_LIST_ADDRESS_COST: u32 = EIP8038_COLD_ACCOUNT_ACCESS;
/// Access-list per-storage-key base cost, `COLD_STORAGE_ACCESS` (was 1,900 pre-EIP-8038).
pub(crate) const EIP8038_ACCESS_LIST_STORAGE_KEY_COST: u32 = EIP8038_COLD_STORAGE_ACCESS;
/// Cold premium on top of `EIP8038_WARM_ACCESS` for account access.
pub(crate) const EIP8038_COLD_ACCOUNT_ACCESS_ADDITIONAL: u32 =
    EIP8038_COLD_ACCOUNT_ACCESS - EIP8038_WARM_ACCESS;
/// Cold premium on top of `EIP8038_WARM_ACCESS` for storage access.
pub(crate) const EIP8038_COLD_STORAGE_ACCESS_ADDITIONAL: u32 =
    EIP8038_COLD_STORAGE_ACCESS - EIP8038_WARM_ACCESS;
/// CALL value-transfer cost: `ACCOUNT_WRITE + CALL_STIPEND` per the EIP = 10,300.
pub(crate) const EIP8038_CALL_VALUE: u32 = EIP8038_ACCOUNT_WRITE + CALL_STIPEND;
/// Regular-gas portion of the EIP-7702 per-auth cost under EIP-8038.
///
/// Built on the EIP-8037 regular base (7,500) by applying only the EIP-8038
/// deltas of the primitives in the per-auth breakdown: `ACCOUNT_WRITE`
/// (6,700→8,000) and `COLD_ACCOUNT_ACCESS` (2,600→3,000). The two `WARM_ACCESS`
/// occurrences and the ecRecover / calldata terms are unchanged by EIP-8038, so
/// they contribute no delta. Evaluates to 9,200.
pub(crate) const EIP8038_EIP7702_PER_EMPTY_ACCOUNT_REGULAR: u32 =
    7500 + (EIP8038_ACCOUNT_WRITE - 6700) + (EIP8038_COLD_ACCOUNT_ACCESS - 2600);

// EIP-2780: Reduce intrinsic transaction gas. Replaces the legacy 21,000 base
// with a decomposed model (sender base + `tx.to`-based + `tx.value`-based).
// Active alongside EIP-8037 / EIP-8038 starting at the Amsterdam hardfork.
/// Reduced intrinsic base charged to `tx.sender` (execution-specs `TX_BASE`).
pub(crate) const EIP2780_TX_BASE_COST: u32 = 12_000;
/// Regular gas cost of the EIP-7708 transfer log emitted for every nonzero-value
/// transfer to a different account: `GAS_LOG + 3 * GAS_LOG_TOPIC + 32 *
/// GAS_LOG_DATA_PER_BYTE = 375 + 1_125 + 256 = 1_756`.
pub(crate) const EIP2780_TRANSFER_LOG_COST: u32 = 1_756;
/// Additional intrinsic regular-gas charge for a value-bearing (non-create,
/// non-self) transaction (execution-specs `TX_VALUE_COST`), on top of
/// [`EIP2780_TRANSFER_LOG_COST`].
pub(crate) const EIP2780_TX_VALUE_COST: u32 = 4_244;

/// Tracks regular, state, and refunded gas.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct GasTracker {
    remaining: u64,
    gas_limit: u64,
    reservoir: u64,
    /// Net state gas spent so far (EIP-8037).
    ///
    /// Can be negative within a call frame when 0→x→0 storage restoration
    /// refills more state gas than the frame itself charged (the parent
    /// previously charged the 0→x portion). The net is reconciled on frame
    /// return by [`Self::merge_child_gas`].
    state_gas_spent: i64,
    /// State gas drawn from regular gas (`remaining`) because the reservoir was
    /// empty (EIP-8037's `state_gas_from_gas_left`).
    ///
    /// Incremented by [`Self::spend_state`] whenever a state-gas charge spills
    /// out of the reservoir into regular gas. On frame rollback (revert or halt)
    /// the spilled portion is credited back to `remaining` in last-in-first-out
    /// order by [`Self::unwind_state_gas`]; on success it is propagated to the
    /// parent frame so a later parent rollback can return it.
    state_gas_spilled: u64,
    refunded: i64,
}

impl GasTracker {
    /// Creates a gas tracker with `limit` regular gas.
    #[inline]
    pub const fn new(limit: u64) -> Self {
        Self::from_parts(limit, limit, 0)
    }

    /// Creates a gas tracker with regular gas and a state gas reservoir.
    #[inline]
    pub const fn new_with_regular_gas_and_reservoir(limit: u64, reservoir: u64) -> Self {
        Self::from_parts(limit, limit, reservoir)
    }

    /// Creates spent gas with a state gas reservoir.
    #[inline]
    pub const fn new_spent_with_reservoir(limit: u64, reservoir: u64) -> Self {
        Self::from_parts(limit, 0, reservoir)
    }

    /// Creates a gas tracker from its raw counters.
    #[inline]
    pub const fn from_parts(gas_limit: u64, remaining: u64, reservoir: u64) -> Self {
        Self {
            remaining,
            gas_limit,
            reservoir,
            state_gas_spent: 0,
            state_gas_spilled: 0,
            refunded: 0,
        }
    }

    /// Creates a gas tracker from already used gas.
    #[inline]
    pub const fn new_used_gas(gas_limit: u64, used_gas: u64, reservoir: u64) -> Self {
        Self::from_parts(gas_limit, gas_limit - used_gas, reservoir)
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

    /// Returns spent state gas. May be negative within a frame (see field docs).
    #[inline]
    pub const fn state_gas_spent(&self) -> i64 {
        self.state_gas_spent
    }

    /// Adds `delta` to spent state gas, saturating. May leave the total negative (see field docs).
    #[inline]
    pub const fn add_state_gas_spent(&mut self, delta: i64) {
        self.state_gas_spent = self.state_gas_spent.saturating_add(delta);
    }

    /// Returns state gas drawn from regular gas because the reservoir was empty
    /// (EIP-8037's `state_gas_from_gas_left`). See the field docs.
    #[inline]
    pub const fn state_gas_spilled(&self) -> u64 {
        self.state_gas_spilled
    }

    /// Adds `delta` to the spilled state gas, saturating.
    ///
    /// Used to merge a successful child frame's spilled state gas into this
    /// frame, since it is now backed by the merged regular gas.
    #[inline]
    pub const fn add_state_gas_spilled(&mut self, delta: u64) {
        self.state_gas_spilled = self.state_gas_spilled.saturating_add(delta);
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
            self.state_gas_spent = self.state_gas_spent.saturating_add(cost as i64);
            self.reservoir -= cost;
            return Ok(());
        }

        let spill = cost - self.reservoir;

        self.spend(spill)?;
        self.state_gas_spent = self.state_gas_spent.saturating_add(cost as i64);
        self.state_gas_spilled = self.state_gas_spilled.saturating_add(spill);
        self.reservoir = 0;
        Ok(())
    }

    /// Rolls back this frame's state-gas charges on revert or exceptional halt
    /// (EIP-8037).
    ///
    /// The state gas charged within the frame is refilled in last-in-first-out
    /// order: the spilled portion is credited back to `remaining` (the pool
    /// charged last) and the rest restores the reservoir to its frame-start
    /// value. Concretely, `remaining` gains `state_gas_spilled` and the reservoir
    /// becomes `reservoir + state_gas_spent - state_gas_spilled`, which is exactly
    /// the reservoir the frame inherited. Both state-gas counters are then reset.
    ///
    /// On revert the resulting `remaining` (including the refilled spill) is
    /// returned to the parent; on halt the caller additionally zeroes `remaining`
    /// so the spilled gas is consumed while the reservoir is still left untouched.
    #[inline]
    pub const fn unwind_state_gas(&mut self) {
        self.reservoir = self
            .reservoir
            .saturating_add_signed(self.state_gas_spent)
            .saturating_sub(self.state_gas_spilled);
        self.remaining = self.remaining.saturating_add(self.state_gas_spilled);
        self.state_gas_spent = 0;
        self.state_gas_spilled = 0;
    }

    /// Refills `amount` of state gas undone during execution, in last-in-first-out
    /// order (EIP-8037).
    ///
    /// When a state creation is undone within the same transaction — a storage
    /// slot restored to its original zero value (0→x→0), or a failed CREATE's
    /// upfront charge — the corresponding state gas is restored directly rather
    /// than routed through the capped refund counter. Because charges deduct from
    /// the reservoir first and from regular gas (`remaining`) last, the refill
    /// credits the pool charged last first: `remaining` is credited up to
    /// `state_gas_spilled` (decrementing it by the same amount) and any remainder
    /// tops up the reservoir.
    ///
    /// `state_gas_spent` is decremented by the full `amount` and may become
    /// negative when the matching charge was made by a parent frame (so this
    /// frame's `state_gas_spilled` is zero and the whole refill lands in the
    /// reservoir); the parent's total is reconciled on frame return.
    #[inline]
    pub const fn refill_reservoir(&mut self, amount: u64) {
        let to_remaining =
            if amount < self.state_gas_spilled { amount } else { self.state_gas_spilled };
        self.remaining = self.remaining.saturating_add(to_remaining);
        self.state_gas_spilled -= to_remaining;
        self.reservoir = self.reservoir.saturating_add(amount - to_remaining);
        self.state_gas_spent = self.state_gas_spent.saturating_sub(amount as i64);
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

    /// Settles this returning frame's own gas for its `stop` reason.
    ///
    /// A failing frame rolls back its state changes, so its state gas is refilled
    /// in LIFO order ([`Self::unwind_state_gas`]) and the execution refund counter
    /// is dropped; an exceptional halt additionally consumes the frame's regular
    /// gas. On success the gas is left as-is. Applied once when a frame returns, so
    /// every consumer — the parent [`Self::merge_child_gas`], top-level accounting,
    /// and inspectors — reads the same settled gas.
    #[inline]
    pub const fn settle_gas(&mut self, stop: InstrStop) {
        if !stop.is_success() {
            self.unwind_state_gas();
            self.set_refunded(0);
        }
        if stop.is_halt() {
            self.set_remaining(0);
        }
    }

    /// Merges a returning child frame's (already [settled](Self::settle_gas)) gas
    /// into this (parent) frame, per the child's `stop` reason.
    ///
    /// - **Unused regular gas** returns to the parent only on success or revert; a
    ///   halt consumes the child's regular gas (already zeroed when settled).
    /// - **The reservoir** is a shared state-gas pool the child inherited at call
    ///   time, so the parent always adopts the child's value — which settling
    ///   restored to the inherited amount on revert/halt, leaving it untouched.
    /// - **Net state gas, its spilled portion, and the refund counter** persist
    ///   only on success; on revert/halt the child's state changes roll back, so
    ///   it contributes none.
    #[inline]
    pub const fn merge_child_gas(&mut self, child: Self, stop: InstrStop) {
        if stop.is_success() || stop.is_revert() {
            self.erase_cost(child.remaining);
        }
        self.set_reservoir(child.reservoir);
        if stop.is_success() {
            self.add_state_gas_spent(child.state_gas_spent);
            self.add_state_gas_spilled(child.state_gas_spilled);
            self.record_refund(child.refunded);
        }
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
        Self { tracker: GasTracker::new(limit), memory: MemoryGas::new() }
    }

    /// Creates gas with regular gas and a state gas reservoir.
    #[inline]
    pub const fn new_with_regular_gas_and_reservoir(limit: u64, reservoir: u64) -> Self {
        Self {
            tracker: GasTracker::new_with_regular_gas_and_reservoir(limit, reservoir),
            memory: MemoryGas::new(),
        }
    }

    /// Creates spent gas with a state gas reservoir.
    #[inline]
    pub const fn new_spent_with_reservoir(limit: u64, reservoir: u64) -> Self {
        Self {
            tracker: GasTracker::new_spent_with_reservoir(limit, reservoir),
            memory: MemoryGas::new(),
        }
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

    /// Returns spent state gas. May be negative within a frame (see field docs).
    #[inline]
    pub const fn state_gas_spent(&self) -> i64 {
        self.tracker.state_gas_spent()
    }

    /// Adds `delta` to spent state gas, saturating. May leave the total negative (see field docs).
    #[inline]
    pub const fn add_state_gas_spent(&mut self, delta: i64) {
        self.tracker.add_state_gas_spent(delta);
    }

    /// Returns state gas drawn from regular gas because the reservoir was empty
    /// (EIP-8037). See [`GasTracker::state_gas_spilled`].
    #[inline]
    pub const fn state_gas_spilled(&self) -> u64 {
        self.tracker.state_gas_spilled()
    }

    /// Adds `delta` to the spilled state gas, saturating.
    /// See [`GasTracker::add_state_gas_spilled`].
    #[inline]
    pub const fn add_state_gas_spilled(&mut self, delta: u64) {
        self.tracker.add_state_gas_spilled(delta);
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

    /// Refills the reservoir with state gas returned by 0→x→0 storage
    /// restoration (EIP-8037). See [`GasTracker::refill_reservoir`].
    #[inline]
    pub const fn refill_reservoir(&mut self, amount: u64) {
        self.tracker.refill_reservoir(amount);
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

    /// Merges a returning child frame's gas into this frame.
    /// See [`GasTracker::merge_child_gas`].
    #[inline]
    pub const fn merge_child_gas(&mut self, child: GasTracker, stop: InstrStop) {
        self.tracker.merge_child_gas(child, stop);
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
    use core::assert_matches;

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
        assert_matches!(gas.spend_state(200), Err(InstrStop::OutOfGas));

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
        assert_matches!(gas.spend_state(100), Err(InstrStop::OutOfGas));
        assert_eq!(gas.state_gas_spent(), 0);

        let mut gas = Gas::new_with_regular_gas_and_reservoir(30, 20);
        assert_matches!(gas.spend_state(100), Err(InstrStop::OutOfGas));
        assert_eq!(gas.state_gas_spent(), 0);
        assert_eq!(gas.reservoir(), 20);
    }

    #[test]
    fn test_spend_state_tracks_spilled() {
        // No spill while the reservoir covers the charge.
        let mut gas = Gas::new_with_regular_gas_and_reservoir(1000, 500);
        assert!(gas.spend_state(300).is_ok());
        assert_eq!((gas.state_gas_spilled(), gas.reservoir()), (0, 200));

        // Spilling the remainder into regular gas records it.
        assert!(gas.spend_state(500).is_ok());
        assert_eq!((gas.state_gas_spilled(), gas.reservoir(), gas.remaining()), (300, 0, 700));

        // Further charges spill in full once the reservoir is empty.
        assert!(gas.spend_state(100).is_ok());
        assert_eq!((gas.state_gas_spilled(), gas.remaining()), (400, 600));
    }

    #[test]
    fn test_unwind_state_gas_no_spill() {
        // Pure-reservoir spend: unwind restores the reservoir, leaving regular gas alone.
        let mut gas = Gas::new_with_regular_gas_and_reservoir(1000, 500);
        assert!(gas.spend_state(300).is_ok());
        gas.tracker_mut().unwind_state_gas();
        assert_eq!(
            (gas.reservoir(), gas.remaining(), gas.state_gas_spent(), gas.state_gas_spilled()),
            (500, 1000, 0, 0)
        );
    }

    #[test]
    fn test_unwind_state_gas_with_spill() {
        // Reservoir exhausted then spilled: unwind returns the spill to regular gas
        // and restores the reservoir to its frame-start value.
        let mut gas = Gas::new_with_regular_gas_and_reservoir(1000, 200);
        assert!(gas.spend_state(500).is_ok());
        assert_eq!((gas.reservoir(), gas.remaining(), gas.state_gas_spilled()), (0, 700, 300));
        gas.tracker_mut().unwind_state_gas();
        assert_eq!(
            (gas.reservoir(), gas.remaining(), gas.state_gas_spent(), gas.state_gas_spilled()),
            (200, 1000, 0, 0)
        );
    }

    #[test]
    fn test_refill_reservoir_lifo() {
        // Refill credits the spilled (regular-gas) portion first, remainder to reservoir.
        let mut gas = Gas::new_with_regular_gas_and_reservoir(1000, 200);
        assert!(gas.spend_state(500).is_ok()); // spill 300, reservoir 0, remaining 700
        assert_eq!((gas.reservoir(), gas.remaining(), gas.state_gas_spilled()), (0, 700, 300));
        // Refund 200: less than the 300 spilled, so it all returns to regular gas.
        gas.refill_reservoir(200);
        assert_eq!(
            (gas.reservoir(), gas.remaining(), gas.state_gas_spent(), gas.state_gas_spilled()),
            (0, 900, 300, 100)
        );
        // Refund 250: 100 returns to regular gas (draining the spill), 150 to the reservoir.
        gas.refill_reservoir(250);
        assert_eq!(
            (gas.reservoir(), gas.remaining(), gas.state_gas_spent(), gas.state_gas_spilled()),
            (150, 1000, 50, 0)
        );

        // With no spill recorded, the refill lands entirely in the reservoir and may
        // drive `state_gas_spent` negative (charge made by a parent frame).
        let mut gas = Gas::new_with_regular_gas_and_reservoir(1000, 100);
        gas.refill_reservoir(300);
        assert_eq!(
            (gas.reservoir(), gas.remaining(), gas.state_gas_spent(), gas.state_gas_spilled()),
            (400, 1000, -300, 0)
        );
    }

    #[test]
    fn test_unwind_state_gas_after_refill() {
        // A partial refill returns part of the spill to regular gas; unwind still
        // restores the original split.
        let mut gas = Gas::new_with_regular_gas_and_reservoir(1000, 200);
        assert!(gas.spend_state(500).is_ok()); // spill 300, reservoir 0, remaining 700
        gas.refill_reservoir(100); // LIFO: remaining 800, spilled 200, reservoir 0
        assert_eq!((gas.reservoir(), gas.remaining(), gas.state_gas_spilled()), (0, 800, 200));
        gas.tracker_mut().unwind_state_gas();
        assert_eq!(
            (gas.reservoir(), gas.remaining(), gas.state_gas_spent(), gas.state_gas_spilled()),
            (200, 1000, 0, 0)
        );
    }

    #[test]
    fn test_spend_state_zero_remaining_with_reservoir() {
        let mut gas = Gas::new_with_regular_gas_and_reservoir(0, 500);
        assert!(gas.spend_state(200).is_ok());
        assert_eq!((gas.reservoir(), gas.remaining(), gas.state_gas_spent()), (300, 0, 200));

        assert!(gas.spend_state(300).is_ok());
        assert_eq!((gas.reservoir(), gas.remaining(), gas.state_gas_spent()), (0, 0, 500));

        assert_matches!(gas.spend_state(1), Err(InstrStop::OutOfGas));
    }
}
