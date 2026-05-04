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

        macro_rules! set_op {
            ($name:ident, $cost:expr, $instr:path) => {
                gt.set(op::$name, $cost as u16);
                i.set(op::$name, Some(&$instr));
            };
        }

        set_op!(STOP, ZERO, instr::stop);
        set_op!(ADD, VERYLOW, instr::add);
        set_op!(MUL, LOW, instr::mul);
        set_op!(SUB, VERYLOW, instr::sub);
        set_op!(DIV, LOW, instr::div);
        set_op!(SDIV, LOW, instr::sdiv);
        set_op!(MOD, LOW, instr::rem);
        set_op!(SMOD, LOW, instr::smod);
        set_op!(ADDMOD, MID, instr::addmod);
        set_op!(MULMOD, MID, instr::mulmod);
        set_op!(EXP, EXP, instr::exp);
        set_op!(SIGNEXTEND, LOW, instr::signextend);
        set_op!(LT, VERYLOW, instr::lt);
        set_op!(GT, VERYLOW, instr::gt);
        set_op!(SLT, VERYLOW, instr::slt);
        set_op!(SGT, VERYLOW, instr::sgt);
        set_op!(EQ, VERYLOW, instr::eq);
        set_op!(ISZERO, VERYLOW, instr::iszero);
        set_op!(AND, VERYLOW, instr::bitand);
        set_op!(OR, VERYLOW, instr::bitor);
        set_op!(XOR, VERYLOW, instr::bitxor);
        set_op!(NOT, VERYLOW, instr::not);
        set_op!(BYTE, VERYLOW, instr::byte);
        set_op!(KECCAK256, KECCAK256, instr::keccak256);
        set_op!(ADDRESS, BASE, instr::address);
        set_op!(BALANCE, 20, instr::balance);
        set_op!(ORIGIN, BASE, instr::origin);
        set_op!(CALLER, BASE, instr::caller);
        set_op!(CALLVALUE, BASE, instr::callvalue);
        set_op!(CALLDATALOAD, VERYLOW, instr::calldataload);
        set_op!(CALLDATASIZE, BASE, instr::calldatasize);
        set_op!(CALLDATACOPY, VERYLOW, instr::calldatacopy);
        set_op!(CODESIZE, BASE, instr::codesize);
        set_op!(CODECOPY, VERYLOW, instr::codecopy);
        set_op!(GASPRICE, BASE, instr::gasprice);
        set_op!(EXTCODESIZE, 20, instr::extcodesize);
        set_op!(EXTCODECOPY, 20, instr::extcodecopy);
        set_op!(BLOCKHASH, BLOCKHASH, instr::blockhash);
        set_op!(COINBASE, BASE, instr::coinbase);
        set_op!(TIMESTAMP, BASE, instr::timestamp);
        set_op!(NUMBER, BASE, instr::block_number);
        set_op!(DIFFICULTY, BASE, instr::difficulty);
        set_op!(GASLIMIT, BASE, instr::gaslimit);
        set_op!(POP, BASE, instr::pop);
        set_op!(MLOAD, VERYLOW, instr::mload);
        set_op!(MSTORE, VERYLOW, instr::mstore);
        set_op!(MSTORE8, VERYLOW, instr::mstore8);
        set_op!(SLOAD, 50, instr::sload);
        set_op!(SSTORE, ZERO, instr::sstore);
        set_op!(JUMP, MID, instr::jump);
        set_op!(JUMPI, HIGH, instr::jumpi);
        set_op!(PC, BASE, instr::pc);
        set_op!(MSIZE, BASE, instr::msize);
        set_op!(GAS, BASE, instr::gas);
        set_op!(JUMPDEST, JUMPDEST, instr::jumpdest);
        set_op!(PUSH1, VERYLOW, instr::push::<1>);
        set_op!(PUSH2, VERYLOW, instr::push::<2>);
        set_op!(PUSH3, VERYLOW, instr::push::<3>);
        set_op!(PUSH4, VERYLOW, instr::push::<4>);
        set_op!(PUSH5, VERYLOW, instr::push::<5>);
        set_op!(PUSH6, VERYLOW, instr::push::<6>);
        set_op!(PUSH7, VERYLOW, instr::push::<7>);
        set_op!(PUSH8, VERYLOW, instr::push::<8>);
        set_op!(PUSH9, VERYLOW, instr::push::<9>);
        set_op!(PUSH10, VERYLOW, instr::push::<10>);
        set_op!(PUSH11, VERYLOW, instr::push::<11>);
        set_op!(PUSH12, VERYLOW, instr::push::<12>);
        set_op!(PUSH13, VERYLOW, instr::push::<13>);
        set_op!(PUSH14, VERYLOW, instr::push::<14>);
        set_op!(PUSH15, VERYLOW, instr::push::<15>);
        set_op!(PUSH16, VERYLOW, instr::push::<16>);
        set_op!(PUSH17, VERYLOW, instr::push::<17>);
        set_op!(PUSH18, VERYLOW, instr::push::<18>);
        set_op!(PUSH19, VERYLOW, instr::push::<19>);
        set_op!(PUSH20, VERYLOW, instr::push::<20>);
        set_op!(PUSH21, VERYLOW, instr::push::<21>);
        set_op!(PUSH22, VERYLOW, instr::push::<22>);
        set_op!(PUSH23, VERYLOW, instr::push::<23>);
        set_op!(PUSH24, VERYLOW, instr::push::<24>);
        set_op!(PUSH25, VERYLOW, instr::push::<25>);
        set_op!(PUSH26, VERYLOW, instr::push::<26>);
        set_op!(PUSH27, VERYLOW, instr::push::<27>);
        set_op!(PUSH28, VERYLOW, instr::push::<28>);
        set_op!(PUSH29, VERYLOW, instr::push::<29>);
        set_op!(PUSH30, VERYLOW, instr::push::<30>);
        set_op!(PUSH31, VERYLOW, instr::push::<31>);
        set_op!(PUSH32, VERYLOW, instr::push::<32>);
        set_op!(DUP1, VERYLOW, instr::dup::<1>);
        set_op!(DUP2, VERYLOW, instr::dup::<2>);
        set_op!(DUP3, VERYLOW, instr::dup::<3>);
        set_op!(DUP4, VERYLOW, instr::dup::<4>);
        set_op!(DUP5, VERYLOW, instr::dup::<5>);
        set_op!(DUP6, VERYLOW, instr::dup::<6>);
        set_op!(DUP7, VERYLOW, instr::dup::<7>);
        set_op!(DUP8, VERYLOW, instr::dup::<8>);
        set_op!(DUP9, VERYLOW, instr::dup::<9>);
        set_op!(DUP10, VERYLOW, instr::dup::<10>);
        set_op!(DUP11, VERYLOW, instr::dup::<11>);
        set_op!(DUP12, VERYLOW, instr::dup::<12>);
        set_op!(DUP13, VERYLOW, instr::dup::<13>);
        set_op!(DUP14, VERYLOW, instr::dup::<14>);
        set_op!(DUP15, VERYLOW, instr::dup::<15>);
        set_op!(DUP16, VERYLOW, instr::dup::<16>);
        set_op!(SWAP1, VERYLOW, instr::swap::<1>);
        set_op!(SWAP2, VERYLOW, instr::swap::<2>);
        set_op!(SWAP3, VERYLOW, instr::swap::<3>);
        set_op!(SWAP4, VERYLOW, instr::swap::<4>);
        set_op!(SWAP5, VERYLOW, instr::swap::<5>);
        set_op!(SWAP6, VERYLOW, instr::swap::<6>);
        set_op!(SWAP7, VERYLOW, instr::swap::<7>);
        set_op!(SWAP8, VERYLOW, instr::swap::<8>);
        set_op!(SWAP9, VERYLOW, instr::swap::<9>);
        set_op!(SWAP10, VERYLOW, instr::swap::<10>);
        set_op!(SWAP11, VERYLOW, instr::swap::<11>);
        set_op!(SWAP12, VERYLOW, instr::swap::<12>);
        set_op!(SWAP13, VERYLOW, instr::swap::<13>);
        set_op!(SWAP14, VERYLOW, instr::swap::<14>);
        set_op!(SWAP15, VERYLOW, instr::swap::<15>);
        set_op!(SWAP16, VERYLOW, instr::swap::<16>);
        set_op!(LOG0, LOG, instr::log::<0>);
        set_op!(LOG1, LOG, instr::log::<1>);
        set_op!(LOG2, LOG, instr::log::<2>);
        set_op!(LOG3, LOG, instr::log::<3>);
        set_op!(LOG4, LOG, instr::log::<4>);
        set_op!(CREATE, ZERO, instr::create::<false>);
        set_op!(CALL, 40, instr::call);
        set_op!(CALLCODE, 40, instr::callcode);
        set_op!(RETURN, ZERO, instr::r#return);
        set_op!(INVALID, ZERO, instr::invalid);
        set_op!(SELFDESTRUCT, ZERO, instr::selfdestruct);

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
            set_op!(DELEGATECALL, 40, instr::delegatecall);

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
            set_op!(RETURNDATASIZE, BASE, instr::returndatasize);
            set_op!(RETURNDATACOPY, VERYLOW, instr::returndatacopy);
            set_op!(STATICCALL, 700, instr::staticcall);
            set_op!(REVERT, ZERO, instr::revert);
        }

        if spec_id.enables(SpecId::CONSTANTINOPLE) {
            set_op!(SHL, VERYLOW, instr::shl);
            set_op!(SHR, VERYLOW, instr::shr);
            set_op!(SAR, VERYLOW, instr::sar);
            set_op!(EXTCODEHASH, 400, instr::extcodehash);
        }

        if spec_id.enables(SpecId::PETERSBURG) {
            set_op!(CREATE2, ZERO, instr::create::<true>);
        }

        if spec_id.enables(SpecId::ISTANBUL) {
            set_op!(CHAINID, BASE, instr::chainid);
            set_op!(SELFBALANCE, LOW, instr::selfbalance);

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
            set_op!(BASEFEE, BASE, instr::basefee);

            gp.set(SstoreClearingSlotRefund, WARM_SSTORE_RESET + ACCESS_LIST_STORAGE_KEY);
            gp.set(SelfdestructRefund, 0);
        }

        if spec_id.enables(SpecId::SHANGHAI) {
            set_op!(PUSH0, BASE, instr::push::<0>);

            gp.set(TxInitcodeCost, INITCODE_WORD_COST);
        }

        if spec_id.enables(SpecId::CANCUN) {
            set_op!(BLOBHASH, VERYLOW, instr::blobhash);
            set_op!(BLOBBASEFEE, BASE, instr::blobbasefee);
            set_op!(TLOAD, WARM_STORAGE_READ_COST, instr::tload);
            set_op!(TSTORE, WARM_STORAGE_READ_COST, instr::tstore);
            set_op!(MCOPY, VERYLOW, instr::mcopy);
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
            set_op!(CLZ, LOW, instr::clz);
            set_op!(DUPN, VERYLOW, instr::dupn);
            set_op!(SWAPN, VERYLOW, instr::swapn);
            set_op!(EXCHANGE, VERYLOW, instr::exchange);
        }

        if spec_id.enables(SpecId::AMSTERDAM) {
            const CPSB: u32 = 1174;

            set_op!(SLOTNUM, BASE, instr::slotnum);

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
