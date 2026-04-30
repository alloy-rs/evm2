//! EVM gas calculation utilities.

use super::{InstrErr, Result, SpecId};
use alloy_primitives::U256;
use core::hint::cold_path;

const COPY: u64 = 3;
const MEMORY: u64 = 3;
const KECCAK256WORD: u64 = 6;
const LOGDATA: u64 = 8;
const LOGTOPIC: u64 = 375;
const CREATE: u64 = 32000;
const CALLVALUE: u64 = 9000;
const NEWACCOUNT: u64 = 25000;
const SSTORE_SET: u64 = 20000;
const SSTORE_RESET: u64 = 5000;
const REFUND_SSTORE_CLEARS: u64 = 15000;
const SELFDESTRUCT_REFUND: u64 = 24000;
const CODEDEPOSIT: u64 = 200;
const STANDARD_TOKEN_COST: u64 = 4;
const NON_ZERO_BYTE_MULTIPLIER: u64 = 17;
const INITCODE_WORD_COST: u64 = 2;
const CALL_STIPEND: u64 = 2300;

macro_rules! gas_ids {
    ($($value:literal => $variant:ident => $name:literal => $doc:literal;)*) => {
        /// Gas parameter identifier.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        #[non_exhaustive]
        #[repr(u8)]
        pub enum GasId {
            $(
                #[doc = $doc]
                $variant = $value,
            )*
        }

        impl GasId {
            /// Returns the raw gas parameter identifier.
            #[inline]
            pub const fn as_u8(self) -> u8 {
                self as u8
            }

            /// Returns the gas parameter identifier as a table index.
            #[inline]
            pub const fn as_usize(self) -> usize {
                self.as_u8() as usize
            }

            /// Returns the revm gas parameter name.
            #[inline]
            pub const fn name(self) -> &'static str {
                match self {
                    $(
                        Self::$variant => $name,
                    )*
                }
            }

            /// Returns the gas parameter for a raw identifier.
            #[inline]
            pub const fn from_u8(value: u8) -> Option<Self> {
                match value {
                    $(
                        $value => Some(Self::$variant),
                    )*
                    _ => None,
                }
            }

            /// Returns the gas parameter for a revm gas parameter name.
            #[inline]
            pub fn from_name(name: &str) -> Option<Self> {
                match name {
                    $(
                        $name => Some(Self::$variant),
                    )*
                    _ => None,
                }
            }
        }
    };
}

gas_ids! {
    1 => ExpByteGas => "exp_byte_gas" => "Gas charged per non-zero byte in `EXP` exponent.";
    2 => ExtcodecopyPerWord => "extcodecopy_per_word" => "Gas charged per copied word in `EXTCODECOPY`.";
    3 => CopyPerWord => "copy_per_word" => "Gas charged per copied word.";
    4 => Logdata => "logdata" => "Gas charged per byte of log data.";
    5 => Logtopic => "logtopic" => "Gas charged per log topic.";
    6 => McopyPerWord => "mcopy_per_word" => "Gas charged per copied word in `MCOPY`.";
    7 => Keccak256PerWord => "keccak256_per_word" => "Gas charged per hashed word in `KECCAK256`.";
    8 => MemoryLinearCost => "memory_linear_cost" => "Linear memory gas coefficient.";
    9 => MemoryQuadraticReduction => "memory_quadratic_reduction" => "Quadratic memory gas divisor.";
    10 => InitcodePerWord => "initcode_per_word" => "Gas charged per initcode word.";
    11 => Create => "create" => "Gas charged by `CREATE`.";
    12 => CallStipendReduction => "call_stipend_reduction" => "Call gas stipend reduction divisor.";
    13 => TransferValueCost => "transfer_value_cost" => "Gas charged when a call transfers value.";
    14 => ColdAccountAdditionalCost => "cold_account_additional_cost" => "Additional gas charged for a cold account access.";
    15 => NewAccountCost => "new_account_cost" => "Gas charged for creating a new account.";
    16 => WarmStorageReadCost => "warm_storage_read_cost" => "Gas charged for a warm storage read.";
    17 => SstoreStatic => "sstore_static" => "Static `SSTORE` gas.";
    18 => SstoreSetWithoutLoadCost => "sstore_set_without_load_cost" => "Gas charged by `SSTORE` for setting a slot, excluding the load.";
    19 => SstoreResetWithoutColdLoadCost => "sstore_reset_without_cold_load_cost" => "Gas charged by `SSTORE` for resetting a slot, excluding a cold load.";
    20 => SstoreClearingSlotRefund => "sstore_clearing_slot_refund" => "Refund for clearing a storage slot.";
    21 => SelfdestructRefund => "selfdestruct_refund" => "`SELFDESTRUCT` refund.";
    22 => CallStipend => "call_stipend" => "Gas stipend for a value-transferring call.";
    23 => ColdStorageAdditionalCost => "cold_storage_additional_cost" => "Additional gas charged for cold storage.";
    24 => ColdStorageCost => "cold_storage_cost" => "Gas charged for cold storage.";
    25 => NewAccountCostForSelfdestruct => "new_account_cost_for_selfdestruct" => "New account cost charged by `SELFDESTRUCT`.";
    26 => CodeDepositCost => "code_deposit_cost" => "Gas charged per deposited code byte.";
    27 => TxEip7702PerEmptyAccountCost => "tx_eip7702_per_empty_account_cost" => "EIP-7702 transaction cost per empty account.";
    28 => TxTokenNonZeroByteMultiplier => "tx_token_non_zero_byte_multiplier" => "Transaction token multiplier for non-zero bytes.";
    29 => TxTokenCost => "tx_token_cost" => "Transaction token base cost.";
    30 => TxFloorCostPerToken => "tx_floor_cost_per_token" => "Transaction floor cost per token.";
    31 => TxFloorCostBaseGas => "tx_floor_cost_base_gas" => "Transaction floor base gas.";
    32 => TxAccessListAddressCost => "tx_access_list_address_cost" => "Transaction access-list address cost.";
    33 => TxAccessListStorageKeyCost => "tx_access_list_storage_key_cost" => "Transaction access-list storage-key cost.";
    34 => TxBaseStipend => "tx_base_stipend" => "Transaction base stipend.";
    35 => TxCreateCost => "tx_create_cost" => "Transaction create cost.";
    36 => TxInitcodeCost => "tx_initcode_cost" => "Transaction initcode cost.";
    37 => SstoreSetRefund => "sstore_set_refund" => "`SSTORE` set refund.";
    38 => SstoreResetRefund => "sstore_reset_refund" => "`SSTORE` reset refund.";
    39 => TxEip7702AuthRefund => "tx_eip7702_auth_refund" => "EIP-7702 transaction authorization refund.";
    40 => SstoreSetStateGas => "sstore_set_state_gas" => "`SSTORE` set state gas.";
    41 => NewAccountStateGas => "new_account_state_gas" => "New account state gas.";
    42 => CodeDepositStateGas => "code_deposit_state_gas" => "Code deposit state gas.";
    43 => CreateStateGas => "create_state_gas" => "`CREATE` state gas.";
    44 => TxEip7702PerAuthStateGas => "tx_eip7702_per_auth_state_gas" => "EIP-7702 transaction state gas per authorization.";
}

/// Gas parameter table.
pub type GasParamTable = [u64; 256];

/// Returns the number of EVM words needed for `len` bytes.
#[inline]
pub const fn num_words(len: usize) -> usize {
    len.div_ceil(32)
}

/// Dynamic gas parameter table.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GasParams {
    table: GasParamTable,
}

impl Default for GasParams {
    #[inline]
    fn default() -> Self {
        Self::new_spec(SpecId::Frontier)
    }
}

impl GasParams {
    /// Creates gas parameters from a raw table.
    #[inline]
    pub const fn new(table: GasParamTable) -> Self {
        Self { table }
    }

    /// Creates gas parameters for `spec`.
    #[inline]
    pub fn new_spec(spec: SpecId) -> Self {
        Self::new_spec_inner(spec)
    }

    #[inline]
    fn new_spec_inner(spec: SpecId) -> Self {
        let mut table = [0; 256];

        set_gas_param(&mut table, GasId::ExpByteGas, 10);
        set_gas_param(&mut table, GasId::Logdata, LOGDATA);
        set_gas_param(&mut table, GasId::Logtopic, LOGTOPIC);
        set_gas_param(&mut table, GasId::CopyPerWord, COPY);
        set_gas_param(&mut table, GasId::ExtcodecopyPerWord, COPY);
        set_gas_param(&mut table, GasId::McopyPerWord, COPY);
        set_gas_param(&mut table, GasId::Keccak256PerWord, KECCAK256WORD);
        set_gas_param(&mut table, GasId::MemoryLinearCost, MEMORY);
        set_gas_param(&mut table, GasId::MemoryQuadraticReduction, 512);
        set_gas_param(&mut table, GasId::InitcodePerWord, INITCODE_WORD_COST);
        set_gas_param(&mut table, GasId::Create, CREATE);
        set_gas_param(&mut table, GasId::CallStipendReduction, 64);
        set_gas_param(&mut table, GasId::TransferValueCost, CALLVALUE);
        set_gas_param(&mut table, GasId::ColdAccountAdditionalCost, 0);
        set_gas_param(&mut table, GasId::NewAccountCost, NEWACCOUNT);
        set_gas_param(&mut table, GasId::WarmStorageReadCost, 0);
        set_gas_param(&mut table, GasId::SstoreStatic, SSTORE_RESET);
        set_gas_param(&mut table, GasId::SstoreSetWithoutLoadCost, SSTORE_SET - SSTORE_RESET);
        set_gas_param(&mut table, GasId::SstoreResetWithoutColdLoadCost, 0);
        set_gas_param(&mut table, GasId::SstoreSetRefund, SSTORE_SET - SSTORE_RESET);
        set_gas_param(&mut table, GasId::SstoreResetRefund, 0);
        set_gas_param(&mut table, GasId::SstoreClearingSlotRefund, REFUND_SSTORE_CLEARS);
        set_gas_param(&mut table, GasId::SelfdestructRefund, SELFDESTRUCT_REFUND);
        set_gas_param(&mut table, GasId::CallStipend, CALL_STIPEND);
        set_gas_param(&mut table, GasId::ColdStorageAdditionalCost, 0);
        set_gas_param(&mut table, GasId::ColdStorageCost, 0);
        set_gas_param(&mut table, GasId::NewAccountCostForSelfdestruct, 0);
        set_gas_param(&mut table, GasId::CodeDepositCost, CODEDEPOSIT);
        set_gas_param(&mut table, GasId::TxTokenNonZeroByteMultiplier, NON_ZERO_BYTE_MULTIPLIER);
        set_gas_param(&mut table, GasId::TxTokenCost, STANDARD_TOKEN_COST);
        set_gas_param(&mut table, GasId::TxBaseStipend, 21000);

        if spec >= SpecId::Homestead {
            set_gas_param(&mut table, GasId::TxCreateCost, CREATE);
        }

        Self::new(table)
    }

    /// Overrides gas costs by gas identifier.
    #[inline]
    pub fn override_gas(&mut self, values: impl IntoIterator<Item = (GasId, u64)>) {
        for (id, value) in values {
            self.table[id.as_usize()] = value;
        }
    }

    /// Returns the raw gas parameter table.
    #[inline]
    pub const fn table(&self) -> &GasParamTable {
        &self.table
    }

    /// Returns the gas cost for `id`.
    #[inline]
    pub const fn get(&self, id: GasId) -> u64 {
        self.table[id.as_usize()]
    }

    /// Calculates memory expansion cost for `len` words.
    #[inline]
    pub const fn memory_cost(&self, len: usize) -> u64 {
        let len = len as u64;
        self.get(GasId::MemoryLinearCost)
            .saturating_mul(len)
            .saturating_add(len.saturating_mul(len) / self.get(GasId::MemoryQuadraticReduction))
    }

    /// Calculates dynamic `EXP` gas.
    #[inline]
    pub fn exp_cost(&self, power: U256) -> u64 {
        if power.is_zero() {
            return 0;
        }
        self.get(GasId::ExpByteGas).saturating_mul(power.bit_len().div_ceil(8) as u64)
    }

    /// Calculates copy gas for `len` bytes.
    #[inline]
    pub const fn copy_cost(&self, len: usize) -> u64 {
        self.get(GasId::CopyPerWord).saturating_mul(num_words(len) as u64)
    }

    /// Calculates `EXTCODECOPY` copy gas for `len` bytes.
    #[inline]
    pub const fn extcodecopy_cost(&self, len: usize) -> u64 {
        self.get(GasId::ExtcodecopyPerWord).saturating_mul(num_words(len) as u64)
    }

    /// Calculates `MCOPY` copy gas for `len` bytes.
    #[inline]
    pub const fn mcopy_cost(&self, len: usize) -> u64 {
        self.get(GasId::McopyPerWord).saturating_mul(num_words(len) as u64)
    }

    /// Calculates `KECCAK256` word gas for `len` bytes.
    #[inline]
    pub const fn keccak256_word_cost(&self, len: usize) -> u64 {
        self.get(GasId::Keccak256PerWord).saturating_mul(num_words(len) as u64)
    }
}

#[inline]
fn set_gas_param(table: &mut GasParamTable, id: GasId, value: u64) {
    table[id.as_usize()] = value;
}

/// Tracks regular, state, and refunded gas.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct GasTracker {
    gas_limit: u64,
    remaining: u64,
    reservoir: u64,
    state_gas_spent: u64,
    refunded: i64,
}

impl GasTracker {
    /// Creates a gas tracker from its raw counters.
    #[inline]
    pub const fn new(gas_limit: u64, remaining: u64, reservoir: u64) -> Self {
        Self { gas_limit, remaining, reservoir, state_gas_spent: 0, refunded: 0 }
    }

    /// Creates a gas tracker from already used gas.
    #[inline]
    pub const fn new_used_gas(gas_limit: u64, used_gas: u64, reservoir: u64) -> Self {
        Self::new(gas_limit, gas_limit - used_gas, reservoir)
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

    /// Records regular gas cost.
    #[inline]
    #[must_use = "In case of not enough gas, the interpreter should halt with an out-of-gas error"]
    pub const fn record_regular_cost(&mut self, cost: u64) -> bool {
        if let Some(new_remaining) = self.remaining.checked_sub(cost) {
            self.remaining = new_remaining;
            return true;
        }
        false
    }

    /// Records state gas cost.
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

/// Interpreter gas state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Gas {
    tracker: GasTracker,
    memory: MemoryGas,
}

/// Mutable gas reference.
pub type GasRef<'a> = &'a mut Gas;

impl Gas {
    /// Creates gas with `limit` regular gas.
    #[inline]
    pub const fn new(limit: u64) -> Self {
        Self { tracker: GasTracker::new(limit, limit, 0), memory: MemoryGas::new() }
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

    /// Returns the gas limit.
    #[inline]
    pub const fn limit(&self) -> u64 {
        self.tracker.limit()
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

    /// Returns gas refund.
    #[inline]
    pub const fn refunded(&self) -> i64 {
        self.tracker.refunded()
    }

    /// Returns spent regular gas.
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

    /// Returns total gas spent.
    #[inline]
    pub const fn total_gas_spent(&self) -> u64 {
        self.tracker.limit().saturating_sub(self.tracker.remaining())
    }

    /// Returns used gas after refund.
    #[inline]
    pub const fn used(&self) -> u64 {
        self.total_gas_spent().saturating_sub(self.refunded() as u64)
    }

    /// Returns spent gas after refund.
    #[inline]
    pub const fn spent_sub_refunded(&self) -> u64 {
        self.total_gas_spent().saturating_sub(self.tracker.refunded() as u64)
    }

    /// Returns remaining regular gas.
    #[inline]
    pub const fn remaining(&self) -> u64 {
        self.tracker.remaining()
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

    /// Adds gas refund.
    #[inline]
    pub const fn record_refund(&mut self, refund: i64) {
        self.tracker.record_refund(refund);
    }

    /// Applies the final refund cap.
    #[inline]
    pub fn set_final_refund(&mut self, is_london: bool) {
        let max_refund_quotient = if is_london { 5 } else { 2 };
        let gas_used = self.total_gas_spent().saturating_sub(self.reservoir());
        self.tracker
            .set_refunded((self.refunded() as u64).min(gas_used / max_refund_quotient) as i64);
    }

    /// Sets gas refund.
    #[inline]
    pub const fn set_refund(&mut self, refund: i64) {
        self.tracker.set_refunded(refund);
    }

    /// Sets remaining regular gas.
    #[inline]
    pub const fn set_remaining(&mut self, remaining: u64) {
        self.tracker.set_remaining(remaining);
    }

    /// Sets spent regular gas.
    #[inline]
    pub const fn set_spent(&mut self, spent: u64) {
        self.tracker.set_remaining(self.tracker.limit().saturating_sub(spent));
    }

    /// Records regular gas cost.
    #[inline]
    #[must_use = "prefer using `gas!` instead to return an out-of-gas error on failure"]
    #[deprecated(since = "32.0.0", note = "use record_regular_cost instead")]
    pub const fn record_cost(&mut self, cost: u64) -> bool {
        self.record_regular_cost(cost)
    }

    /// Records cost with wrapping subtraction.
    #[inline(always)]
    #[must_use = "In case of not enough gas, the interpreter should halt with an out-of-gas error"]
    pub const fn record_cost_unsafe(&mut self, cost: u64) -> bool {
        let remaining = self.tracker.remaining();
        let oog = remaining < cost;
        self.tracker.set_remaining(remaining.wrapping_sub(cost));
        oog
    }

    /// Records state gas cost.
    #[inline]
    #[must_use = "In case of not enough gas, the interpreter should halt with an out-of-gas error"]
    pub const fn record_state_cost(&mut self, cost: u64) -> bool {
        self.tracker.record_state_cost(cost)
    }

    /// Records regular gas cost.
    #[inline]
    #[must_use = "In case of not enough gas, the interpreter should halt with an out-of-gas error"]
    pub const fn record_regular_cost(&mut self, cost: u64) -> bool {
        self.tracker.record_regular_cost(cost)
    }

    /// Spends regular gas or returns out of gas.
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

/// Memory expansion result.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub enum MemoryExtensionResult {
    /// Memory was extended.
    Extended,
    /// Memory size did not change.
    Same,
    /// Memory expansion ran out of gas.
    OutOfGas,
}

/// Memory gas accounting state.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq, Hash)]
pub struct MemoryGas {
    /// Current memory size in EVM words.
    pub words_num: usize,
    /// Current total expansion cost.
    pub expansion_cost: u64,
}

impl MemoryGas {
    /// Creates empty memory gas state.
    #[inline]
    pub const fn new() -> Self {
        Self { words_num: 0, expansion_cost: 0 }
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
    fn gas_id_roundtrips_names_and_values() {
        assert_eq!(GasId::from_u8(1), Some(GasId::ExpByteGas));
        assert_eq!(GasId::ExpByteGas.as_u8(), 1);
        assert_eq!(GasId::ExpByteGas.name(), "exp_byte_gas");
        assert_eq!(GasId::from_name("exp_byte_gas"), Some(GasId::ExpByteGas));
        assert_eq!(GasId::from_u8(0), None);
        assert_eq!(GasId::from_name("missing"), None);
    }

    #[test]
    fn gas_params_match_frontier_defaults() {
        let params = GasParams::new_spec(SpecId::Frontier);
        assert_eq!(params.get(GasId::ExpByteGas), 10);
        assert_eq!(params.get(GasId::MemoryLinearCost), 3);
        assert_eq!(params.get(GasId::MemoryQuadraticReduction), 512);
        assert_eq!(params.get(GasId::SstoreStatic), 5000);
        assert_eq!(params.get(GasId::SstoreSetWithoutLoadCost), 15000);
        assert_eq!(params.get(GasId::TxCreateCost), 0);
    }

    #[test]
    fn gas_params_apply_homestead_defaults() {
        let params = GasParams::new_spec(SpecId::Homestead);
        assert_eq!(params.get(GasId::TxCreateCost), 32000);
    }

    #[test]
    fn gas_params_override_values() {
        let mut params = GasParams::default();
        params
            .override_gas([(GasId::MemoryLinearCost, 7), (GasId::MemoryQuadraticReduction, 1024)]);
        assert_eq!(params.get(GasId::MemoryLinearCost), 7);
        assert_eq!(params.get(GasId::MemoryQuadraticReduction), 1024);
    }

    #[test]
    fn gas_params_calculate_costs() {
        let params = GasParams::default();
        assert_eq!(num_words(0), 0);
        assert_eq!(num_words(33), 2);
        assert_eq!(params.memory_cost(10), 30);
        assert_eq!(params.copy_cost(33), 6);
        assert_eq!(params.extcodecopy_cost(33), 6);
        assert_eq!(params.mcopy_cost(33), 6);
        assert_eq!(params.keccak256_word_cost(33), 12);
        assert_eq!(params.exp_cost(U256::ZERO), 0);
        assert_eq!(params.exp_cost(U256::from(0xff_u64)), 10);
        assert_eq!(params.exp_cost(U256::from(0x100_u64)), 20);
    }

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
