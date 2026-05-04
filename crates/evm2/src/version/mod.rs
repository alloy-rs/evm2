//! EVM version data.

use crate::{
    EvmConfig,
    interpreter::{SpecId, opcode::op},
};

mod gas_params;
pub use gas_params::{GasId, GasParams, num_words};

mod static_gas_table;
pub use static_gas_table::StaticGasTable;

mod instruction_impl_table;
pub use instruction_impl_table::InstructionImplTable;

/// EVM version data.
#[derive(Debug)]
pub struct EvmVersion<C: EvmConfig = crate::BaseEvmTypes> {
    /// Active hard fork specification.
    pub spec_id: SpecId,
    /// Static opcode gas table.
    pub static_gas_table: StaticGasTable,
    /// Dynamic gas parameter table.
    pub gas_params: GasParams,
    /// Instruction implementations.
    pub instruction_impls: InstructionImplTable<C>,
}

impl<C: EvmConfig> EvmVersion<C> {
    /// Creates the base EVM version for `spec`.
    pub const fn new_base(spec_id: SpecId) -> Self {
        use crate::interpreter::{gas::*, instructions as instr};
        use GasId::*;

        let mut gt = StaticGasTable::empty();
        let mut gp = GasParams::empty();
        let mut i = InstructionImplTable::empty();

        macro_rules! new_op {
            ($name:ident, $cost:expr, $instr:path) => {
                gt.set(op::$name, $cost as u16);
                i.set(op::$name, Some(&<$instr>::NEW));
            };
        }

        new_op!(STOP, ZERO, instr::stop<C>);
        new_op!(ADD, VERYLOW, instr::add<C>);
        new_op!(MUL, LOW, instr::mul<C>);
        new_op!(SUB, VERYLOW, instr::sub<C>);
        new_op!(DIV, LOW, instr::div<C>);
        new_op!(SDIV, LOW, instr::sdiv<C>);
        new_op!(MOD, LOW, instr::rem<C>);
        new_op!(SMOD, LOW, instr::smod<C>);
        new_op!(ADDMOD, MID, instr::addmod<C>);
        new_op!(MULMOD, MID, instr::mulmod<C>);
        new_op!(EXP, EXP, instr::exp<C>);
        new_op!(SIGNEXTEND, LOW, instr::signextend<C>);
        new_op!(LT, VERYLOW, instr::lt<C>);
        new_op!(GT, VERYLOW, instr::gt<C>);
        new_op!(SLT, VERYLOW, instr::slt<C>);
        new_op!(SGT, VERYLOW, instr::sgt<C>);
        new_op!(EQ, VERYLOW, instr::eq<C>);
        new_op!(ISZERO, VERYLOW, instr::iszero<C>);
        new_op!(AND, VERYLOW, instr::bitand<C>);
        new_op!(OR, VERYLOW, instr::bitor<C>);
        new_op!(XOR, VERYLOW, instr::bitxor<C>);
        new_op!(NOT, VERYLOW, instr::not<C>);
        new_op!(BYTE, VERYLOW, instr::byte<C>);
        new_op!(KECCAK256, KECCAK256, instr::keccak256<C>);
        new_op!(ADDRESS, BASE, instr::address<C>);
        new_op!(BALANCE, 20, instr::balance<C>);
        new_op!(ORIGIN, BASE, instr::origin<C>);
        new_op!(CALLER, BASE, instr::caller<C>);
        new_op!(CALLVALUE, BASE, instr::callvalue<C>);
        new_op!(CALLDATALOAD, VERYLOW, instr::calldataload<C>);
        new_op!(CALLDATASIZE, BASE, instr::calldatasize<C>);
        new_op!(CALLDATACOPY, VERYLOW, instr::calldatacopy<C>);
        new_op!(CODESIZE, BASE, instr::codesize<C>);
        new_op!(CODECOPY, VERYLOW, instr::codecopy<C>);
        new_op!(GASPRICE, BASE, instr::gasprice<C>);
        new_op!(EXTCODESIZE, 20, instr::extcodesize<C>);
        new_op!(EXTCODECOPY, 20, instr::extcodecopy<C>);
        new_op!(BLOCKHASH, BLOCKHASH, instr::blockhash<C>);
        new_op!(COINBASE, BASE, instr::coinbase<C>);
        new_op!(TIMESTAMP, BASE, instr::timestamp<C>);
        new_op!(NUMBER, BASE, instr::block_number<C>);
        new_op!(DIFFICULTY, BASE, instr::difficulty<C>);
        new_op!(GASLIMIT, BASE, instr::gaslimit<C>);
        new_op!(POP, BASE, instr::pop<C>);
        new_op!(MLOAD, VERYLOW, instr::mload<C>);
        new_op!(MSTORE, VERYLOW, instr::mstore<C>);
        new_op!(MSTORE8, VERYLOW, instr::mstore8<C>);
        new_op!(SLOAD, 50, instr::sload<C>);
        new_op!(SSTORE, ZERO, instr::sstore<C>);
        new_op!(JUMP, MID, instr::jump<C>);
        new_op!(JUMPI, HIGH, instr::jumpi<C>);
        new_op!(PC, BASE, instr::pc<C>);
        new_op!(MSIZE, BASE, instr::msize<C>);
        new_op!(GAS, BASE, instr::gas<C>);
        new_op!(JUMPDEST, JUMPDEST, instr::jumpdest<C>);
        new_op!(PUSH1, VERYLOW, instr::push<C, 1>);
        new_op!(PUSH2, VERYLOW, instr::push<C, 2>);
        new_op!(PUSH3, VERYLOW, instr::push<C, 3>);
        new_op!(PUSH4, VERYLOW, instr::push<C, 4>);
        new_op!(PUSH5, VERYLOW, instr::push<C, 5>);
        new_op!(PUSH6, VERYLOW, instr::push<C, 6>);
        new_op!(PUSH7, VERYLOW, instr::push<C, 7>);
        new_op!(PUSH8, VERYLOW, instr::push<C, 8>);
        new_op!(PUSH9, VERYLOW, instr::push<C, 9>);
        new_op!(PUSH10, VERYLOW, instr::push<C, 10>);
        new_op!(PUSH11, VERYLOW, instr::push<C, 11>);
        new_op!(PUSH12, VERYLOW, instr::push<C, 12>);
        new_op!(PUSH13, VERYLOW, instr::push<C, 13>);
        new_op!(PUSH14, VERYLOW, instr::push<C, 14>);
        new_op!(PUSH15, VERYLOW, instr::push<C, 15>);
        new_op!(PUSH16, VERYLOW, instr::push<C, 16>);
        new_op!(PUSH17, VERYLOW, instr::push<C, 17>);
        new_op!(PUSH18, VERYLOW, instr::push<C, 18>);
        new_op!(PUSH19, VERYLOW, instr::push<C, 19>);
        new_op!(PUSH20, VERYLOW, instr::push<C, 20>);
        new_op!(PUSH21, VERYLOW, instr::push<C, 21>);
        new_op!(PUSH22, VERYLOW, instr::push<C, 22>);
        new_op!(PUSH23, VERYLOW, instr::push<C, 23>);
        new_op!(PUSH24, VERYLOW, instr::push<C, 24>);
        new_op!(PUSH25, VERYLOW, instr::push<C, 25>);
        new_op!(PUSH26, VERYLOW, instr::push<C, 26>);
        new_op!(PUSH27, VERYLOW, instr::push<C, 27>);
        new_op!(PUSH28, VERYLOW, instr::push<C, 28>);
        new_op!(PUSH29, VERYLOW, instr::push<C, 29>);
        new_op!(PUSH30, VERYLOW, instr::push<C, 30>);
        new_op!(PUSH31, VERYLOW, instr::push<C, 31>);
        new_op!(PUSH32, VERYLOW, instr::push<C, 32>);
        new_op!(DUP1, VERYLOW, instr::dup<C, 1>);
        new_op!(DUP2, VERYLOW, instr::dup<C, 2>);
        new_op!(DUP3, VERYLOW, instr::dup<C, 3>);
        new_op!(DUP4, VERYLOW, instr::dup<C, 4>);
        new_op!(DUP5, VERYLOW, instr::dup<C, 5>);
        new_op!(DUP6, VERYLOW, instr::dup<C, 6>);
        new_op!(DUP7, VERYLOW, instr::dup<C, 7>);
        new_op!(DUP8, VERYLOW, instr::dup<C, 8>);
        new_op!(DUP9, VERYLOW, instr::dup<C, 9>);
        new_op!(DUP10, VERYLOW, instr::dup<C, 10>);
        new_op!(DUP11, VERYLOW, instr::dup<C, 11>);
        new_op!(DUP12, VERYLOW, instr::dup<C, 12>);
        new_op!(DUP13, VERYLOW, instr::dup<C, 13>);
        new_op!(DUP14, VERYLOW, instr::dup<C, 14>);
        new_op!(DUP15, VERYLOW, instr::dup<C, 15>);
        new_op!(DUP16, VERYLOW, instr::dup<C, 16>);
        new_op!(SWAP1, VERYLOW, instr::swap<C, 1>);
        new_op!(SWAP2, VERYLOW, instr::swap<C, 2>);
        new_op!(SWAP3, VERYLOW, instr::swap<C, 3>);
        new_op!(SWAP4, VERYLOW, instr::swap<C, 4>);
        new_op!(SWAP5, VERYLOW, instr::swap<C, 5>);
        new_op!(SWAP6, VERYLOW, instr::swap<C, 6>);
        new_op!(SWAP7, VERYLOW, instr::swap<C, 7>);
        new_op!(SWAP8, VERYLOW, instr::swap<C, 8>);
        new_op!(SWAP9, VERYLOW, instr::swap<C, 9>);
        new_op!(SWAP10, VERYLOW, instr::swap<C, 10>);
        new_op!(SWAP11, VERYLOW, instr::swap<C, 11>);
        new_op!(SWAP12, VERYLOW, instr::swap<C, 12>);
        new_op!(SWAP13, VERYLOW, instr::swap<C, 13>);
        new_op!(SWAP14, VERYLOW, instr::swap<C, 14>);
        new_op!(SWAP15, VERYLOW, instr::swap<C, 15>);
        new_op!(SWAP16, VERYLOW, instr::swap<C, 16>);
        new_op!(LOG0, LOG, instr::log<C, 0>);
        new_op!(LOG1, LOG, instr::log<C, 1>);
        new_op!(LOG2, LOG, instr::log<C, 2>);
        new_op!(LOG3, LOG, instr::log<C, 3>);
        new_op!(LOG4, LOG, instr::log<C, 4>);
        new_op!(CREATE, ZERO, instr::create<C, false>);
        new_op!(CALL, 40, instr::call<C>);
        new_op!(CALLCODE, 40, instr::callcode<C>);
        new_op!(RETURN, ZERO, instr::r#return<C>);
        new_op!(INVALID, ZERO, instr::invalid<C>);
        new_op!(SELFDESTRUCT, ZERO, instr::selfdestruct<C>);

        gp.set(ExpByteGas, 10);
        gp.set(Logdata, LOGDATA);
        gp.set(Logtopic, LOGTOPIC);
        gp.set(CopyPerWord, COPY);
        gp.set(ExtcodecopyPerWord, COPY);
        gp.set(McopyPerWord, COPY);
        gp.set(Keccak256PerWord, KECCAK256WORD);
        gp.set(MemoryLinearCost, MEMORY);
        gp.set(MemoryQuadraticReduction, 512);
        gp.set(InitcodePerWord, INITCODE_WORD_COST);
        gp.set(Create, CREATE);
        gp.set(CallStipendReduction, 64);
        gp.set(TransferValueCost, CALLVALUE);
        gp.set(NewAccountCost, NEWACCOUNT);
        gp.set(SstoreStatic, SSTORE_RESET);
        gp.set(SstoreSetWithoutLoadCost, SSTORE_SET - SSTORE_RESET);
        gp.set(SstoreSetRefund, SSTORE_SET - SSTORE_RESET);
        gp.set(SstoreClearingSlotRefund, REFUND_SSTORE_CLEARS);
        gp.set(SelfdestructRefund, SELFDESTRUCT_REFUND);
        gp.set(CallStipend, CALL_STIPEND);
        gp.set(CodeDepositCost, CODEDEPOSIT);
        gp.set(TxTokenNonZeroByteMultiplier, NON_ZERO_BYTE_MULTIPLIER);
        gp.set(TxTokenCost, STANDARD_TOKEN_COST);
        gp.set(TxBaseStipend, 21000);

        if spec_id.enables(SpecId::HOMESTEAD) {
            new_op!(DELEGATECALL, 40, instr::delegatecall<C>);

            gp.set(TxCreateCost, CREATE);
        }

        if spec_id.enables(SpecId::TANGERINE) {
            gt.set(op::SLOAD, 200);
            gt.set(op::BALANCE, 400);
            gt.set(op::EXTCODESIZE, 700);
            gt.set(op::EXTCODECOPY, 700);
            gt.set(op::CALL, 700);
            gt.set(op::CALLCODE, 700);
            gt.set(op::DELEGATECALL, 700);
            gt.set(op::SELFDESTRUCT, 5000);
            gp.set(NewAccountCostForSelfdestruct, NEWACCOUNT);
        }

        if spec_id.enables(SpecId::SPURIOUS_DRAGON) {
            gp.set(ExpByteGas, 50);
        }

        if spec_id.enables(SpecId::BYZANTIUM) {
            new_op!(RETURNDATASIZE, BASE, instr::returndatasize<C>);
            new_op!(RETURNDATACOPY, VERYLOW, instr::returndatacopy<C>);
            new_op!(STATICCALL, 700, instr::staticcall<C>);
            new_op!(REVERT, ZERO, instr::revert<C>);
        }

        if spec_id.enables(SpecId::CONSTANTINOPLE) {
            new_op!(SHL, VERYLOW, instr::shl<C>);
            new_op!(SHR, VERYLOW, instr::shr<C>);
            new_op!(SAR, VERYLOW, instr::sar<C>);
            new_op!(EXTCODEHASH, 400, instr::extcodehash<C>);
        }

        if spec_id.enables(SpecId::PETERSBURG) {
            new_op!(CREATE2, ZERO, instr::create<C, true>);
        }

        if spec_id.enables(SpecId::ISTANBUL) {
            new_op!(CHAINID, BASE, instr::chainid<C>);
            new_op!(SELFBALANCE, LOW, instr::selfbalance<C>);

            gt.set(op::SLOAD, ISTANBUL_SLOAD_GAS as u16);
            gt.set(op::BALANCE, 700);
            gt.set(op::EXTCODEHASH, 700);

            gp.set(SstoreStatic, ISTANBUL_SLOAD_GAS);
            gp.set(SstoreSetWithoutLoadCost, SSTORE_SET - ISTANBUL_SLOAD_GAS);
            gp.set(SstoreResetWithoutColdLoadCost, SSTORE_RESET - ISTANBUL_SLOAD_GAS);
            gp.set(SstoreSetRefund, SSTORE_SET - ISTANBUL_SLOAD_GAS);
            gp.set(SstoreResetRefund, SSTORE_RESET - ISTANBUL_SLOAD_GAS);
            gp.set(TxTokenNonZeroByteMultiplier, NON_ZERO_BYTE_MULTIPLIER_ISTANBUL);
        }

        if spec_id.enables(SpecId::BERLIN) {
            gt.set(op::SLOAD, WARM_STORAGE_READ_COST as u16);
            gt.set(op::BALANCE, WARM_STORAGE_READ_COST as u16);
            gt.set(op::EXTCODESIZE, WARM_STORAGE_READ_COST as u16);
            gt.set(op::EXTCODEHASH, WARM_STORAGE_READ_COST as u16);
            gt.set(op::EXTCODECOPY, WARM_STORAGE_READ_COST as u16);
            gt.set(op::CALL, WARM_STORAGE_READ_COST as u16);
            gt.set(op::CALLCODE, WARM_STORAGE_READ_COST as u16);
            gt.set(op::DELEGATECALL, WARM_STORAGE_READ_COST as u16);
            gt.set(op::STATICCALL, WARM_STORAGE_READ_COST as u16);

            gp.set(SstoreStatic, WARM_STORAGE_READ_COST);
            gp.set(ColdAccountAdditionalCost, COLD_ACCOUNT_ACCESS_COST_ADDITIONAL);
            gp.set(ColdStorageAdditionalCost, COLD_SLOAD_COST - WARM_STORAGE_READ_COST);
            gp.set(ColdStorageCost, COLD_SLOAD_COST);
            gp.set(WarmStorageReadCost, WARM_STORAGE_READ_COST);
            gp.set(SstoreResetWithoutColdLoadCost, WARM_SSTORE_RESET - WARM_STORAGE_READ_COST);
            gp.set(SstoreSetWithoutLoadCost, SSTORE_SET - WARM_STORAGE_READ_COST);
            gp.set(SstoreSetRefund, SSTORE_SET - WARM_STORAGE_READ_COST);
            gp.set(SstoreResetRefund, WARM_SSTORE_RESET - WARM_STORAGE_READ_COST);
            gp.set(TxAccessListAddressCost, ACCESS_LIST_ADDRESS);
            gp.set(TxAccessListStorageKeyCost, ACCESS_LIST_STORAGE_KEY);
        }

        if spec_id.enables(SpecId::LONDON) {
            new_op!(BASEFEE, BASE, instr::basefee<C>);

            gp.set(SstoreClearingSlotRefund, WARM_SSTORE_RESET + ACCESS_LIST_STORAGE_KEY);
            gp.set(SelfdestructRefund, 0);
        }

        if spec_id.enables(SpecId::SHANGHAI) {
            new_op!(PUSH0, BASE, instr::push<C, 0>);

            gp.set(TxInitcodeCost, INITCODE_WORD_COST);
        }

        if spec_id.enables(SpecId::CANCUN) {
            new_op!(BLOBHASH, VERYLOW, instr::blobhash<C>);
            new_op!(BLOBBASEFEE, BASE, instr::blobbasefee<C>);
            new_op!(TLOAD, WARM_STORAGE_READ_COST, instr::tload<C>);
            new_op!(TSTORE, WARM_STORAGE_READ_COST, instr::tstore<C>);
            new_op!(MCOPY, VERYLOW, instr::mcopy<C>);
        }

        if spec_id.enables(SpecId::PRAGUE) {
            gp.set(TxEip7702PerEmptyAccountCost, EIP7702_PER_EMPTY_ACCOUNT_COST);
            gp.set(
                TxEip7702AuthRefund,
                EIP7702_PER_EMPTY_ACCOUNT_COST - EIP7702_PER_AUTH_BASE_COST,
            );
            gp.set(TxFloorCostPerToken, TOTAL_COST_FLOOR_PER_TOKEN);
            gp.set(TxFloorCostBaseGas, 21000);
        }

        if spec_id.enables(SpecId::OSAKA) {
            new_op!(CLZ, LOW, instr::clz<C>);
            new_op!(DUPN, VERYLOW, instr::dupn<C>);
            new_op!(SWAPN, VERYLOW, instr::swapn<C>);
            new_op!(EXCHANGE, VERYLOW, instr::exchange<C>);
        }

        if spec_id.enables(SpecId::AMSTERDAM) {
            const CPSB: u32 = 1174;

            new_op!(SLOTNUM, BASE, instr::slotnum<C>);

            gp.set(Create, 9000);
            gp.set(TxCreateCost, 9000);
            gp.set(CodeDepositCost, 0);
            gp.set(NewAccountCost, 0);
            gp.set(NewAccountCostForSelfdestruct, 0);
            gp.set(SstoreSetWithoutLoadCost, 2800);
            gp.set(SstoreSetStateGas, 32 * CPSB);
            gp.set(NewAccountStateGas, 112 * CPSB);
            gp.set(CodeDepositStateGas, CPSB);
            gp.set(CreateStateGas, 112 * CPSB);
            gp.set(SstoreSetRefund, 32 * CPSB + 2800);
            gp.set(TxEip7702PerEmptyAccountCost, 7500 + (112 + 23) * CPSB);
            gp.set(TxEip7702AuthRefund, 112 * CPSB);
            gp.set(TxEip7702PerAuthStateGas, (112 + 23) * CPSB);
        }

        Self { spec_id, static_gas_table: gt, gas_params: gp, instruction_impls: i }
    }

    /// Returns the hard fork specification for this version.
    #[inline]
    pub const fn spec_id(&self) -> SpecId {
        self.spec_id
    }
}
