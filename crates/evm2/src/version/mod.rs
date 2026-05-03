//! EVM version data.

mod gas_params;
pub use gas_params::{GasId, GasParams, num_words};

mod gas_table;
pub use gas_table::GasTable;

mod instruction_impl_table;
pub use instruction_impl_table::InstructionImplTable;

use crate::{
    EvmConfig,
    interpreter::{
        SpecId,
        gas::{
            ACCESS_LIST_ADDRESS, ACCESS_LIST_STORAGE_KEY, BASE, BLOCKHASH, CALL_STIPEND, CALLVALUE,
            CODEDEPOSIT, COLD_ACCOUNT_ACCESS_COST_ADDITIONAL, COLD_SLOAD_COST, COPY, CREATE,
            EIP7702_PER_AUTH_BASE_COST, EIP7702_PER_EMPTY_ACCOUNT_COST, EXP, HIGH,
            INITCODE_WORD_COST, ISTANBUL_SLOAD_GAS, JUMPDEST, KECCAK256, KECCAK256WORD, LOG,
            LOGDATA, LOGTOPIC, LOW, MEMORY, MID, NEWACCOUNT, NON_ZERO_BYTE_MULTIPLIER,
            NON_ZERO_BYTE_MULTIPLIER_ISTANBUL, REFUND_SSTORE_CLEARS, SELFDESTRUCT_REFUND,
            SSTORE_RESET, SSTORE_SET, STANDARD_TOKEN_COST, TOTAL_COST_FLOOR_PER_TOKEN, VERYLOW,
            WARM_SSTORE_RESET, WARM_STORAGE_READ_COST, ZERO,
        },
        opcode::{for_each_opcode, op},
    },
};

/// EVM version data.
#[derive(Debug)]
pub struct EvmVersion<C: EvmConfig = crate::BaseEvmTypes> {
    /// Active hard fork specification.
    pub spec_id: SpecId,
    /// Static opcode gas table.
    pub gas_table: GasTable,
    /// Dynamic gas parameter table.
    pub gas_params: GasParams,
    /// Instruction implementations.
    pub instruction_impls: InstructionImplTable<C>,
}

macro_rules! make_instruction_table_inner {
    ([$table:expr, $config:ty, $spec:expr] $(
        ($op:ident, $instr:path),
    )*) => {
        $(
            if opcode_spec(op::$op) as u8 == $spec as u8 {
                $table.set(op::$op, Some(&$instr as &'static dyn crate::interpreter::Instruction<$config>));
            }
        )*
    };
}

const fn opcode_spec(opcode: u8) -> SpecId {
    match opcode {
        op::DELEGATECALL => SpecId::HOMESTEAD,
        op::RETURNDATASIZE | op::RETURNDATACOPY | op::STATICCALL | op::REVERT => SpecId::BYZANTIUM,
        op::SHL | op::SHR | op::SAR | op::EXTCODEHASH => SpecId::CONSTANTINOPLE,
        op::CREATE2 => SpecId::PETERSBURG,
        op::CHAINID | op::SELFBALANCE => SpecId::ISTANBUL,
        op::BASEFEE => SpecId::LONDON,
        op::PUSH0 => SpecId::SHANGHAI,
        op::BLOBHASH | op::BLOBBASEFEE | op::TLOAD | op::TSTORE | op::MCOPY => SpecId::CANCUN,
        op::CLZ | op::DUPN | op::SWAPN | op::EXCHANGE => SpecId::OSAKA,
        op::SLOTNUM => SpecId::AMSTERDAM,
        _ => SpecId::FRONTIER,
    }
}

impl<C: EvmConfig> EvmVersion<C> {
    /// Creates the base EVM version for `spec`.
    pub const fn new_base(spec: SpecId) -> Self {
        use crate::interpreter::instructions::*;
        use GasId::*;

        let mut gas_table = GasTable::empty();
        let mut gas_params = GasParams::empty();
        let mut instruction_impls = InstructionImplTable::empty();

        gas_table.set(op::STOP, ZERO as u16);
        gas_table.set(op::ADD, VERYLOW as u16);
        gas_table.set(op::MUL, LOW as u16);
        gas_table.set(op::SUB, VERYLOW as u16);
        gas_table.set(op::DIV, LOW as u16);
        gas_table.set(op::SDIV, LOW as u16);
        gas_table.set(op::MOD, LOW as u16);
        gas_table.set(op::SMOD, LOW as u16);
        gas_table.set(op::ADDMOD, MID as u16);
        gas_table.set(op::MULMOD, MID as u16);
        gas_table.set(op::EXP, EXP as u16);
        gas_table.set(op::SIGNEXTEND, LOW as u16);

        gas_table.set(op::LT, VERYLOW as u16);
        gas_table.set(op::GT, VERYLOW as u16);
        gas_table.set(op::SLT, VERYLOW as u16);
        gas_table.set(op::SGT, VERYLOW as u16);
        gas_table.set(op::EQ, VERYLOW as u16);
        gas_table.set(op::ISZERO, VERYLOW as u16);
        gas_table.set(op::AND, VERYLOW as u16);
        gas_table.set(op::OR, VERYLOW as u16);
        gas_table.set(op::XOR, VERYLOW as u16);
        gas_table.set(op::NOT, VERYLOW as u16);
        gas_table.set(op::BYTE, VERYLOW as u16);

        gas_table.set(op::KECCAK256, KECCAK256 as u16);

        gas_table.set(op::ADDRESS, BASE as u16);
        gas_table.set(op::BALANCE, 20);
        gas_table.set(op::ORIGIN, BASE as u16);
        gas_table.set(op::CALLER, BASE as u16);
        gas_table.set(op::CALLVALUE, BASE as u16);
        gas_table.set(op::CALLDATALOAD, VERYLOW as u16);
        gas_table.set(op::CALLDATASIZE, BASE as u16);
        gas_table.set(op::CALLDATACOPY, VERYLOW as u16);
        gas_table.set(op::CODESIZE, BASE as u16);
        gas_table.set(op::CODECOPY, VERYLOW as u16);
        gas_table.set(op::GASPRICE, BASE as u16);
        gas_table.set(op::EXTCODESIZE, 20);
        gas_table.set(op::EXTCODECOPY, 20);

        gas_table.set(op::BLOCKHASH, BLOCKHASH as u16);
        gas_table.set(op::COINBASE, BASE as u16);
        gas_table.set(op::TIMESTAMP, BASE as u16);
        gas_table.set(op::NUMBER, BASE as u16);
        gas_table.set(op::DIFFICULTY, BASE as u16);
        gas_table.set(op::GASLIMIT, BASE as u16);

        gas_table.set(op::POP, BASE as u16);
        gas_table.set(op::MLOAD, VERYLOW as u16);
        gas_table.set(op::MSTORE, VERYLOW as u16);
        gas_table.set(op::MSTORE8, VERYLOW as u16);
        gas_table.set(op::SLOAD, 50);
        gas_table.set(op::SSTORE, ZERO as u16);
        gas_table.set(op::JUMP, MID as u16);
        gas_table.set(op::JUMPI, HIGH as u16);
        gas_table.set(op::PC, BASE as u16);
        gas_table.set(op::MSIZE, BASE as u16);
        gas_table.set(op::GAS, BASE as u16);
        gas_table.set(op::JUMPDEST, JUMPDEST as u16);

        let mut opcode = op::PUSH1;
        while opcode <= op::PUSH32 {
            gas_table.set(opcode, VERYLOW as u16);
            opcode += 1;
        }

        opcode = op::DUP1;
        while opcode <= op::DUP16 {
            gas_table.set(opcode, VERYLOW as u16);
            opcode += 1;
        }

        opcode = op::SWAP1;
        while opcode <= op::SWAP16 {
            gas_table.set(opcode, VERYLOW as u16);
            opcode += 1;
        }

        gas_table.set(op::LOG0, LOG as u16);
        gas_table.set(op::LOG1, LOG as u16);
        gas_table.set(op::LOG2, LOG as u16);
        gas_table.set(op::LOG3, LOG as u16);
        gas_table.set(op::LOG4, LOG as u16);

        gas_table.set(op::CREATE, ZERO as u16);
        gas_table.set(op::CALL, 40);
        gas_table.set(op::CALLCODE, 40);
        gas_table.set(op::RETURN, ZERO as u16);
        gas_table.set(op::INVALID, ZERO as u16);
        gas_table.set(op::SELFDESTRUCT, ZERO as u16);

        gas_params.set(ExpByteGas, 10);
        gas_params.set(Logdata, LOGDATA);
        gas_params.set(Logtopic, LOGTOPIC);
        gas_params.set(CopyPerWord, COPY);
        gas_params.set(ExtcodecopyPerWord, COPY);
        gas_params.set(McopyPerWord, COPY);
        gas_params.set(Keccak256PerWord, KECCAK256WORD);
        gas_params.set(MemoryLinearCost, MEMORY);
        gas_params.set(MemoryQuadraticReduction, 512);
        gas_params.set(InitcodePerWord, INITCODE_WORD_COST);
        gas_params.set(Create, CREATE);
        gas_params.set(CallStipendReduction, 64);
        gas_params.set(TransferValueCost, CALLVALUE);
        gas_params.set(NewAccountCost, NEWACCOUNT);
        gas_params.set(SstoreStatic, SSTORE_RESET);
        gas_params.set(SstoreSetWithoutLoadCost, SSTORE_SET - SSTORE_RESET);
        gas_params.set(SstoreSetRefund, SSTORE_SET - SSTORE_RESET);
        gas_params.set(SstoreClearingSlotRefund, REFUND_SSTORE_CLEARS);
        gas_params.set(SelfdestructRefund, SELFDESTRUCT_REFUND);
        gas_params.set(CallStipend, CALL_STIPEND);
        gas_params.set(CodeDepositCost, CODEDEPOSIT);
        gas_params.set(TxTokenNonZeroByteMultiplier, NON_ZERO_BYTE_MULTIPLIER);
        gas_params.set(TxTokenCost, STANDARD_TOKEN_COST);
        gas_params.set(TxBaseStipend, 21000);
        for_each_opcode!([instruction_impls, C, SpecId::FRONTIER] make_instruction_table_inner);

        if spec.enables(SpecId::HOMESTEAD) {
            gas_table.set(op::DELEGATECALL, 40);
            gas_params.set(TxCreateCost, CREATE);
            for_each_opcode!([instruction_impls, C, SpecId::HOMESTEAD] make_instruction_table_inner);
        }

        if spec.enables(SpecId::TANGERINE) {
            gas_table.set(op::SLOAD, 200);
            gas_table.set(op::BALANCE, 400);
            gas_table.set(op::EXTCODESIZE, 700);
            gas_table.set(op::EXTCODECOPY, 700);
            gas_table.set(op::CALL, 700);
            gas_table.set(op::CALLCODE, 700);
            gas_table.set(op::DELEGATECALL, 700);
            gas_table.set(op::SELFDESTRUCT, 5000);
            gas_params.set(NewAccountCostForSelfdestruct, NEWACCOUNT);
        }

        if spec.enables(SpecId::SPURIOUS_DRAGON) {
            gas_params.set(ExpByteGas, 50);
        }

        if spec.enables(SpecId::BYZANTIUM) {
            gas_table.set(op::RETURNDATASIZE, BASE as u16);
            gas_table.set(op::RETURNDATACOPY, VERYLOW as u16);
            gas_table.set(op::STATICCALL, 700);
            gas_table.set(op::REVERT, ZERO as u16);
            for_each_opcode!([instruction_impls, C, SpecId::BYZANTIUM] make_instruction_table_inner);
        }

        if spec.enables(SpecId::CONSTANTINOPLE) {
            gas_table.set(op::SHL, VERYLOW as u16);
            gas_table.set(op::SHR, VERYLOW as u16);
            gas_table.set(op::SAR, VERYLOW as u16);
            gas_table.set(op::EXTCODEHASH, 400);
            for_each_opcode!(
                [instruction_impls, C, SpecId::CONSTANTINOPLE]
                make_instruction_table_inner
            );
        }

        if spec.enables(SpecId::PETERSBURG) {
            gas_table.set(op::CREATE2, ZERO as u16);
            for_each_opcode!([instruction_impls, C, SpecId::PETERSBURG] make_instruction_table_inner);
        }

        if spec.enables(SpecId::ISTANBUL) {
            gas_table.set(op::CHAINID, BASE as u16);
            gas_table.set(op::SELFBALANCE, LOW as u16);
            gas_table.set(op::SLOAD, ISTANBUL_SLOAD_GAS as u16);
            gas_table.set(op::BALANCE, 700);
            gas_table.set(op::EXTCODEHASH, 700);
            gas_params.set(SstoreStatic, ISTANBUL_SLOAD_GAS);
            gas_params.set(SstoreSetWithoutLoadCost, SSTORE_SET - ISTANBUL_SLOAD_GAS);
            gas_params.set(SstoreResetWithoutColdLoadCost, SSTORE_RESET - ISTANBUL_SLOAD_GAS);
            gas_params.set(SstoreSetRefund, SSTORE_SET - ISTANBUL_SLOAD_GAS);
            gas_params.set(SstoreResetRefund, SSTORE_RESET - ISTANBUL_SLOAD_GAS);
            gas_params.set(TxTokenNonZeroByteMultiplier, NON_ZERO_BYTE_MULTIPLIER_ISTANBUL);
            for_each_opcode!([instruction_impls, C, SpecId::ISTANBUL] make_instruction_table_inner);
        }

        if spec.enables(SpecId::BERLIN) {
            gas_table.set(op::SLOAD, WARM_STORAGE_READ_COST as u16);
            gas_table.set(op::BALANCE, WARM_STORAGE_READ_COST as u16);
            gas_table.set(op::EXTCODESIZE, WARM_STORAGE_READ_COST as u16);
            gas_table.set(op::EXTCODEHASH, WARM_STORAGE_READ_COST as u16);
            gas_table.set(op::EXTCODECOPY, WARM_STORAGE_READ_COST as u16);
            gas_table.set(op::CALL, WARM_STORAGE_READ_COST as u16);
            gas_table.set(op::CALLCODE, WARM_STORAGE_READ_COST as u16);
            gas_table.set(op::DELEGATECALL, WARM_STORAGE_READ_COST as u16);
            gas_table.set(op::STATICCALL, WARM_STORAGE_READ_COST as u16);
            gas_params.set(SstoreStatic, WARM_STORAGE_READ_COST);
            gas_params.set(ColdAccountAdditionalCost, COLD_ACCOUNT_ACCESS_COST_ADDITIONAL);
            gas_params.set(ColdStorageAdditionalCost, COLD_SLOAD_COST - WARM_STORAGE_READ_COST);
            gas_params.set(ColdStorageCost, COLD_SLOAD_COST);
            gas_params.set(WarmStorageReadCost, WARM_STORAGE_READ_COST);
            gas_params
                .set(SstoreResetWithoutColdLoadCost, WARM_SSTORE_RESET - WARM_STORAGE_READ_COST);
            gas_params.set(SstoreSetWithoutLoadCost, SSTORE_SET - WARM_STORAGE_READ_COST);
            gas_params.set(SstoreSetRefund, SSTORE_SET - WARM_STORAGE_READ_COST);
            gas_params.set(SstoreResetRefund, WARM_SSTORE_RESET - WARM_STORAGE_READ_COST);
            gas_params.set(TxAccessListAddressCost, ACCESS_LIST_ADDRESS);
            gas_params.set(TxAccessListStorageKeyCost, ACCESS_LIST_STORAGE_KEY);
        }

        if spec.enables(SpecId::LONDON) {
            gas_table.set(op::BASEFEE, BASE as u16);
            gas_params.set(SstoreClearingSlotRefund, WARM_SSTORE_RESET + ACCESS_LIST_STORAGE_KEY);
            gas_params.set(SelfdestructRefund, 0);
            for_each_opcode!([instruction_impls, C, SpecId::LONDON] make_instruction_table_inner);
        }

        if spec.enables(SpecId::SHANGHAI) {
            gas_table.set(op::PUSH0, BASE as u16);
            gas_params.set(TxInitcodeCost, INITCODE_WORD_COST);
            for_each_opcode!([instruction_impls, C, SpecId::SHANGHAI] make_instruction_table_inner);
        }

        if spec.enables(SpecId::CANCUN) {
            gas_table.set(op::BLOBHASH, VERYLOW as u16);
            gas_table.set(op::BLOBBASEFEE, BASE as u16);
            gas_table.set(op::TLOAD, WARM_STORAGE_READ_COST as u16);
            gas_table.set(op::TSTORE, WARM_STORAGE_READ_COST as u16);
            gas_table.set(op::MCOPY, VERYLOW as u16);
            for_each_opcode!([instruction_impls, C, SpecId::CANCUN] make_instruction_table_inner);
        }

        if spec.enables(SpecId::PRAGUE) {
            gas_params.set(TxEip7702PerEmptyAccountCost, EIP7702_PER_EMPTY_ACCOUNT_COST);
            gas_params.set(
                TxEip7702AuthRefund,
                EIP7702_PER_EMPTY_ACCOUNT_COST - EIP7702_PER_AUTH_BASE_COST,
            );
            gas_params.set(TxFloorCostPerToken, TOTAL_COST_FLOOR_PER_TOKEN);
            gas_params.set(TxFloorCostBaseGas, 21000);
        }

        if spec.enables(SpecId::OSAKA) {
            gas_table.set(op::CLZ, LOW as u16);
            gas_table.set(op::DUPN, VERYLOW as u16);
            gas_table.set(op::SWAPN, VERYLOW as u16);
            gas_table.set(op::EXCHANGE, VERYLOW as u16);
            for_each_opcode!([instruction_impls, C, SpecId::OSAKA] make_instruction_table_inner);
        }

        if spec.enables(SpecId::AMSTERDAM) {
            const CPSB: u32 = 1174;

            gas_table.set(op::SLOTNUM, BASE as u16);
            gas_params.set(Create, 9000);
            gas_params.set(TxCreateCost, 9000);
            gas_params.set(CodeDepositCost, 0);
            gas_params.set(NewAccountCost, 0);
            gas_params.set(NewAccountCostForSelfdestruct, 0);
            gas_params.set(SstoreSetWithoutLoadCost, 2800);
            gas_params.set(SstoreSetStateGas, 32 * CPSB);
            gas_params.set(NewAccountStateGas, 112 * CPSB);
            gas_params.set(CodeDepositStateGas, CPSB);
            gas_params.set(CreateStateGas, 112 * CPSB);
            gas_params.set(SstoreSetRefund, 32 * CPSB + 2800);
            gas_params.set(TxEip7702PerEmptyAccountCost, 7500 + (112 + 23) * CPSB);
            gas_params.set(TxEip7702AuthRefund, 112 * CPSB);
            gas_params.set(TxEip7702PerAuthStateGas, (112 + 23) * CPSB);
            for_each_opcode!([instruction_impls, C, SpecId::AMSTERDAM] make_instruction_table_inner);
        }

        Self { spec_id: spec, gas_table, gas_params, instruction_impls }
    }

    /// Returns the hard fork specification for this version.
    #[inline]
    pub const fn spec_id(&self) -> SpecId {
        self.spec_id
    }
}
