use crate::{BaseEvmTypes, EvmVersion, interpreter::SpecId};
use alloy_primitives::U256;
use core::ops::{Index, IndexMut};
use paste::paste;

macro_rules! gas_ids {
    ($($tokens:tt)*) => {
        gas_ids_find_last! { [] $($tokens)* }
    };
}

macro_rules! gas_ids_find_last {
    ([$($variants:tt)*] #[$last_doc:meta] $last_variant:ident;) => {
        gas_ids_impl! { [$($variants)*] #[$last_doc] $last_variant; }
    };
    ([$($variants:tt)*] #[$doc:meta] $variant:ident; $($rest:tt)+) => {
        gas_ids_find_last! { [$($variants)* #[$doc] $variant;] $($rest)+ }
    };
}

macro_rules! gas_ids_impl {
    (
        [#[$first_doc:meta] $first_variant:ident; $(#[$doc:meta] $variant:ident;)*]
        #[$last_doc:meta] $last_variant:ident;
    ) => {
        paste! {
            /// Gas parameter identifier.
            #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
            #[non_exhaustive]
            #[repr(u8)]
            pub enum GasId {
                #[$first_doc]
                $first_variant = 1,
                $(
                    #[$doc]
                    $variant,
                )*
                #[$last_doc]
                $last_variant,
            }

            impl GasId {
                /// Largest gas parameter identifier.
                pub const MAX: u8 = Self::$last_variant as u8;

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
                        Self::$first_variant => stringify!([<$first_variant:snake>]),
                        $(
                            Self::$variant => stringify!([<$variant:snake>]),
                        )*
                        Self::$last_variant => stringify!([<$last_variant:snake>]),
                    }
                }

                /// Returns the gas parameter for a raw identifier.
                #[inline]
                pub const fn from_u8(value: u8) -> Option<Self> {
                    if value >= 1 && value <= Self::MAX {
                        // SAFETY: `GasId` is `repr(u8)`, starts at 1, and every variant up to
                        // `MAX` is assigned contiguously by the enum declaration.
                        return Some(unsafe { core::mem::transmute::<u8, Self>(value) });
                    }
                    None
                }

                /// Returns the gas parameter for a revm gas parameter name.
                #[inline]
                pub fn from_name(name: &str) -> Option<Self> {
                    match name {
                        stringify!([<$first_variant:snake>]) => Some(Self::$first_variant),
                        $(
                            stringify!([<$variant:snake>]) => Some(Self::$variant),
                        )*
                        stringify!([<$last_variant:snake>]) => Some(Self::$last_variant),
                        _ => None,
                    }
                }
            }
        }
    };
}

gas_ids! {
    /// Gas charged per non-zero byte in `EXP` exponent.
    ExpByteGas;
    /// Gas charged per copied word in `EXTCODECOPY`.
    ExtcodecopyPerWord;
    /// Gas charged per copied word.
    CopyPerWord;
    /// Gas charged per byte of log data.
    Logdata;
    /// Gas charged per log topic.
    Logtopic;
    /// Gas charged per copied word in `MCOPY`.
    McopyPerWord;
    /// Gas charged per hashed word in `KECCAK256`.
    Keccak256PerWord;
    /// Linear memory gas coefficient.
    MemoryLinearCost;
    /// Quadratic memory gas divisor.
    MemoryQuadraticReduction;
    /// Gas charged per initcode word.
    InitcodePerWord;
    /// Gas charged by `CREATE`.
    Create;
    /// Call gas stipend reduction divisor.
    CallStipendReduction;
    /// Gas charged when a call transfers value.
    TransferValueCost;
    /// Additional gas charged for a cold account access.
    ColdAccountAdditionalCost;
    /// Gas charged for creating a new account.
    NewAccountCost;
    /// Gas charged for a warm storage read.
    WarmStorageReadCost;
    /// Static `SSTORE` gas.
    SstoreStatic;
    /// Gas charged by `SSTORE` for setting a slot, excluding the load.
    SstoreSetWithoutLoadCost;
    /// Gas charged by `SSTORE` for resetting a slot, excluding a cold load.
    SstoreResetWithoutColdLoadCost;
    /// Refund for clearing a storage slot.
    SstoreClearingSlotRefund;
    /// `SELFDESTRUCT` refund.
    SelfdestructRefund;
    /// Gas stipend for a value-transferring call.
    CallStipend;
    /// Additional gas charged for cold storage.
    ColdStorageAdditionalCost;
    /// Gas charged for cold storage.
    ColdStorageCost;
    /// New account cost charged by `SELFDESTRUCT`.
    NewAccountCostForSelfdestruct;
    /// Gas charged per deposited code byte.
    CodeDepositCost;
    /// EIP-7702 transaction cost per empty account.
    TxEip7702PerEmptyAccountCost;
    /// Transaction token multiplier for non-zero bytes.
    TxTokenNonZeroByteMultiplier;
    /// Transaction token base cost.
    TxTokenCost;
    /// Transaction floor cost per token.
    TxFloorCostPerToken;
    /// Transaction floor base gas.
    TxFloorCostBaseGas;
    /// Transaction access-list address cost.
    TxAccessListAddressCost;
    /// Transaction access-list storage-key cost.
    TxAccessListStorageKeyCost;
    /// Transaction base stipend.
    TxBaseStipend;
    /// Transaction create cost.
    TxCreateCost;
    /// Transaction initcode cost.
    TxInitcodeCost;
    /// `SSTORE` set refund.
    SstoreSetRefund;
    /// `SSTORE` reset refund.
    SstoreResetRefund;
    /// EIP-7702 transaction authorization refund.
    TxEip7702AuthRefund;
    /// `SSTORE` set state gas.
    SstoreSetStateGas;
    /// New account state gas.
    NewAccountStateGas;
    /// Code deposit state gas.
    CodeDepositStateGas;
    /// `CREATE` state gas.
    CreateStateGas;
    /// EIP-7702 transaction state gas per authorization.
    TxEip7702PerAuthStateGas;
}

/// Gas parameter table.
pub type GasParamTable = [u32; 256];

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

impl Index<GasId> for GasParams {
    type Output = u32;

    #[inline]
    fn index(&self, id: GasId) -> &Self::Output {
        &self.table[id.as_usize()]
    }
}

impl IndexMut<GasId> for GasParams {
    #[inline]
    fn index_mut(&mut self, id: GasId) -> &mut Self::Output {
        &mut self.table[id.as_usize()]
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
    pub const fn new_spec(spec: SpecId) -> Self {
        EvmVersion::<BaseEvmTypes>::new_base(spec).gas_params
    }

    /// Returns the raw gas parameter table.
    #[inline]
    pub const fn table(&self) -> &GasParamTable {
        &self.table
    }

    /// Returns the gas cost for `id`.
    #[inline]
    pub const fn get(&self, id: GasId) -> u64 {
        self.table[id.as_usize()] as u64
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
    pub const fn exp_cost(&self, power: U256) -> u64 {
        if power.const_is_zero() {
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

    /// Calculates dynamic `LOG` gas.
    #[inline]
    pub const fn log_cost(&self, n: u8, len: usize) -> u64 {
        self.get(GasId::Logdata)
            .saturating_mul(len as u64)
            .saturating_add(self.get(GasId::Logtopic).saturating_mul(n as u64))
    }

    /// Calculates initcode word gas for `len` bytes.
    #[inline]
    pub const fn initcode_cost(&self, len: usize) -> u64 {
        self.get(GasId::InitcodePerWord).saturating_mul(num_words(len) as u64)
    }

    /// Calculates dynamic `CREATE2` gas for `len` bytes.
    #[inline]
    pub const fn create2_cost(&self, len: usize) -> u64 {
        self.get(GasId::Create)
            .saturating_add(self.get(GasId::Keccak256PerWord).saturating_mul(num_words(len) as u64))
    }

    /// Returns `CALL` stipend reduction.
    #[inline]
    pub const fn call_stipend_reduction(&self, gas_limit: u64) -> u64 {
        gas_limit - gas_limit / self.get(GasId::CallStipendReduction)
    }

    /// Returns `SELFDESTRUCT` cold account cost.
    #[inline]
    pub const fn selfdestruct_cold_cost(&self) -> u64 {
        self.get(GasId::ColdAccountAdditionalCost)
            .saturating_add(self.get(GasId::WarmStorageReadCost))
    }

    /// Calculates `SELFDESTRUCT` dynamic gas.
    #[inline]
    pub const fn selfdestruct_cost(&self, should_charge_topup: bool, is_cold: bool) -> u64 {
        let mut gas = 0;
        if should_charge_topup {
            gas += self.get(GasId::NewAccountCostForSelfdestruct);
        }
        if is_cold {
            gas += self.selfdestruct_cold_cost();
        }
        gas
    }

    /// Returns additional cold account access gas.
    #[inline]
    pub const fn cold_account_additional_cost(&self) -> u64 {
        self.get(GasId::ColdAccountAdditionalCost)
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
        assert_eq!(GasId::from_u8(GasId::MAX), Some(GasId::TxEip7702PerAuthStateGas));
        assert_eq!(GasId::from_u8(GasId::MAX + 1), None);
        assert_eq!(GasId::from_u8(0), None);
        assert_eq!(GasId::from_name("missing"), None);
    }

    #[test]
    fn gas_params_match_frontier_defaults() {
        let params = GasParams::new_spec(SpecId::FRONTIER);
        assert_eq!(params.get(GasId::ExpByteGas), 10);
        assert_eq!(params.get(GasId::MemoryLinearCost), 3);
        assert_eq!(params.get(GasId::MemoryQuadraticReduction), 512);
        assert_eq!(params.get(GasId::SstoreStatic), 5000);
        assert_eq!(params.get(GasId::SstoreSetWithoutLoadCost), 15000);
        assert_eq!(params.get(GasId::TxCreateCost), 0);
    }

    #[test]
    fn gas_params_apply_homestead_defaults() {
        let params = GasParams::new_spec(SpecId::HOMESTEAD);
        assert_eq!(params.get(GasId::TxCreateCost), 32000);
    }

    #[test]
    fn gas_params_apply_fork_defaults() {
        let tangerine = GasParams::new_spec(SpecId::TANGERINE);
        assert_eq!(tangerine.get(GasId::NewAccountCostForSelfdestruct), 25000);

        let spurious_dragon = GasParams::new_spec(SpecId::SPURIOUS_DRAGON);
        assert_eq!(spurious_dragon.get(GasId::ExpByteGas), 50);

        let istanbul = GasParams::new_spec(SpecId::ISTANBUL);
        assert_eq!(istanbul.get(GasId::SstoreStatic), 800);
        assert_eq!(istanbul.get(GasId::TxTokenNonZeroByteMultiplier), 4);

        let berlin = GasParams::new_spec(SpecId::BERLIN);
        assert_eq!(berlin.get(GasId::SstoreStatic), 100);
        assert_eq!(berlin.get(GasId::ColdAccountAdditionalCost), 2500);
        assert_eq!(berlin.get(GasId::ColdStorageCost), 2100);

        let london = GasParams::new_spec(SpecId::LONDON);
        assert_eq!(london.get(GasId::SstoreClearingSlotRefund), 4800);
        assert_eq!(london.get(GasId::SelfdestructRefund), 0);

        let shanghai = GasParams::new_spec(SpecId::SHANGHAI);
        assert_eq!(shanghai.get(GasId::TxInitcodeCost), 2);

        let prague = GasParams::new_spec(SpecId::PRAGUE);
        assert_eq!(prague.get(GasId::TxEip7702PerEmptyAccountCost), 25000);
        assert_eq!(prague.get(GasId::TxEip7702AuthRefund), 12500);
        assert_eq!(prague.get(GasId::TxFloorCostPerToken), 10);

        let amsterdam = GasParams::new_spec(SpecId::AMSTERDAM);
        assert_eq!(amsterdam.get(GasId::Create), 9000);
        assert_eq!(amsterdam.get(GasId::SstoreSetStateGas), 37568);
        assert_eq!(amsterdam.get(GasId::TxEip7702PerAuthStateGas), 158490);
    }

    #[test]
    fn gas_params_override_values() {
        let mut params = GasParams::new_spec(SpecId::default());
        params[GasId::MemoryLinearCost] = 7;
        params[GasId::MemoryQuadraticReduction] = 1024;
        assert_eq!(params[GasId::MemoryLinearCost], 7);
        assert_eq!(params[GasId::MemoryQuadraticReduction], 1024);
    }

    #[test]
    fn gas_params_calculate_costs() {
        let params = GasParams::new_spec(SpecId::FRONTIER);
        assert_eq!(num_words(0), 0);
        assert_eq!(num_words(33), 2);
        assert_eq!(params.memory_cost(10), 30);
        assert_eq!(params.copy_cost(33), 6);
        assert_eq!(params.extcodecopy_cost(33), 6);
        assert_eq!(params.mcopy_cost(33), 6);
        assert_eq!(params.keccak256_word_cost(33), 12);
        assert_eq!(params.exp_cost(U256::ZERO), 0);
        assert_eq!(params.exp_cost(U256::from(0xff)), 10);
        assert_eq!(params.exp_cost(U256::from(0x100)), 20);
    }
}
