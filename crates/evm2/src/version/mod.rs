//! EVM version data.

mod gas_params;
pub use gas_params::{GasId, GasParams, num_words};

mod static_gas_table;
pub use static_gas_table::StaticGasTable;

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
        opcode::op,
    },
};

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
    pub const fn new_base(spec: SpecId) -> Self {
        use crate::interpreter::instructions as instr;
        use GasId::*;

        let mut gt = StaticGasTable::empty();
        let mut gp = GasParams::empty();
        let mut instruction_impls = InstructionImplTable::empty();

        gt.set(op::STOP, ZERO as u16);
        gt.set(op::ADD, VERYLOW as u16);
        gt.set(op::MUL, LOW as u16);
        gt.set(op::SUB, VERYLOW as u16);
        gt.set(op::DIV, LOW as u16);
        gt.set(op::SDIV, LOW as u16);
        gt.set(op::MOD, LOW as u16);
        gt.set(op::SMOD, LOW as u16);
        gt.set(op::ADDMOD, MID as u16);
        gt.set(op::MULMOD, MID as u16);
        gt.set(op::EXP, EXP as u16);
        gt.set(op::SIGNEXTEND, LOW as u16);

        gt.set(op::LT, VERYLOW as u16);
        gt.set(op::GT, VERYLOW as u16);
        gt.set(op::SLT, VERYLOW as u16);
        gt.set(op::SGT, VERYLOW as u16);
        gt.set(op::EQ, VERYLOW as u16);
        gt.set(op::ISZERO, VERYLOW as u16);
        gt.set(op::AND, VERYLOW as u16);
        gt.set(op::OR, VERYLOW as u16);
        gt.set(op::XOR, VERYLOW as u16);
        gt.set(op::NOT, VERYLOW as u16);
        gt.set(op::BYTE, VERYLOW as u16);

        gt.set(op::KECCAK256, KECCAK256 as u16);

        gt.set(op::ADDRESS, BASE as u16);
        gt.set(op::BALANCE, 20);
        gt.set(op::ORIGIN, BASE as u16);
        gt.set(op::CALLER, BASE as u16);
        gt.set(op::CALLVALUE, BASE as u16);
        gt.set(op::CALLDATALOAD, VERYLOW as u16);
        gt.set(op::CALLDATASIZE, BASE as u16);
        gt.set(op::CALLDATACOPY, VERYLOW as u16);
        gt.set(op::CODESIZE, BASE as u16);
        gt.set(op::CODECOPY, VERYLOW as u16);
        gt.set(op::GASPRICE, BASE as u16);
        gt.set(op::EXTCODESIZE, 20);
        gt.set(op::EXTCODECOPY, 20);

        gt.set(op::BLOCKHASH, BLOCKHASH as u16);
        gt.set(op::COINBASE, BASE as u16);
        gt.set(op::TIMESTAMP, BASE as u16);
        gt.set(op::NUMBER, BASE as u16);
        gt.set(op::DIFFICULTY, BASE as u16);
        gt.set(op::GASLIMIT, BASE as u16);

        gt.set(op::POP, BASE as u16);
        gt.set(op::MLOAD, VERYLOW as u16);
        gt.set(op::MSTORE, VERYLOW as u16);
        gt.set(op::MSTORE8, VERYLOW as u16);
        gt.set(op::SLOAD, 50);
        gt.set(op::SSTORE, ZERO as u16);
        gt.set(op::JUMP, MID as u16);
        gt.set(op::JUMPI, HIGH as u16);
        gt.set(op::PC, BASE as u16);
        gt.set(op::MSIZE, BASE as u16);
        gt.set(op::GAS, BASE as u16);
        gt.set(op::JUMPDEST, JUMPDEST as u16);

        let mut opcode = op::PUSH1;
        while opcode <= op::PUSH32 {
            gt.set(opcode, VERYLOW as u16);
            opcode += 1;
        }

        opcode = op::DUP1;
        while opcode <= op::DUP16 {
            gt.set(opcode, VERYLOW as u16);
            opcode += 1;
        }

        opcode = op::SWAP1;
        while opcode <= op::SWAP16 {
            gt.set(opcode, VERYLOW as u16);
            opcode += 1;
        }

        gt.set(op::LOG0, LOG as u16);
        gt.set(op::LOG1, LOG as u16);
        gt.set(op::LOG2, LOG as u16);
        gt.set(op::LOG3, LOG as u16);
        gt.set(op::LOG4, LOG as u16);

        gt.set(op::CREATE, ZERO as u16);
        gt.set(op::CALL, 40);
        gt.set(op::CALLCODE, 40);
        gt.set(op::RETURN, ZERO as u16);
        gt.set(op::INVALID, ZERO as u16);
        gt.set(op::SELFDESTRUCT, ZERO as u16);

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

        instruction_impls
            .set(op::STOP, Some(&instr::stop as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::ADD, Some(&instr::add as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::MUL, Some(&instr::mul as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::SUB, Some(&instr::sub as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::DIV, Some(&instr::div as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::SDIV, Some(&instr::sdiv as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::MOD, Some(&instr::rem as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::SMOD, Some(&instr::smod as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls.set(
            op::ADDMOD,
            Some(&instr::addmod as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::MULMOD,
            Some(&instr::mulmod as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls
            .set(op::EXP, Some(&instr::exp as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls.set(
            op::SIGNEXTEND,
            Some(&instr::signextend as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls
            .set(op::LT, Some(&instr::lt as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::GT, Some(&instr::gt as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::SLT, Some(&instr::slt as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::SGT, Some(&instr::sgt as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::EQ, Some(&instr::eq as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls.set(
            op::ISZERO,
            Some(&instr::iszero as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls
            .set(op::AND, Some(&instr::bitand as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::OR, Some(&instr::bitor as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::XOR, Some(&instr::bitxor as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::NOT, Some(&instr::not as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::BYTE, Some(&instr::byte as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls.set(
            op::KECCAK256,
            Some(&instr::keccak256 as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::ADDRESS,
            Some(&instr::address as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::BALANCE,
            Some(&instr::balance as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::ORIGIN,
            Some(&instr::origin as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::CALLER,
            Some(&instr::caller as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::CALLVALUE,
            Some(&instr::callvalue as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::CALLDATALOAD,
            Some(&instr::calldataload as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::CALLDATASIZE,
            Some(&instr::calldatasize as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::CALLDATACOPY,
            Some(&instr::calldatacopy as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::CODESIZE,
            Some(&instr::codesize as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::CODECOPY,
            Some(&instr::codecopy as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::GASPRICE,
            Some(&instr::gasprice as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::EXTCODESIZE,
            Some(&instr::extcodesize as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::EXTCODECOPY,
            Some(&instr::extcodecopy as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::BLOCKHASH,
            Some(&instr::blockhash as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::COINBASE,
            Some(&instr::coinbase as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::TIMESTAMP,
            Some(&instr::timestamp as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::NUMBER,
            Some(&instr::block_number as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::DIFFICULTY,
            Some(&instr::difficulty as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::GASLIMIT,
            Some(&instr::gaslimit as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls
            .set(op::POP, Some(&instr::pop as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::MLOAD, Some(&instr::mload as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls.set(
            op::MSTORE,
            Some(&instr::mstore as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::MSTORE8,
            Some(&instr::mstore8 as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls
            .set(op::SLOAD, Some(&instr::sload as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls.set(
            op::SSTORE,
            Some(&instr::sstore as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls
            .set(op::JUMP, Some(&instr::jump as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::JUMPI, Some(&instr::jumpi as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::PC, Some(&instr::pc as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::MSIZE, Some(&instr::msize as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::GAS, Some(&instr::gas as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls.set(
            op::JUMPDEST,
            Some(&instr::jumpdest as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::PUSH1,
            Some(&instr::push::<1> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::PUSH2,
            Some(&instr::push::<2> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::PUSH3,
            Some(&instr::push::<3> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::PUSH4,
            Some(&instr::push::<4> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::PUSH5,
            Some(&instr::push::<5> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::PUSH6,
            Some(&instr::push::<6> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::PUSH7,
            Some(&instr::push::<7> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::PUSH8,
            Some(&instr::push::<8> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::PUSH9,
            Some(&instr::push::<9> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::PUSH10,
            Some(&instr::push::<10> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::PUSH11,
            Some(&instr::push::<11> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::PUSH12,
            Some(&instr::push::<12> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::PUSH13,
            Some(&instr::push::<13> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::PUSH14,
            Some(&instr::push::<14> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::PUSH15,
            Some(&instr::push::<15> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::PUSH16,
            Some(&instr::push::<16> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::PUSH17,
            Some(&instr::push::<17> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::PUSH18,
            Some(&instr::push::<18> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::PUSH19,
            Some(&instr::push::<19> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::PUSH20,
            Some(&instr::push::<20> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::PUSH21,
            Some(&instr::push::<21> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::PUSH22,
            Some(&instr::push::<22> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::PUSH23,
            Some(&instr::push::<23> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::PUSH24,
            Some(&instr::push::<24> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::PUSH25,
            Some(&instr::push::<25> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::PUSH26,
            Some(&instr::push::<26> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::PUSH27,
            Some(&instr::push::<27> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::PUSH28,
            Some(&instr::push::<28> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::PUSH29,
            Some(&instr::push::<29> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::PUSH30,
            Some(&instr::push::<30> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::PUSH31,
            Some(&instr::push::<31> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::PUSH32,
            Some(&instr::push::<32> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::DUP1,
            Some(&instr::dup::<1> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::DUP2,
            Some(&instr::dup::<2> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::DUP3,
            Some(&instr::dup::<3> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::DUP4,
            Some(&instr::dup::<4> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::DUP5,
            Some(&instr::dup::<5> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::DUP6,
            Some(&instr::dup::<6> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::DUP7,
            Some(&instr::dup::<7> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::DUP8,
            Some(&instr::dup::<8> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::DUP9,
            Some(&instr::dup::<9> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::DUP10,
            Some(&instr::dup::<10> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::DUP11,
            Some(&instr::dup::<11> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::DUP12,
            Some(&instr::dup::<12> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::DUP13,
            Some(&instr::dup::<13> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::DUP14,
            Some(&instr::dup::<14> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::DUP15,
            Some(&instr::dup::<15> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::DUP16,
            Some(&instr::dup::<16> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::SWAP1,
            Some(&instr::swap::<1> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::SWAP2,
            Some(&instr::swap::<2> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::SWAP3,
            Some(&instr::swap::<3> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::SWAP4,
            Some(&instr::swap::<4> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::SWAP5,
            Some(&instr::swap::<5> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::SWAP6,
            Some(&instr::swap::<6> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::SWAP7,
            Some(&instr::swap::<7> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::SWAP8,
            Some(&instr::swap::<8> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::SWAP9,
            Some(&instr::swap::<9> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::SWAP10,
            Some(&instr::swap::<10> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::SWAP11,
            Some(&instr::swap::<11> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::SWAP12,
            Some(&instr::swap::<12> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::SWAP13,
            Some(&instr::swap::<13> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::SWAP14,
            Some(&instr::swap::<14> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::SWAP15,
            Some(&instr::swap::<15> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::SWAP16,
            Some(&instr::swap::<16> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::LOG0,
            Some(&instr::log::<0> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::LOG1,
            Some(&instr::log::<1> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::LOG2,
            Some(&instr::log::<2> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::LOG3,
            Some(&instr::log::<3> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::LOG4,
            Some(&instr::log::<4> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::CREATE,
            Some(&instr::create::<false> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls
            .set(op::CALL, Some(&instr::call as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls.set(
            op::CALLCODE,
            Some(&instr::callcode as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::RETURN,
            Some(&instr::r#return as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::INVALID,
            Some(&instr::invalid as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::SELFDESTRUCT,
            Some(&instr::selfdestruct as &'static dyn crate::interpreter::Instruction<C>),
        );

        if spec.enables(SpecId::HOMESTEAD) {
            gt.set(op::DELEGATECALL, 40);
            gp.set(TxCreateCost, CREATE);

            instruction_impls.set(
                op::DELEGATECALL,
                Some(&instr::delegatecall as &'static dyn crate::interpreter::Instruction<C>),
            );
        }

        if spec.enables(SpecId::TANGERINE) {
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

        if spec.enables(SpecId::SPURIOUS_DRAGON) {
            gp.set(ExpByteGas, 50);
        }

        if spec.enables(SpecId::BYZANTIUM) {
            gt.set(op::RETURNDATASIZE, BASE as u16);
            gt.set(op::RETURNDATACOPY, VERYLOW as u16);
            gt.set(op::STATICCALL, 700);
            gt.set(op::REVERT, ZERO as u16);

            instruction_impls.set(
                op::RETURNDATASIZE,
                Some(&instr::returndatasize as &'static dyn crate::interpreter::Instruction<C>),
            );
            instruction_impls.set(
                op::RETURNDATACOPY,
                Some(&instr::returndatacopy as &'static dyn crate::interpreter::Instruction<C>),
            );
            instruction_impls.set(
                op::STATICCALL,
                Some(&instr::staticcall as &'static dyn crate::interpreter::Instruction<C>),
            );
            instruction_impls.set(
                op::REVERT,
                Some(&instr::revert as &'static dyn crate::interpreter::Instruction<C>),
            );
        }

        if spec.enables(SpecId::CONSTANTINOPLE) {
            gt.set(op::SHL, VERYLOW as u16);
            gt.set(op::SHR, VERYLOW as u16);
            gt.set(op::SAR, VERYLOW as u16);
            gt.set(op::EXTCODEHASH, 400);

            instruction_impls
                .set(op::SHL, Some(&instr::shl as &'static dyn crate::interpreter::Instruction<C>));
            instruction_impls
                .set(op::SHR, Some(&instr::shr as &'static dyn crate::interpreter::Instruction<C>));
            instruction_impls
                .set(op::SAR, Some(&instr::sar as &'static dyn crate::interpreter::Instruction<C>));
            instruction_impls.set(
                op::EXTCODEHASH,
                Some(&instr::extcodehash as &'static dyn crate::interpreter::Instruction<C>),
            );
        }

        if spec.enables(SpecId::PETERSBURG) {
            gt.set(op::CREATE2, ZERO as u16);

            instruction_impls.set(
                op::CREATE2,
                Some(&instr::create::<true> as &'static dyn crate::interpreter::Instruction<C>),
            );
        }

        if spec.enables(SpecId::ISTANBUL) {
            gt.set(op::CHAINID, BASE as u16);
            gt.set(op::SELFBALANCE, LOW as u16);
            gt.set(op::SLOAD, ISTANBUL_SLOAD_GAS as u16);
            gt.set(op::BALANCE, 700);
            gt.set(op::EXTCODEHASH, 700);
            gp.set(SstoreStatic, ISTANBUL_SLOAD_GAS);
            gp.set(SstoreSetWithoutLoadCost, SSTORE_SET - ISTANBUL_SLOAD_GAS);
            gp.set(SstoreResetWithoutColdLoadCost, SSTORE_RESET - ISTANBUL_SLOAD_GAS);
            gp.set(SstoreSetRefund, SSTORE_SET - ISTANBUL_SLOAD_GAS);
            gp.set(SstoreResetRefund, SSTORE_RESET - ISTANBUL_SLOAD_GAS);
            gp.set(TxTokenNonZeroByteMultiplier, NON_ZERO_BYTE_MULTIPLIER_ISTANBUL);

            instruction_impls.set(
                op::CHAINID,
                Some(&instr::chainid as &'static dyn crate::interpreter::Instruction<C>),
            );
            instruction_impls.set(
                op::SELFBALANCE,
                Some(&instr::selfbalance as &'static dyn crate::interpreter::Instruction<C>),
            );
        }

        if spec.enables(SpecId::BERLIN) {
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

        if spec.enables(SpecId::LONDON) {
            gt.set(op::BASEFEE, BASE as u16);
            gp.set(SstoreClearingSlotRefund, WARM_SSTORE_RESET + ACCESS_LIST_STORAGE_KEY);
            gp.set(SelfdestructRefund, 0);

            instruction_impls.set(
                op::BASEFEE,
                Some(&instr::basefee as &'static dyn crate::interpreter::Instruction<C>),
            );
        }

        if spec.enables(SpecId::SHANGHAI) {
            gt.set(op::PUSH0, BASE as u16);
            gp.set(TxInitcodeCost, INITCODE_WORD_COST);

            instruction_impls.set(
                op::PUSH0,
                Some(&instr::push::<0> as &'static dyn crate::interpreter::Instruction<C>),
            );
        }

        if spec.enables(SpecId::CANCUN) {
            gt.set(op::BLOBHASH, VERYLOW as u16);
            gt.set(op::BLOBBASEFEE, BASE as u16);
            gt.set(op::TLOAD, WARM_STORAGE_READ_COST as u16);
            gt.set(op::TSTORE, WARM_STORAGE_READ_COST as u16);
            gt.set(op::MCOPY, VERYLOW as u16);

            instruction_impls.set(
                op::BLOBHASH,
                Some(&instr::blobhash as &'static dyn crate::interpreter::Instruction<C>),
            );
            instruction_impls.set(
                op::BLOBBASEFEE,
                Some(&instr::blobbasefee as &'static dyn crate::interpreter::Instruction<C>),
            );
            instruction_impls.set(
                op::TLOAD,
                Some(&instr::tload as &'static dyn crate::interpreter::Instruction<C>),
            );
            instruction_impls.set(
                op::TSTORE,
                Some(&instr::tstore as &'static dyn crate::interpreter::Instruction<C>),
            );
            instruction_impls.set(
                op::MCOPY,
                Some(&instr::mcopy as &'static dyn crate::interpreter::Instruction<C>),
            );
        }

        if spec.enables(SpecId::PRAGUE) {
            gp.set(TxEip7702PerEmptyAccountCost, EIP7702_PER_EMPTY_ACCOUNT_COST);
            gp.set(
                TxEip7702AuthRefund,
                EIP7702_PER_EMPTY_ACCOUNT_COST - EIP7702_PER_AUTH_BASE_COST,
            );
            gp.set(TxFloorCostPerToken, TOTAL_COST_FLOOR_PER_TOKEN);
            gp.set(TxFloorCostBaseGas, 21000);
        }

        if spec.enables(SpecId::OSAKA) {
            gt.set(op::CLZ, LOW as u16);
            gt.set(op::DUPN, VERYLOW as u16);
            gt.set(op::SWAPN, VERYLOW as u16);
            gt.set(op::EXCHANGE, VERYLOW as u16);

            instruction_impls
                .set(op::CLZ, Some(&instr::clz as &'static dyn crate::interpreter::Instruction<C>));
            instruction_impls.set(
                op::DUPN,
                Some(&instr::dupn as &'static dyn crate::interpreter::Instruction<C>),
            );
            instruction_impls.set(
                op::SWAPN,
                Some(&instr::swapn as &'static dyn crate::interpreter::Instruction<C>),
            );
            instruction_impls.set(
                op::EXCHANGE,
                Some(&instr::exchange as &'static dyn crate::interpreter::Instruction<C>),
            );
        }

        if spec.enables(SpecId::AMSTERDAM) {
            const CPSB: u32 = 1174;

            gt.set(op::SLOTNUM, BASE as u16);
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

            instruction_impls.set(
                op::SLOTNUM,
                Some(&instr::slotnum as &'static dyn crate::interpreter::Instruction<C>),
            );
        }

        Self { spec_id: spec, static_gas_table: gt, gas_params: gp, instruction_impls }
    }

    /// Returns the hard fork specification for this version.
    #[inline]
    pub const fn spec_id(&self) -> SpecId {
        self.spec_id
    }
}
