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
        use crate::interpreter::instructions::*;
        use GasId::*;

        let mut static_gas_table = StaticGasTable::empty();
        let mut gas_params = GasParams::empty();
        let mut instruction_impls = InstructionImplTable::empty();

        static_gas_table.set(op::STOP, ZERO as u16);
        static_gas_table.set(op::ADD, VERYLOW as u16);
        static_gas_table.set(op::MUL, LOW as u16);
        static_gas_table.set(op::SUB, VERYLOW as u16);
        static_gas_table.set(op::DIV, LOW as u16);
        static_gas_table.set(op::SDIV, LOW as u16);
        static_gas_table.set(op::MOD, LOW as u16);
        static_gas_table.set(op::SMOD, LOW as u16);
        static_gas_table.set(op::ADDMOD, MID as u16);
        static_gas_table.set(op::MULMOD, MID as u16);
        static_gas_table.set(op::EXP, EXP as u16);
        static_gas_table.set(op::SIGNEXTEND, LOW as u16);

        static_gas_table.set(op::LT, VERYLOW as u16);
        static_gas_table.set(op::GT, VERYLOW as u16);
        static_gas_table.set(op::SLT, VERYLOW as u16);
        static_gas_table.set(op::SGT, VERYLOW as u16);
        static_gas_table.set(op::EQ, VERYLOW as u16);
        static_gas_table.set(op::ISZERO, VERYLOW as u16);
        static_gas_table.set(op::AND, VERYLOW as u16);
        static_gas_table.set(op::OR, VERYLOW as u16);
        static_gas_table.set(op::XOR, VERYLOW as u16);
        static_gas_table.set(op::NOT, VERYLOW as u16);
        static_gas_table.set(op::BYTE, VERYLOW as u16);

        static_gas_table.set(op::KECCAK256, KECCAK256 as u16);

        static_gas_table.set(op::ADDRESS, BASE as u16);
        static_gas_table.set(op::BALANCE, 20);
        static_gas_table.set(op::ORIGIN, BASE as u16);
        static_gas_table.set(op::CALLER, BASE as u16);
        static_gas_table.set(op::CALLVALUE, BASE as u16);
        static_gas_table.set(op::CALLDATALOAD, VERYLOW as u16);
        static_gas_table.set(op::CALLDATASIZE, BASE as u16);
        static_gas_table.set(op::CALLDATACOPY, VERYLOW as u16);
        static_gas_table.set(op::CODESIZE, BASE as u16);
        static_gas_table.set(op::CODECOPY, VERYLOW as u16);
        static_gas_table.set(op::GASPRICE, BASE as u16);
        static_gas_table.set(op::EXTCODESIZE, 20);
        static_gas_table.set(op::EXTCODECOPY, 20);

        static_gas_table.set(op::BLOCKHASH, BLOCKHASH as u16);
        static_gas_table.set(op::COINBASE, BASE as u16);
        static_gas_table.set(op::TIMESTAMP, BASE as u16);
        static_gas_table.set(op::NUMBER, BASE as u16);
        static_gas_table.set(op::DIFFICULTY, BASE as u16);
        static_gas_table.set(op::GASLIMIT, BASE as u16);

        static_gas_table.set(op::POP, BASE as u16);
        static_gas_table.set(op::MLOAD, VERYLOW as u16);
        static_gas_table.set(op::MSTORE, VERYLOW as u16);
        static_gas_table.set(op::MSTORE8, VERYLOW as u16);
        static_gas_table.set(op::SLOAD, 50);
        static_gas_table.set(op::SSTORE, ZERO as u16);
        static_gas_table.set(op::JUMP, MID as u16);
        static_gas_table.set(op::JUMPI, HIGH as u16);
        static_gas_table.set(op::PC, BASE as u16);
        static_gas_table.set(op::MSIZE, BASE as u16);
        static_gas_table.set(op::GAS, BASE as u16);
        static_gas_table.set(op::JUMPDEST, JUMPDEST as u16);

        let mut opcode = op::PUSH1;
        while opcode <= op::PUSH32 {
            static_gas_table.set(opcode, VERYLOW as u16);
            opcode += 1;
        }

        opcode = op::DUP1;
        while opcode <= op::DUP16 {
            static_gas_table.set(opcode, VERYLOW as u16);
            opcode += 1;
        }

        opcode = op::SWAP1;
        while opcode <= op::SWAP16 {
            static_gas_table.set(opcode, VERYLOW as u16);
            opcode += 1;
        }

        static_gas_table.set(op::LOG0, LOG as u16);
        static_gas_table.set(op::LOG1, LOG as u16);
        static_gas_table.set(op::LOG2, LOG as u16);
        static_gas_table.set(op::LOG3, LOG as u16);
        static_gas_table.set(op::LOG4, LOG as u16);

        static_gas_table.set(op::CREATE, ZERO as u16);
        static_gas_table.set(op::CALL, 40);
        static_gas_table.set(op::CALLCODE, 40);
        static_gas_table.set(op::RETURN, ZERO as u16);
        static_gas_table.set(op::INVALID, ZERO as u16);
        static_gas_table.set(op::SELFDESTRUCT, ZERO as u16);

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

        instruction_impls
            .set(op::STOP, Some(&stop as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::ADD, Some(&add as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::MUL, Some(&mul as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::SUB, Some(&sub as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::DIV, Some(&div as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::SDIV, Some(&sdiv as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::MOD, Some(&rem as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::SMOD, Some(&smod as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::ADDMOD, Some(&addmod as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::MULMOD, Some(&mulmod as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::EXP, Some(&exp as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls.set(
            op::SIGNEXTEND,
            Some(&signextend as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(op::LT, Some(&lt as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls.set(op::GT, Some(&gt as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::SLT, Some(&slt as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::SGT, Some(&sgt as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls.set(op::EQ, Some(&eq as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::ISZERO, Some(&iszero as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::AND, Some(&bitand as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::OR, Some(&bitor as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::XOR, Some(&bitxor as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::NOT, Some(&not as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::BYTE, Some(&byte as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls.set(
            op::KECCAK256,
            Some(&keccak256 as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls
            .set(op::ADDRESS, Some(&address as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::BALANCE, Some(&balance as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::ORIGIN, Some(&origin as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::CALLER, Some(&caller as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls.set(
            op::CALLVALUE,
            Some(&callvalue as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::CALLDATALOAD,
            Some(&calldataload as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::CALLDATASIZE,
            Some(&calldatasize as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::CALLDATACOPY,
            Some(&calldatacopy as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls
            .set(op::CODESIZE, Some(&codesize as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::CODECOPY, Some(&codecopy as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::GASPRICE, Some(&gasprice as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls.set(
            op::EXTCODESIZE,
            Some(&extcodesize as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::EXTCODECOPY,
            Some(&extcodecopy as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::BLOCKHASH,
            Some(&blockhash as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls
            .set(op::COINBASE, Some(&coinbase as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls.set(
            op::TIMESTAMP,
            Some(&timestamp as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::NUMBER,
            Some(&block_number as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls.set(
            op::DIFFICULTY,
            Some(&difficulty as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls
            .set(op::GASLIMIT, Some(&gaslimit as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::POP, Some(&pop as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::MLOAD, Some(&mload as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::MSTORE, Some(&mstore as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::MSTORE8, Some(&mstore8 as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::SLOAD, Some(&sload as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::SSTORE, Some(&sstore as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::JUMP, Some(&jump as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::JUMPI, Some(&jumpi as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls.set(op::PC, Some(&pc as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::MSIZE, Some(&msize as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::GAS, Some(&gas as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::JUMPDEST, Some(&jumpdest as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::PUSH1, Some(&push::<1> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::PUSH2, Some(&push::<2> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::PUSH3, Some(&push::<3> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::PUSH4, Some(&push::<4> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::PUSH5, Some(&push::<5> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::PUSH6, Some(&push::<6> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::PUSH7, Some(&push::<7> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::PUSH8, Some(&push::<8> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::PUSH9, Some(&push::<9> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::PUSH10, Some(&push::<10> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::PUSH11, Some(&push::<11> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::PUSH12, Some(&push::<12> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::PUSH13, Some(&push::<13> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::PUSH14, Some(&push::<14> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::PUSH15, Some(&push::<15> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::PUSH16, Some(&push::<16> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::PUSH17, Some(&push::<17> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::PUSH18, Some(&push::<18> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::PUSH19, Some(&push::<19> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::PUSH20, Some(&push::<20> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::PUSH21, Some(&push::<21> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::PUSH22, Some(&push::<22> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::PUSH23, Some(&push::<23> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::PUSH24, Some(&push::<24> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::PUSH25, Some(&push::<25> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::PUSH26, Some(&push::<26> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::PUSH27, Some(&push::<27> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::PUSH28, Some(&push::<28> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::PUSH29, Some(&push::<29> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::PUSH30, Some(&push::<30> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::PUSH31, Some(&push::<31> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::PUSH32, Some(&push::<32> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::DUP1, Some(&dup::<1> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::DUP2, Some(&dup::<2> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::DUP3, Some(&dup::<3> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::DUP4, Some(&dup::<4> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::DUP5, Some(&dup::<5> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::DUP6, Some(&dup::<6> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::DUP7, Some(&dup::<7> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::DUP8, Some(&dup::<8> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::DUP9, Some(&dup::<9> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::DUP10, Some(&dup::<10> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::DUP11, Some(&dup::<11> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::DUP12, Some(&dup::<12> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::DUP13, Some(&dup::<13> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::DUP14, Some(&dup::<14> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::DUP15, Some(&dup::<15> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::DUP16, Some(&dup::<16> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::SWAP1, Some(&swap::<1> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::SWAP2, Some(&swap::<2> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::SWAP3, Some(&swap::<3> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::SWAP4, Some(&swap::<4> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::SWAP5, Some(&swap::<5> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::SWAP6, Some(&swap::<6> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::SWAP7, Some(&swap::<7> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::SWAP8, Some(&swap::<8> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::SWAP9, Some(&swap::<9> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::SWAP10, Some(&swap::<10> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::SWAP11, Some(&swap::<11> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::SWAP12, Some(&swap::<12> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::SWAP13, Some(&swap::<13> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::SWAP14, Some(&swap::<14> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::SWAP15, Some(&swap::<15> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::SWAP16, Some(&swap::<16> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::LOG0, Some(&log::<0> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::LOG1, Some(&log::<1> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::LOG2, Some(&log::<2> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::LOG3, Some(&log::<3> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::LOG4, Some(&log::<4> as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls.set(
            op::CREATE,
            Some(&create::<false> as &'static dyn crate::interpreter::Instruction<C>),
        );
        instruction_impls
            .set(op::CALL, Some(&call as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::CALLCODE, Some(&callcode as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::RETURN, Some(&r#return as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls
            .set(op::INVALID, Some(&invalid as &'static dyn crate::interpreter::Instruction<C>));
        instruction_impls.set(
            op::SELFDESTRUCT,
            Some(&selfdestruct as &'static dyn crate::interpreter::Instruction<C>),
        );

        if spec.enables(SpecId::HOMESTEAD) {
            static_gas_table.set(op::DELEGATECALL, 40);
            gas_params.set(TxCreateCost, CREATE);

            instruction_impls.set(
                op::DELEGATECALL,
                Some(&delegatecall as &'static dyn crate::interpreter::Instruction<C>),
            );
        }

        if spec.enables(SpecId::TANGERINE) {
            static_gas_table.set(op::SLOAD, 200);
            static_gas_table.set(op::BALANCE, 400);
            static_gas_table.set(op::EXTCODESIZE, 700);
            static_gas_table.set(op::EXTCODECOPY, 700);
            static_gas_table.set(op::CALL, 700);
            static_gas_table.set(op::CALLCODE, 700);
            static_gas_table.set(op::DELEGATECALL, 700);
            static_gas_table.set(op::SELFDESTRUCT, 5000);
            gas_params.set(NewAccountCostForSelfdestruct, NEWACCOUNT);
        }

        if spec.enables(SpecId::SPURIOUS_DRAGON) {
            gas_params.set(ExpByteGas, 50);
        }

        if spec.enables(SpecId::BYZANTIUM) {
            static_gas_table.set(op::RETURNDATASIZE, BASE as u16);
            static_gas_table.set(op::RETURNDATACOPY, VERYLOW as u16);
            static_gas_table.set(op::STATICCALL, 700);
            static_gas_table.set(op::REVERT, ZERO as u16);

            instruction_impls.set(
                op::RETURNDATASIZE,
                Some(&returndatasize as &'static dyn crate::interpreter::Instruction<C>),
            );
            instruction_impls.set(
                op::RETURNDATACOPY,
                Some(&returndatacopy as &'static dyn crate::interpreter::Instruction<C>),
            );
            instruction_impls.set(
                op::STATICCALL,
                Some(&staticcall as &'static dyn crate::interpreter::Instruction<C>),
            );
            instruction_impls
                .set(op::REVERT, Some(&revert as &'static dyn crate::interpreter::Instruction<C>));
        }

        if spec.enables(SpecId::CONSTANTINOPLE) {
            static_gas_table.set(op::SHL, VERYLOW as u16);
            static_gas_table.set(op::SHR, VERYLOW as u16);
            static_gas_table.set(op::SAR, VERYLOW as u16);
            static_gas_table.set(op::EXTCODEHASH, 400);

            instruction_impls
                .set(op::SHL, Some(&shl as &'static dyn crate::interpreter::Instruction<C>));
            instruction_impls
                .set(op::SHR, Some(&shr as &'static dyn crate::interpreter::Instruction<C>));
            instruction_impls
                .set(op::SAR, Some(&sar as &'static dyn crate::interpreter::Instruction<C>));
            instruction_impls.set(
                op::EXTCODEHASH,
                Some(&extcodehash as &'static dyn crate::interpreter::Instruction<C>),
            );
        }

        if spec.enables(SpecId::PETERSBURG) {
            static_gas_table.set(op::CREATE2, ZERO as u16);

            instruction_impls.set(
                op::CREATE2,
                Some(&create::<true> as &'static dyn crate::interpreter::Instruction<C>),
            );
        }

        if spec.enables(SpecId::ISTANBUL) {
            static_gas_table.set(op::CHAINID, BASE as u16);
            static_gas_table.set(op::SELFBALANCE, LOW as u16);
            static_gas_table.set(op::SLOAD, ISTANBUL_SLOAD_GAS as u16);
            static_gas_table.set(op::BALANCE, 700);
            static_gas_table.set(op::EXTCODEHASH, 700);
            gas_params.set(SstoreStatic, ISTANBUL_SLOAD_GAS);
            gas_params.set(SstoreSetWithoutLoadCost, SSTORE_SET - ISTANBUL_SLOAD_GAS);
            gas_params.set(SstoreResetWithoutColdLoadCost, SSTORE_RESET - ISTANBUL_SLOAD_GAS);
            gas_params.set(SstoreSetRefund, SSTORE_SET - ISTANBUL_SLOAD_GAS);
            gas_params.set(SstoreResetRefund, SSTORE_RESET - ISTANBUL_SLOAD_GAS);
            gas_params.set(TxTokenNonZeroByteMultiplier, NON_ZERO_BYTE_MULTIPLIER_ISTANBUL);

            instruction_impls.set(
                op::CHAINID,
                Some(&chainid as &'static dyn crate::interpreter::Instruction<C>),
            );
            instruction_impls.set(
                op::SELFBALANCE,
                Some(&selfbalance as &'static dyn crate::interpreter::Instruction<C>),
            );
        }

        if spec.enables(SpecId::BERLIN) {
            static_gas_table.set(op::SLOAD, WARM_STORAGE_READ_COST as u16);
            static_gas_table.set(op::BALANCE, WARM_STORAGE_READ_COST as u16);
            static_gas_table.set(op::EXTCODESIZE, WARM_STORAGE_READ_COST as u16);
            static_gas_table.set(op::EXTCODEHASH, WARM_STORAGE_READ_COST as u16);
            static_gas_table.set(op::EXTCODECOPY, WARM_STORAGE_READ_COST as u16);
            static_gas_table.set(op::CALL, WARM_STORAGE_READ_COST as u16);
            static_gas_table.set(op::CALLCODE, WARM_STORAGE_READ_COST as u16);
            static_gas_table.set(op::DELEGATECALL, WARM_STORAGE_READ_COST as u16);
            static_gas_table.set(op::STATICCALL, WARM_STORAGE_READ_COST as u16);
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
            static_gas_table.set(op::BASEFEE, BASE as u16);
            gas_params.set(SstoreClearingSlotRefund, WARM_SSTORE_RESET + ACCESS_LIST_STORAGE_KEY);
            gas_params.set(SelfdestructRefund, 0);

            instruction_impls.set(
                op::BASEFEE,
                Some(&basefee as &'static dyn crate::interpreter::Instruction<C>),
            );
        }

        if spec.enables(SpecId::SHANGHAI) {
            static_gas_table.set(op::PUSH0, BASE as u16);
            gas_params.set(TxInitcodeCost, INITCODE_WORD_COST);

            instruction_impls.set(
                op::PUSH0,
                Some(&push::<0> as &'static dyn crate::interpreter::Instruction<C>),
            );
        }

        if spec.enables(SpecId::CANCUN) {
            static_gas_table.set(op::BLOBHASH, VERYLOW as u16);
            static_gas_table.set(op::BLOBBASEFEE, BASE as u16);
            static_gas_table.set(op::TLOAD, WARM_STORAGE_READ_COST as u16);
            static_gas_table.set(op::TSTORE, WARM_STORAGE_READ_COST as u16);
            static_gas_table.set(op::MCOPY, VERYLOW as u16);

            instruction_impls.set(
                op::BLOBHASH,
                Some(&blobhash as &'static dyn crate::interpreter::Instruction<C>),
            );
            instruction_impls.set(
                op::BLOBBASEFEE,
                Some(&blobbasefee as &'static dyn crate::interpreter::Instruction<C>),
            );
            instruction_impls
                .set(op::TLOAD, Some(&tload as &'static dyn crate::interpreter::Instruction<C>));
            instruction_impls
                .set(op::TSTORE, Some(&tstore as &'static dyn crate::interpreter::Instruction<C>));
            instruction_impls
                .set(op::MCOPY, Some(&mcopy as &'static dyn crate::interpreter::Instruction<C>));
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
            static_gas_table.set(op::CLZ, LOW as u16);
            static_gas_table.set(op::DUPN, VERYLOW as u16);
            static_gas_table.set(op::SWAPN, VERYLOW as u16);
            static_gas_table.set(op::EXCHANGE, VERYLOW as u16);

            instruction_impls
                .set(op::CLZ, Some(&clz as &'static dyn crate::interpreter::Instruction<C>));
            instruction_impls
                .set(op::DUPN, Some(&dupn as &'static dyn crate::interpreter::Instruction<C>));
            instruction_impls
                .set(op::SWAPN, Some(&swapn as &'static dyn crate::interpreter::Instruction<C>));
            instruction_impls.set(
                op::EXCHANGE,
                Some(&exchange as &'static dyn crate::interpreter::Instruction<C>),
            );
        }

        if spec.enables(SpecId::AMSTERDAM) {
            const CPSB: u32 = 1174;

            static_gas_table.set(op::SLOTNUM, BASE as u16);
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

            instruction_impls.set(
                op::SLOTNUM,
                Some(&slotnum as &'static dyn crate::interpreter::Instruction<C>),
            );
        }

        Self { spec_id: spec, static_gas_table, gas_params, instruction_impls }
    }

    /// Returns the hard fork specification for this version.
    #[inline]
    pub const fn spec_id(&self) -> SpecId {
        self.spec_id
    }
}
