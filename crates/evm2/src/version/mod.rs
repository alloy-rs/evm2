//! EVM version definitions.

use crate::{
    EvmConfig, EvmTypes, SpecId,
    interpreter::{instructions as instr, opcode::op},
};
use alloy_eips::eip7825::MAX_TX_GAS_LIMIT_OSAKA;

mod gas_params;
pub use gas_params::{GasId, GasParams};

mod tables;
pub use tables::VersionTables;

/// Runtime version data.
///
/// Holds the active base `SpecId` and dynamic gas parameter table so instructions can read
/// version-dependent runtime parameters without monomorphization.
#[derive(Clone, Copy, Debug)]
pub struct Version {
    /// Active base specification ID.
    spec_id: SpecId,
    /// Dynamic gas parameter table.
    gas_params: GasParams,
    /// Transaction gas limit cap.
    tx_gas_limit_cap: u64,
}

impl Version {
    /// Returns the base EVM version for `spec_id`.
    #[inline]
    pub const fn base(spec_id: SpecId) -> &'static Self {
        &BASE_VERSIONS[spec_id as usize]
    }

    /// Returns the base specification ID for this version.
    #[inline]
    pub const fn spec_id(&self) -> SpecId {
        self.spec_id
    }

    /// Returns the dynamic gas parameter table for this version.
    #[inline]
    pub const fn gas_params(&'static self) -> &'static GasParams {
        &self.gas_params
    }

    /// Returns the transaction gas limit cap for this version.
    #[inline]
    pub const fn tx_gas_limit_cap(&self) -> u64 {
        self.tx_gas_limit_cap
    }
}

const fn base_tx_gas_limit_cap(spec_id: SpecId) -> u64 {
    if spec_id.enables(SpecId::OSAKA) { MAX_TX_GAS_LIMIT_OSAKA } else { u64::MAX }
}

static BASE_VERSIONS: [Version; SpecId::COUNT] = {
    let mut versions = [const {
        Version {
            spec_id: SpecId::FRONTIER,
            gas_params: GasParams::empty(),
            tx_gas_limit_cap: u64::MAX,
        }
    }; SpecId::COUNT];
    let mut i = 0;
    while i < SpecId::COUNT {
        let spec_id = SpecId::try_from_u8(i as u8).unwrap();
        versions[i] = Version {
            spec_id,
            gas_params: base_gas_params(spec_id),
            tx_gas_limit_cap: base_tx_gas_limit_cap(spec_id),
        };
        i += 1;
    }
    versions
};

macro_rules! noop {
    ($($t:tt)*) => {};
}

macro_rules! evm_versions {
    ($($spec:ident { $($tokens:tt)* })*) => {
        /// Creates the base dynamic gas parameters for `spec_id`.
        const fn base_gas_params(spec_id: SpecId) -> GasParams {
            use crate::interpreter::gas::*;
            use GasId::*;

            let mut gp = GasParams::empty();

            use {noop as op, noop as static_gas};
            macro_rules! gas {
                ($id:ident, $value:expr) => {
                    gp.set($id, $value);
                };
            }

            $(
                if spec_id.enables(SpecId::$spec) {
                    $($tokens)*
                }
            )*

            gp
        }

        const fn base_version_tables<T: EvmTypes, Cfg: EvmConfig<T>>() -> VersionTables<T> {
            use crate::interpreter::gas::*;

            let version = Cfg::VERSION;
            let spec_id = version.spec_id();
            let mut v = VersionTables::empty(version);

            macro_rules! op {
                ($name:ident, $cost:expr) => {
                    v.set_instruction(
                        op::$name,
                        Some(op_instr!(T, $name)),
                    );
                    v.set_static_gas(op::$name, $cost as u16);
                };
            }
            macro_rules! static_gas {
                ($name:ident, $cost:expr) => {
                    v.set_static_gas(op::$name, $cost as u16);
                };
            }
            use noop as gas;

            $(
                if spec_id.enables(SpecId::$spec) {
                    $($tokens)*
                }
            )*

            v
        }
    };
}

evm_versions! {
    FRONTIER {
        op!(STOP, ZERO);
        op!(ADD, VERYLOW);
        op!(MUL, LOW);
        op!(SUB, VERYLOW);
        op!(DIV, LOW);
        op!(SDIV, LOW);
        op!(MOD, LOW);
        op!(SMOD, LOW);
        op!(ADDMOD, MID);
        op!(MULMOD, MID);
        op!(EXP, EXP);
        op!(SIGNEXTEND, LOW);
        op!(LT, VERYLOW);
        op!(GT, VERYLOW);
        op!(SLT, VERYLOW);
        op!(SGT, VERYLOW);
        op!(EQ, VERYLOW);
        op!(ISZERO, VERYLOW);
        op!(AND, VERYLOW);
        op!(OR, VERYLOW);
        op!(XOR, VERYLOW);
        op!(NOT, VERYLOW);
        op!(BYTE, VERYLOW);
        op!(KECCAK256, KECCAK256);
        op!(ADDRESS, BASE);
        op!(BALANCE, 20);
        op!(ORIGIN, BASE);
        op!(CALLER, BASE);
        op!(CALLVALUE, BASE);
        op!(CALLDATALOAD, VERYLOW);
        op!(CALLDATASIZE, BASE);
        op!(CALLDATACOPY, VERYLOW);
        op!(CODESIZE, BASE);
        op!(CODECOPY, VERYLOW);
        op!(GASPRICE, BASE);
        op!(EXTCODESIZE, 20);
        op!(EXTCODECOPY, 20);
        op!(BLOCKHASH, BLOCKHASH);
        op!(COINBASE, BASE);
        op!(TIMESTAMP, BASE);
        op!(NUMBER, BASE);
        op!(DIFFICULTY, BASE);
        op!(GASLIMIT, BASE);
        op!(POP, BASE);
        op!(MLOAD, VERYLOW);
        op!(MSTORE, VERYLOW);
        op!(MSTORE8, VERYLOW);
        op!(SLOAD, 50);
        op!(SSTORE, ZERO);
        op!(JUMP, MID);
        op!(JUMPI, HIGH);
        op!(PC, BASE);
        op!(MSIZE, BASE);
        op!(GAS, BASE);
        op!(JUMPDEST, JUMPDEST);
        op!(PUSH1, VERYLOW);
        op!(PUSH2, VERYLOW);
        op!(PUSH3, VERYLOW);
        op!(PUSH4, VERYLOW);
        op!(PUSH5, VERYLOW);
        op!(PUSH6, VERYLOW);
        op!(PUSH7, VERYLOW);
        op!(PUSH8, VERYLOW);
        op!(PUSH9, VERYLOW);
        op!(PUSH10, VERYLOW);
        op!(PUSH11, VERYLOW);
        op!(PUSH12, VERYLOW);
        op!(PUSH13, VERYLOW);
        op!(PUSH14, VERYLOW);
        op!(PUSH15, VERYLOW);
        op!(PUSH16, VERYLOW);
        op!(PUSH17, VERYLOW);
        op!(PUSH18, VERYLOW);
        op!(PUSH19, VERYLOW);
        op!(PUSH20, VERYLOW);
        op!(PUSH21, VERYLOW);
        op!(PUSH22, VERYLOW);
        op!(PUSH23, VERYLOW);
        op!(PUSH24, VERYLOW);
        op!(PUSH25, VERYLOW);
        op!(PUSH26, VERYLOW);
        op!(PUSH27, VERYLOW);
        op!(PUSH28, VERYLOW);
        op!(PUSH29, VERYLOW);
        op!(PUSH30, VERYLOW);
        op!(PUSH31, VERYLOW);
        op!(PUSH32, VERYLOW);
        op!(DUP1, VERYLOW);
        op!(DUP2, VERYLOW);
        op!(DUP3, VERYLOW);
        op!(DUP4, VERYLOW);
        op!(DUP5, VERYLOW);
        op!(DUP6, VERYLOW);
        op!(DUP7, VERYLOW);
        op!(DUP8, VERYLOW);
        op!(DUP9, VERYLOW);
        op!(DUP10, VERYLOW);
        op!(DUP11, VERYLOW);
        op!(DUP12, VERYLOW);
        op!(DUP13, VERYLOW);
        op!(DUP14, VERYLOW);
        op!(DUP15, VERYLOW);
        op!(DUP16, VERYLOW);
        op!(SWAP1, VERYLOW);
        op!(SWAP2, VERYLOW);
        op!(SWAP3, VERYLOW);
        op!(SWAP4, VERYLOW);
        op!(SWAP5, VERYLOW);
        op!(SWAP6, VERYLOW);
        op!(SWAP7, VERYLOW);
        op!(SWAP8, VERYLOW);
        op!(SWAP9, VERYLOW);
        op!(SWAP10, VERYLOW);
        op!(SWAP11, VERYLOW);
        op!(SWAP12, VERYLOW);
        op!(SWAP13, VERYLOW);
        op!(SWAP14, VERYLOW);
        op!(SWAP15, VERYLOW);
        op!(SWAP16, VERYLOW);
        op!(LOG0, LOG);
        op!(LOG1, LOG);
        op!(LOG2, LOG);
        op!(LOG3, LOG);
        op!(LOG4, LOG);
        op!(CREATE, ZERO);
        op!(CALL, 40);
        op!(CALLCODE, 40);
        op!(RETURN, ZERO);
        op!(INVALID, ZERO);
        op!(SELFDESTRUCT, ZERO);

        gas!(ExpByte, 10);
        gas!(Logdata, LOGDATA);
        gas!(Logtopic, LOGTOPIC);
        gas!(CopyPerWord, COPY);
        gas!(ExtcodecopyPerWord, COPY);
        gas!(McopyPerWord, COPY);
        gas!(Keccak256PerWord, KECCAK256WORD);
        gas!(MemoryLinearCost, MEMORY);
        gas!(MemoryQuadraticReduction, 512);
        gas!(InitcodePerWord, INITCODE_WORD_COST);
        gas!(Create, CREATE);
        gas!(CallStipendReduction, 64);
        gas!(TransferValueCost, CALLVALUE);
        gas!(NewAccountCost, NEWACCOUNT);
        gas!(SstoreStatic, SSTORE_RESET);
        gas!(SstoreSetWithoutLoadCost, SSTORE_SET - SSTORE_RESET);
        gas!(SstoreSetRefund, SSTORE_SET - SSTORE_RESET);
        gas!(SstoreClearingSlotRefund, REFUND_SSTORE_CLEARS);
        gas!(SelfdestructRefund, SELFDESTRUCT_REFUND);
        gas!(CallStipend, CALL_STIPEND);
        gas!(CodeDepositCost, CODEDEPOSIT);
        gas!(TxTokenNonZeroByteMultiplier, NON_ZERO_BYTE_MULTIPLIER);
        gas!(TxTokenCost, STANDARD_TOKEN_COST);
        gas!(TxBaseStipend, 21000);
    }

    HOMESTEAD {
        op!(DELEGATECALL, 40);

        gas!(TxCreateCost, CREATE);
    }

    TANGERINE {
        gas!(NewAccountCostForSelfdestruct, NEWACCOUNT);

        static_gas!(SLOAD, 200);
        static_gas!(BALANCE, 400);
        static_gas!(EXTCODESIZE, 700);
        static_gas!(EXTCODECOPY, 700);
        static_gas!(CREATE, ZERO);
        static_gas!(CALL, 700);
        static_gas!(CALLCODE, 700);
        static_gas!(DELEGATECALL, 700);
        static_gas!(SELFDESTRUCT, 5000);
    }

    SPURIOUS_DRAGON {
        gas!(ExpByte, 50);

        static_gas!(EXP, EXP);
    }

    BYZANTIUM {
        op!(RETURNDATASIZE, BASE);
        op!(RETURNDATACOPY, VERYLOW);
        op!(STATICCALL, 700);
        op!(REVERT, ZERO);
    }

    PETERSBURG {
        op!(SHL, VERYLOW);
        op!(SHR, VERYLOW);
        op!(SAR, VERYLOW);
        op!(EXTCODEHASH, 400);
        op!(CREATE2, ZERO);
    }

    ISTANBUL {
        op!(CHAINID, BASE);
        op!(SELFBALANCE, LOW);

        gas!(SstoreStatic, ISTANBUL_SLOAD_GAS);
        gas!(SstoreSetWithoutLoadCost, SSTORE_SET - ISTANBUL_SLOAD_GAS);
        gas!(SstoreResetWithoutColdLoadCost, SSTORE_RESET - ISTANBUL_SLOAD_GAS);
        gas!(SstoreSetRefund, SSTORE_SET - ISTANBUL_SLOAD_GAS);
        gas!(SstoreResetRefund, SSTORE_RESET - ISTANBUL_SLOAD_GAS);
        gas!(TxTokenNonZeroByteMultiplier, NON_ZERO_BYTE_MULTIPLIER_ISTANBUL);

        static_gas!(SLOAD, ISTANBUL_SLOAD_GAS);
        static_gas!(BALANCE, 700);
        static_gas!(EXTCODEHASH, 700);
        static_gas!(SSTORE, ZERO);
    }

    BERLIN {
        gas!(SstoreStatic, WARM_STORAGE_READ_COST);
        gas!(ColdAccountAdditionalCost, COLD_ACCOUNT_ACCESS_COST_ADDITIONAL);
        gas!(ColdStorageAdditionalCost, COLD_SLOAD_COST - WARM_STORAGE_READ_COST);
        gas!(ColdStorageCost, COLD_SLOAD_COST);
        gas!(WarmStorageReadCost, WARM_STORAGE_READ_COST);
        gas!(SstoreResetWithoutColdLoadCost, WARM_SSTORE_RESET - WARM_STORAGE_READ_COST);
        gas!(SstoreSetWithoutLoadCost, SSTORE_SET - WARM_STORAGE_READ_COST);
        gas!(SstoreSetRefund, SSTORE_SET - WARM_STORAGE_READ_COST);
        gas!(SstoreResetRefund, WARM_SSTORE_RESET - WARM_STORAGE_READ_COST);
        gas!(TxAccessListAddressCost, ACCESS_LIST_ADDRESS);
        gas!(TxAccessListStorageKeyCost, ACCESS_LIST_STORAGE_KEY);

        static_gas!(SLOAD, WARM_STORAGE_READ_COST);
        static_gas!(BALANCE, WARM_STORAGE_READ_COST);
        static_gas!(EXTCODESIZE, WARM_STORAGE_READ_COST);
        static_gas!(EXTCODEHASH, WARM_STORAGE_READ_COST);
        static_gas!(EXTCODECOPY, WARM_STORAGE_READ_COST);
        static_gas!(SSTORE, ZERO);
        static_gas!(CALL, WARM_STORAGE_READ_COST);
        static_gas!(CALLCODE, WARM_STORAGE_READ_COST);
        static_gas!(DELEGATECALL, WARM_STORAGE_READ_COST);
        static_gas!(STATICCALL, WARM_STORAGE_READ_COST);
        static_gas!(SELFDESTRUCT, 5000);
    }

    LONDON {
        op!(BASEFEE, BASE);

        gas!(SstoreClearingSlotRefund, WARM_SSTORE_RESET + ACCESS_LIST_STORAGE_KEY);
        gas!(SelfdestructRefund, 0);

        static_gas!(SSTORE, ZERO);
        static_gas!(SELFDESTRUCT, 5000);
    }

    MERGE {}

    SHANGHAI {
        op!(PUSH0, BASE);

        gas!(TxInitcodeCost, INITCODE_WORD_COST);

        static_gas!(CREATE, ZERO);
        static_gas!(CREATE2, ZERO);
    }

    CANCUN {
        op!(BLOBHASH, VERYLOW);
        op!(BLOBBASEFEE, BASE);
        op!(TLOAD, WARM_STORAGE_READ_COST);
        op!(TSTORE, WARM_STORAGE_READ_COST);
        op!(MCOPY, VERYLOW);
    }

    PRAGUE {
        gas!(TxEip7702PerEmptyAccountCost, EIP7702_PER_EMPTY_ACCOUNT_COST);
        gas!(TxEip7702AuthRefund, EIP7702_PER_EMPTY_ACCOUNT_COST - EIP7702_PER_AUTH_BASE_COST);
        gas!(TxFloorCostPerToken, TOTAL_COST_FLOOR_PER_TOKEN);
        gas!(TxFloorCostBase, 21000);
    }

    OSAKA {
        op!(CLZ, LOW);
    }

    AMSTERDAM {
        #[allow(dead_code)]
        const CPSB: u32 = 1174;

        op!(DUPN, VERYLOW);
        op!(SWAPN, VERYLOW);
        op!(EXCHANGE, VERYLOW);
        op!(SLOTNUM, BASE);

        gas!(Create, 9000);
        gas!(TxCreateCost, 9000);
        gas!(CodeDepositCost, 0);
        gas!(NewAccountCost, 0);
        gas!(NewAccountCostForSelfdestruct, 0);
        gas!(SstoreSetWithoutLoadCost, 2800);
        gas!(SstoreSetState, 32 * CPSB);
        gas!(NewAccountState, 112 * CPSB);
        gas!(CodeDepositState, CPSB);
        gas!(CreateState, 112 * CPSB);
        gas!(SstoreSetRefund, 32 * CPSB + 2800);
        gas!(TxEip7702PerEmptyAccountCost, 7500 + (112 + 23) * CPSB);
        gas!(TxEip7702AuthRefund, 112 * CPSB);
        gas!(TxEip7702PerAuthState, (112 + 23) * CPSB);

        static_gas!(CREATE, ZERO);
        static_gas!(CREATE2, ZERO);
        static_gas!(SSTORE, ZERO);
        static_gas!(CALL, WARM_STORAGE_READ_COST);
        static_gas!(CALLCODE, WARM_STORAGE_READ_COST);
        static_gas!(DELEGATECALL, WARM_STORAGE_READ_COST);
        static_gas!(STATICCALL, WARM_STORAGE_READ_COST);
        static_gas!(SELFDESTRUCT, 5000);
    }
}

macro_rules! op_instr {
    ($ty:ident, $name:ident) => {
        <op_instr!(@path $ty, $name) as instr::table::Instruction<$ty>>::execute
    };

    (@path $ty:ident, STOP) => { instr::stop<$ty> };
    (@path $ty:ident, ADD) => { instr::add<$ty> };
    (@path $ty:ident, MUL) => { instr::mul<$ty> };
    (@path $ty:ident, SUB) => { instr::sub<$ty> };
    (@path $ty:ident, DIV) => { instr::div<$ty> };
    (@path $ty:ident, SDIV) => { instr::sdiv<$ty> };
    (@path $ty:ident, MOD) => { instr::rem<$ty> };
    (@path $ty:ident, SMOD) => { instr::smod<$ty> };
    (@path $ty:ident, ADDMOD) => { instr::addmod<$ty> };
    (@path $ty:ident, MULMOD) => { instr::mulmod<$ty> };
    (@path $ty:ident, EXP) => { instr::exp<$ty> };
    (@path $ty:ident, SIGNEXTEND) => { instr::signextend<$ty> };
    (@path $ty:ident, LT) => { instr::lt<$ty> };
    (@path $ty:ident, GT) => { instr::gt<$ty> };
    (@path $ty:ident, SLT) => { instr::slt<$ty> };
    (@path $ty:ident, SGT) => { instr::sgt<$ty> };
    (@path $ty:ident, EQ) => { instr::eq<$ty> };
    (@path $ty:ident, ISZERO) => { instr::iszero<$ty> };
    (@path $ty:ident, AND) => { instr::bitand<$ty> };
    (@path $ty:ident, OR) => { instr::bitor<$ty> };
    (@path $ty:ident, XOR) => { instr::bitxor<$ty> };
    (@path $ty:ident, NOT) => { instr::not<$ty> };
    (@path $ty:ident, BYTE) => { instr::byte<$ty> };
    (@path $ty:ident, SHL) => { instr::shl<$ty> };
    (@path $ty:ident, SHR) => { instr::shr<$ty> };
    (@path $ty:ident, SAR) => { instr::sar<$ty> };
    (@path $ty:ident, CLZ) => { instr::clz<$ty> };
    (@path $ty:ident, KECCAK256) => { instr::keccak256<$ty> };
    (@path $ty:ident, ADDRESS) => { instr::address<$ty> };
    (@path $ty:ident, BALANCE) => { instr::balance<$ty> };
    (@path $ty:ident, ORIGIN) => { instr::origin<$ty> };
    (@path $ty:ident, CALLER) => { instr::caller<$ty> };
    (@path $ty:ident, CALLVALUE) => { instr::callvalue<$ty> };
    (@path $ty:ident, CALLDATALOAD) => { instr::calldataload<$ty> };
    (@path $ty:ident, CALLDATASIZE) => { instr::calldatasize<$ty> };
    (@path $ty:ident, CALLDATACOPY) => { instr::calldatacopy<$ty> };
    (@path $ty:ident, CODESIZE) => { instr::codesize<$ty> };
    (@path $ty:ident, CODECOPY) => { instr::codecopy<$ty> };
    (@path $ty:ident, GASPRICE) => { instr::gasprice<$ty> };
    (@path $ty:ident, EXTCODESIZE) => { instr::extcodesize<$ty> };
    (@path $ty:ident, EXTCODECOPY) => { instr::extcodecopy<$ty> };
    (@path $ty:ident, RETURNDATASIZE) => { instr::returndatasize<$ty> };
    (@path $ty:ident, RETURNDATACOPY) => { instr::returndatacopy<$ty> };
    (@path $ty:ident, EXTCODEHASH) => { instr::extcodehash<$ty> };
    (@path $ty:ident, BLOCKHASH) => { instr::blockhash<$ty> };
    (@path $ty:ident, COINBASE) => { instr::coinbase<$ty> };
    (@path $ty:ident, TIMESTAMP) => { instr::timestamp<$ty> };
    (@path $ty:ident, NUMBER) => { instr::block_number<$ty> };
    (@path $ty:ident, DIFFICULTY) => { instr::difficulty<$ty> };
    (@path $ty:ident, GASLIMIT) => { instr::gaslimit<$ty> };
    (@path $ty:ident, CHAINID) => { instr::chainid<$ty> };
    (@path $ty:ident, SELFBALANCE) => { instr::selfbalance<$ty> };
    (@path $ty:ident, BASEFEE) => { instr::basefee<$ty> };
    (@path $ty:ident, BLOBHASH) => { instr::blobhash<$ty> };
    (@path $ty:ident, BLOBBASEFEE) => { instr::blobbasefee<$ty> };
    (@path $ty:ident, SLOTNUM) => { instr::slotnum<$ty> };
    (@path $ty:ident, POP) => { instr::pop<$ty> };
    (@path $ty:ident, MLOAD) => { instr::mload<$ty> };
    (@path $ty:ident, MSTORE) => { instr::mstore<$ty> };
    (@path $ty:ident, MSTORE8) => { instr::mstore8<$ty> };
    (@path $ty:ident, SLOAD) => { instr::sload<$ty> };
    (@path $ty:ident, SSTORE) => { instr::sstore<$ty> };
    (@path $ty:ident, JUMP) => { instr::jump<$ty> };
    (@path $ty:ident, JUMPI) => { instr::jumpi<$ty> };
    (@path $ty:ident, PC) => { instr::pc<$ty> };
    (@path $ty:ident, MSIZE) => { instr::msize<$ty> };
    (@path $ty:ident, GAS) => { instr::gas<$ty> };
    (@path $ty:ident, JUMPDEST) => { instr::jumpdest<$ty> };
    (@path $ty:ident, TLOAD) => { instr::tload<$ty> };
    (@path $ty:ident, TSTORE) => { instr::tstore<$ty> };
    (@path $ty:ident, MCOPY) => { instr::mcopy<$ty> };
    (@path $ty:ident, PUSH0) => { instr::push<$ty, 0> };
    (@path $ty:ident, PUSH1) => { instr::push<$ty, 1> };
    (@path $ty:ident, PUSH2) => { instr::push<$ty, 2> };
    (@path $ty:ident, PUSH3) => { instr::push<$ty, 3> };
    (@path $ty:ident, PUSH4) => { instr::push<$ty, 4> };
    (@path $ty:ident, PUSH5) => { instr::push<$ty, 5> };
    (@path $ty:ident, PUSH6) => { instr::push<$ty, 6> };
    (@path $ty:ident, PUSH7) => { instr::push<$ty, 7> };
    (@path $ty:ident, PUSH8) => { instr::push<$ty, 8> };
    (@path $ty:ident, PUSH9) => { instr::push<$ty, 9> };
    (@path $ty:ident, PUSH10) => { instr::push<$ty, 10> };
    (@path $ty:ident, PUSH11) => { instr::push<$ty, 11> };
    (@path $ty:ident, PUSH12) => { instr::push<$ty, 12> };
    (@path $ty:ident, PUSH13) => { instr::push<$ty, 13> };
    (@path $ty:ident, PUSH14) => { instr::push<$ty, 14> };
    (@path $ty:ident, PUSH15) => { instr::push<$ty, 15> };
    (@path $ty:ident, PUSH16) => { instr::push<$ty, 16> };
    (@path $ty:ident, PUSH17) => { instr::push<$ty, 17> };
    (@path $ty:ident, PUSH18) => { instr::push<$ty, 18> };
    (@path $ty:ident, PUSH19) => { instr::push<$ty, 19> };
    (@path $ty:ident, PUSH20) => { instr::push<$ty, 20> };
    (@path $ty:ident, PUSH21) => { instr::push<$ty, 21> };
    (@path $ty:ident, PUSH22) => { instr::push<$ty, 22> };
    (@path $ty:ident, PUSH23) => { instr::push<$ty, 23> };
    (@path $ty:ident, PUSH24) => { instr::push<$ty, 24> };
    (@path $ty:ident, PUSH25) => { instr::push<$ty, 25> };
    (@path $ty:ident, PUSH26) => { instr::push<$ty, 26> };
    (@path $ty:ident, PUSH27) => { instr::push<$ty, 27> };
    (@path $ty:ident, PUSH28) => { instr::push<$ty, 28> };
    (@path $ty:ident, PUSH29) => { instr::push<$ty, 29> };
    (@path $ty:ident, PUSH30) => { instr::push<$ty, 30> };
    (@path $ty:ident, PUSH31) => { instr::push<$ty, 31> };
    (@path $ty:ident, PUSH32) => { instr::push<$ty, 32> };
    (@path $ty:ident, DUP1) => { instr::dup<$ty, 1> };
    (@path $ty:ident, DUP2) => { instr::dup<$ty, 2> };
    (@path $ty:ident, DUP3) => { instr::dup<$ty, 3> };
    (@path $ty:ident, DUP4) => { instr::dup<$ty, 4> };
    (@path $ty:ident, DUP5) => { instr::dup<$ty, 5> };
    (@path $ty:ident, DUP6) => { instr::dup<$ty, 6> };
    (@path $ty:ident, DUP7) => { instr::dup<$ty, 7> };
    (@path $ty:ident, DUP8) => { instr::dup<$ty, 8> };
    (@path $ty:ident, DUP9) => { instr::dup<$ty, 9> };
    (@path $ty:ident, DUP10) => { instr::dup<$ty, 10> };
    (@path $ty:ident, DUP11) => { instr::dup<$ty, 11> };
    (@path $ty:ident, DUP12) => { instr::dup<$ty, 12> };
    (@path $ty:ident, DUP13) => { instr::dup<$ty, 13> };
    (@path $ty:ident, DUP14) => { instr::dup<$ty, 14> };
    (@path $ty:ident, DUP15) => { instr::dup<$ty, 15> };
    (@path $ty:ident, DUP16) => { instr::dup<$ty, 16> };
    (@path $ty:ident, SWAP1) => { instr::swap<$ty, 1> };
    (@path $ty:ident, SWAP2) => { instr::swap<$ty, 2> };
    (@path $ty:ident, SWAP3) => { instr::swap<$ty, 3> };
    (@path $ty:ident, SWAP4) => { instr::swap<$ty, 4> };
    (@path $ty:ident, SWAP5) => { instr::swap<$ty, 5> };
    (@path $ty:ident, SWAP6) => { instr::swap<$ty, 6> };
    (@path $ty:ident, SWAP7) => { instr::swap<$ty, 7> };
    (@path $ty:ident, SWAP8) => { instr::swap<$ty, 8> };
    (@path $ty:ident, SWAP9) => { instr::swap<$ty, 9> };
    (@path $ty:ident, SWAP10) => { instr::swap<$ty, 10> };
    (@path $ty:ident, SWAP11) => { instr::swap<$ty, 11> };
    (@path $ty:ident, SWAP12) => { instr::swap<$ty, 12> };
    (@path $ty:ident, SWAP13) => { instr::swap<$ty, 13> };
    (@path $ty:ident, SWAP14) => { instr::swap<$ty, 14> };
    (@path $ty:ident, SWAP15) => { instr::swap<$ty, 15> };
    (@path $ty:ident, SWAP16) => { instr::swap<$ty, 16> };
    (@path $ty:ident, LOG0) => { instr::log<$ty, 0> };
    (@path $ty:ident, LOG1) => { instr::log<$ty, 1> };
    (@path $ty:ident, LOG2) => { instr::log<$ty, 2> };
    (@path $ty:ident, LOG3) => { instr::log<$ty, 3> };
    (@path $ty:ident, LOG4) => { instr::log<$ty, 4> };
    (@path $ty:ident, DUPN) => { instr::dupn<$ty> };
    (@path $ty:ident, SWAPN) => { instr::swapn<$ty> };
    (@path $ty:ident, EXCHANGE) => { instr::exchange<$ty> };
    (@path $ty:ident, CREATE) => { instr::create<$ty, false> };
    (@path $ty:ident, CALL) => { instr::call<$ty> };
    (@path $ty:ident, CALLCODE) => { instr::callcode<$ty> };
    (@path $ty:ident, RETURN) => { instr::r#return<$ty> };
    (@path $ty:ident, DELEGATECALL) => { instr::delegatecall<$ty> };
    (@path $ty:ident, CREATE2) => { instr::create<$ty, true> };
    (@path $ty:ident, STATICCALL) => { instr::staticcall<$ty> };
    (@path $ty:ident, REVERT) => { instr::revert<$ty> };
    (@path $ty:ident, INVALID) => { instr::invalid<$ty> };
    (@path $ty:ident, SELFDESTRUCT) => { instr::selfdestruct<$ty> };
}
use op_instr;
