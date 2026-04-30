use crate::interpreter::SpecId;
use alloy_primitives::U256;
use paste::paste;

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
    (#[$first_doc:meta] $first_variant:ident; $(#[$doc:meta] $variant:ident;)*) => {
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
                        Self::$first_variant => stringify!([<$first_variant:snake>]),
                        $(
                            Self::$variant => stringify!([<$variant:snake>]),
                        )*
                    }
                }

                /// Returns the gas parameter for a raw identifier.
                #[inline]
                pub const fn from_u8(value: u8) -> Option<Self> {
                    if value == Self::$first_variant as u8 {
                        return Some(Self::$first_variant);
                    }
                    $(
                        if value == Self::$variant as u8 {
                            return Some(Self::$variant);
                        }
                    )*
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
}
