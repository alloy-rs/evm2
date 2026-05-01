//! Instruction dispatch tables.

#[cfg(feature = "nightly")]
use crate::interpreter::{InstrStop, Interpreter, Pc};
use crate::{
    EvmConfig,
    interpreter::{
        Gas, GasParams, PcMut, Result, SpecId, Stack, State,
        gas::{
            BASE, BLOCKHASH, EXP, HIGH, ISTANBUL_SLOAD_GAS, JUMPDEST, KECCAK256, LOG, LOW, MID,
            VERYLOW, WARM_STORAGE_READ_COST, ZERO,
        },
        opcode::{for_each_opcode, op},
    },
};
#[cfg(feature = "nightly")]
use core::hint::cold_path;
use core::ops::{Index, IndexMut};

/// Opcode gas table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GasTable([u16; 256]);

/// Instruction implementation table.
#[derive(Clone, Copy)]
pub struct InstructionImplTable<C: EvmConfig>([Option<&'static dyn Instruction<C>>; 256]);

impl Index<usize> for GasTable {
    type Output = u16;

    #[inline]
    fn index(&self, index: usize) -> &Self::Output {
        &self.0[index]
    }
}

impl IndexMut<usize> for GasTable {
    #[inline]
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        &mut self.0[index]
    }
}

impl<C: EvmConfig> core::fmt::Debug for InstructionImplTable<C> {
    #[inline]
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("InstructionImplTable").finish_non_exhaustive()
    }
}

impl<C: EvmConfig> Index<usize> for InstructionImplTable<C> {
    type Output = Option<&'static dyn Instruction<C>>;

    #[inline]
    fn index(&self, index: usize) -> &Self::Output {
        &self.0[index]
    }
}

impl<C: EvmConfig> IndexMut<usize> for InstructionImplTable<C> {
    #[inline]
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        &mut self.0[index]
    }
}

impl<C: EvmConfig> Default for InstructionImplTable<C> {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl<C: EvmConfig> InstructionImplTable<C> {
    /// Returns the instruction implementation for `opcode`.
    #[inline]
    pub const fn get(&self, opcode: u8) -> Option<&'static dyn Instruction<C>> {
        self.0[opcode as usize]
    }

    /// Returns the mutable instruction implementation slot for `opcode`.
    #[inline]
    pub const fn get_mut(&mut self, opcode: u8) -> &mut Option<&'static dyn Instruction<C>> {
        &mut self.0[opcode as usize]
    }

    /// Sets the instruction implementation for `opcode`.
    #[inline]
    pub const fn set(&mut self, opcode: u8, instr: Option<&'static dyn Instruction<C>>) {
        self.0[opcode as usize] = instr;
    }
}

/// Normal instruction return value.
#[cfg(not(feature = "nightly"))]
pub(crate) type InstructionFnRet = (usize, Result);

/// Normal instruction function pointer.
#[cfg(not(feature = "nightly"))]
pub(crate) type InstructionFn = extern_table!(
    fn(stack: Stack<'_>, pc: PcMut<'_>, gas: &mut Gas, state: &mut State<'_>) -> InstructionFnRet
);

/// Normal instruction dispatch table.
#[cfg(not(feature = "nightly"))]
pub(crate) type InstructionTable = [InstructionFn; 256];

/// Tail instruction return value.
#[cfg(feature = "nightly")]
pub(crate) type TailInstructionFnRet = InstrStop;

/// Tail instruction function pointer.
#[cfg(feature = "nightly")]
pub(crate) type TailInstructionFn = extern_table!(
    fn(
        stack: Stack<'_>,
        pc: Pc<'_>,
        gas: &mut Gas,
        state: &mut State<'_>,
        ret: *const (),
    ) -> TailInstructionFnRet
);

/// Tail instruction dispatch table.
#[cfg(feature = "nightly")]
pub(crate) type TailInstructionTable = [TailInstructionFn; 256];

pub(crate) trait InstructionTables: EvmConfig {
    #[cfg(not(feature = "nightly"))]
    const INSTRUCTIONS: InstructionTable = make_normal_instruction_table::<Self>();

    #[cfg(feature = "nightly")]
    const TAIL_INSTRUCTIONS: TailInstructionTable = make_tail_instruction_table::<Self>();
}

impl<C: EvmConfig> InstructionTables for C {}

/// Instruction execution context.
#[derive(Debug)]
pub(crate) struct InstructionCx<'a, 'ctrl, 'state> {
    /// Program counter state.
    pub pc: PcMut<'ctrl>,
    /// Gas state.
    pub gas: &'a mut Gas,
    /// Dynamic gas parameters for the active config.
    pub gas_params: &'a GasParams,
    /// Interpreter state.
    pub state: &'a mut State<'state>,
}

/// EVM instruction implementation.
pub trait Instruction<C: EvmConfig = crate::EvmVersion<()>> {
    /// Executes this instruction.
    fn execute(
        &self,
        stack: &mut Stack<'_>,
        pc: PcMut<'_>,
        gas: &mut Gas,
        state: &mut State<'_>,
    ) -> Result;
}

impl GasTable {
    /// Returns the gas cost for `opcode`.
    #[inline]
    pub const fn get(&self, opcode: u8) -> u16 {
        self.0[opcode as usize]
    }

    /// Returns the mutable gas cost slot for `opcode`.
    #[inline]
    pub const fn get_mut(&mut self, opcode: u8) -> &mut u16 {
        &mut self.0[opcode as usize]
    }

    /// Sets the gas cost for `opcode`.
    #[inline]
    pub const fn set(&mut self, opcode: u8, cost: u16) {
        self.0[opcode as usize] = cost;
    }

    /// Creates a gas table for `spec`.
    #[inline]
    pub const fn new_spec(spec: SpecId) -> Self {
        let mut table = Self::default_static();

        if spec.enables(SpecId::TANGERINE) {
            table.set(op::SLOAD, 200);
            table.set(op::BALANCE, 400);
            table.set(op::EXTCODESIZE, 700);
            table.set(op::EXTCODECOPY, 700);
            table.set(op::CALL, 700);
            table.set(op::CALLCODE, 700);
            table.set(op::DELEGATECALL, 700);
            table.set(op::STATICCALL, 700);
            table.set(op::SELFDESTRUCT, 5000);
        }

        if spec.enables(SpecId::ISTANBUL) {
            table.set(op::SLOAD, ISTANBUL_SLOAD_GAS as u16);
            table.set(op::BALANCE, 700);
            table.set(op::EXTCODEHASH, 700);
        }

        if spec.enables(SpecId::BERLIN) {
            table.set(op::SLOAD, WARM_STORAGE_READ_COST as u16);
            table.set(op::BALANCE, WARM_STORAGE_READ_COST as u16);
            table.set(op::EXTCODESIZE, WARM_STORAGE_READ_COST as u16);
            table.set(op::EXTCODEHASH, WARM_STORAGE_READ_COST as u16);
            table.set(op::EXTCODECOPY, WARM_STORAGE_READ_COST as u16);
            table.set(op::CALL, WARM_STORAGE_READ_COST as u16);
            table.set(op::CALLCODE, WARM_STORAGE_READ_COST as u16);
            table.set(op::DELEGATECALL, WARM_STORAGE_READ_COST as u16);
            table.set(op::STATICCALL, WARM_STORAGE_READ_COST as u16);
        }

        table
    }

    /// Creates the default opcode gas table.
    #[inline]
    pub const fn default_static() -> Self {
        let mut table = Self([0; 256]);

        table.set(op::STOP, ZERO as u16);
        table.set(op::ADD, VERYLOW as u16);
        table.set(op::MUL, LOW as u16);
        table.set(op::SUB, VERYLOW as u16);
        table.set(op::DIV, 5);
        table.set(op::SDIV, 5);
        table.set(op::MOD, 5);
        table.set(op::SMOD, 5);
        table.set(op::ADDMOD, MID as u16);
        table.set(op::MULMOD, 8);
        table.set(op::EXP, EXP as u16);
        table.set(op::SIGNEXTEND, 5);

        table.set(op::LT, 3);
        table.set(op::GT, 3);
        table.set(op::SLT, 3);
        table.set(op::SGT, 3);
        table.set(op::EQ, 3);
        table.set(op::ISZERO, 3);
        table.set(op::AND, 3);
        table.set(op::OR, 3);
        table.set(op::XOR, 3);
        table.set(op::NOT, 3);
        table.set(op::BYTE, 3);
        table.set(op::SHL, 3);
        table.set(op::SHR, 3);
        table.set(op::SAR, 3);
        table.set(op::CLZ, 5);

        table.set(op::KECCAK256, KECCAK256 as u16);

        table.set(op::ADDRESS, BASE as u16);
        table.set(op::BALANCE, 20);
        table.set(op::ORIGIN, 2);
        table.set(op::CALLER, 2);
        table.set(op::CALLVALUE, 2);
        table.set(op::CALLDATALOAD, 3);
        table.set(op::CALLDATASIZE, 2);
        table.set(op::CALLDATACOPY, 3);
        table.set(op::CODESIZE, 2);
        table.set(op::CODECOPY, 3);
        table.set(op::GASPRICE, 2);
        table.set(op::EXTCODESIZE, 20);
        table.set(op::EXTCODECOPY, 20);
        table.set(op::RETURNDATASIZE, 2);
        table.set(op::RETURNDATACOPY, 3);
        table.set(op::EXTCODEHASH, 400);
        table.set(op::BLOCKHASH, BLOCKHASH as u16);
        table.set(op::COINBASE, 2);
        table.set(op::TIMESTAMP, 2);
        table.set(op::NUMBER, 2);
        table.set(op::DIFFICULTY, 2);
        table.set(op::GASLIMIT, 2);
        table.set(op::CHAINID, 2);
        table.set(op::SELFBALANCE, 5);
        table.set(op::BASEFEE, 2);
        table.set(op::BLOBHASH, 3);
        table.set(op::BLOBBASEFEE, 2);
        table.set(op::SLOTNUM, 2);

        table.set(op::POP, 2);
        table.set(op::MLOAD, 3);
        table.set(op::MSTORE, 3);
        table.set(op::MSTORE8, 3);
        table.set(op::SLOAD, 50);
        table.set(op::SSTORE, 0);
        table.set(op::JUMP, 8);
        table.set(op::JUMPI, HIGH as u16);
        table.set(op::PC, 2);
        table.set(op::MSIZE, 2);
        table.set(op::GAS, 2);
        table.set(op::JUMPDEST, JUMPDEST as u16);
        table.set(op::TLOAD, 100);
        table.set(op::TSTORE, 100);
        table.set(op::MCOPY, 3);

        table.set(op::PUSH0, 2);
        table.set(op::PUSH1, 3);
        table.set(op::PUSH2, 3);
        table.set(op::PUSH3, 3);
        table.set(op::PUSH4, 3);
        table.set(op::PUSH5, 3);
        table.set(op::PUSH6, 3);
        table.set(op::PUSH7, 3);
        table.set(op::PUSH8, 3);
        table.set(op::PUSH9, 3);
        table.set(op::PUSH10, 3);
        table.set(op::PUSH11, 3);
        table.set(op::PUSH12, 3);
        table.set(op::PUSH13, 3);
        table.set(op::PUSH14, 3);
        table.set(op::PUSH15, 3);
        table.set(op::PUSH16, 3);
        table.set(op::PUSH17, 3);
        table.set(op::PUSH18, 3);
        table.set(op::PUSH19, 3);
        table.set(op::PUSH20, 3);
        table.set(op::PUSH21, 3);
        table.set(op::PUSH22, 3);
        table.set(op::PUSH23, 3);
        table.set(op::PUSH24, 3);
        table.set(op::PUSH25, 3);
        table.set(op::PUSH26, 3);
        table.set(op::PUSH27, 3);
        table.set(op::PUSH28, 3);
        table.set(op::PUSH29, 3);
        table.set(op::PUSH30, 3);
        table.set(op::PUSH31, 3);
        table.set(op::PUSH32, 3);

        table.set(op::DUP1, 3);
        table.set(op::DUP2, 3);
        table.set(op::DUP3, 3);
        table.set(op::DUP4, 3);
        table.set(op::DUP5, 3);
        table.set(op::DUP6, 3);
        table.set(op::DUP7, 3);
        table.set(op::DUP8, 3);
        table.set(op::DUP9, 3);
        table.set(op::DUP10, 3);
        table.set(op::DUP11, 3);
        table.set(op::DUP12, 3);
        table.set(op::DUP13, 3);
        table.set(op::DUP14, 3);
        table.set(op::DUP15, 3);
        table.set(op::DUP16, 3);

        table.set(op::SWAP1, 3);
        table.set(op::SWAP2, 3);
        table.set(op::SWAP3, 3);
        table.set(op::SWAP4, 3);
        table.set(op::SWAP5, 3);
        table.set(op::SWAP6, 3);
        table.set(op::SWAP7, 3);
        table.set(op::SWAP8, 3);
        table.set(op::SWAP9, 3);
        table.set(op::SWAP10, 3);
        table.set(op::SWAP11, 3);
        table.set(op::SWAP12, 3);
        table.set(op::SWAP13, 3);
        table.set(op::SWAP14, 3);
        table.set(op::SWAP15, 3);
        table.set(op::SWAP16, 3);

        table.set(op::DUPN, 3);
        table.set(op::SWAPN, 3);
        table.set(op::EXCHANGE, 3);

        table.set(op::LOG0, LOG as u16);
        table.set(op::LOG1, LOG as u16);
        table.set(op::LOG2, LOG as u16);
        table.set(op::LOG3, LOG as u16);
        table.set(op::LOG4, LOG as u16);

        table.set(op::CREATE, 0);
        table.set(op::CALL, 40);
        table.set(op::CALLCODE, 40);
        table.set(op::RETURN, 0);
        table.set(op::DELEGATECALL, 40);
        table.set(op::CREATE2, 0);
        table.set(op::STATICCALL, 40);
        table.set(op::REVERT, 0);
        table.set(op::INVALID, 0);
        table.set(op::SELFDESTRUCT, 0);

        table
    }
}

macro_rules! make_instruction_table_inner {
    ([$table:expr, $config:ty] $(
        ($op:ident, $instr:path),
    )*) => {
        $(
            $table.set(op::$op, Some(&$instr as &'static dyn Instruction<$config>));
        )*
    };
}

macro_rules! assign_instruction_table_entries {
    ([$table:expr, $config:ty, $dispatch:ident, $instr_fn:ty] $($op:literal,)*) => {
        $(
            $table[$op] = $dispatch::<$config, $op> as $instr_fn;
        )*
    };
}

macro_rules! for_each_opcode_value {
    ([$($extra:tt)*] $m:ident) => {
        $m! { [$($extra)*]
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07,
            0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F,
            0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17,
            0x18, 0x19, 0x1A, 0x1B, 0x1C, 0x1D, 0x1E, 0x1F,
            0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27,
            0x28, 0x29, 0x2A, 0x2B, 0x2C, 0x2D, 0x2E, 0x2F,
            0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37,
            0x38, 0x39, 0x3A, 0x3B, 0x3C, 0x3D, 0x3E, 0x3F,
            0x40, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47,
            0x48, 0x49, 0x4A, 0x4B, 0x4C, 0x4D, 0x4E, 0x4F,
            0x50, 0x51, 0x52, 0x53, 0x54, 0x55, 0x56, 0x57,
            0x58, 0x59, 0x5A, 0x5B, 0x5C, 0x5D, 0x5E, 0x5F,
            0x60, 0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x67,
            0x68, 0x69, 0x6A, 0x6B, 0x6C, 0x6D, 0x6E, 0x6F,
            0x70, 0x71, 0x72, 0x73, 0x74, 0x75, 0x76, 0x77,
            0x78, 0x79, 0x7A, 0x7B, 0x7C, 0x7D, 0x7E, 0x7F,
            0x80, 0x81, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87,
            0x88, 0x89, 0x8A, 0x8B, 0x8C, 0x8D, 0x8E, 0x8F,
            0x90, 0x91, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97,
            0x98, 0x99, 0x9A, 0x9B, 0x9C, 0x9D, 0x9E, 0x9F,
            0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7,
            0xA8, 0xA9, 0xAA, 0xAB, 0xAC, 0xAD, 0xAE, 0xAF,
            0xB0, 0xB1, 0xB2, 0xB3, 0xB4, 0xB5, 0xB6, 0xB7,
            0xB8, 0xB9, 0xBA, 0xBB, 0xBC, 0xBD, 0xBE, 0xBF,
            0xC0, 0xC1, 0xC2, 0xC3, 0xC4, 0xC5, 0xC6, 0xC7,
            0xC8, 0xC9, 0xCA, 0xCB, 0xCC, 0xCD, 0xCE, 0xCF,
            0xD0, 0xD1, 0xD2, 0xD3, 0xD4, 0xD5, 0xD6, 0xD7,
            0xD8, 0xD9, 0xDA, 0xDB, 0xDC, 0xDD, 0xDE, 0xDF,
            0xE0, 0xE1, 0xE2, 0xE3, 0xE4, 0xE5, 0xE6, 0xE7,
            0xE8, 0xE9, 0xEA, 0xEB, 0xEC, 0xED, 0xEE, 0xEF,
            0xF0, 0xF1, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7,
            0xF8, 0xF9, 0xFA, 0xFB, 0xFC, 0xFD, 0xFE, 0xFF,
        }
    };
}

impl<C: EvmConfig> InstructionImplTable<C> {
    /// Creates an instruction implementation table.
    pub const fn new() -> Self {
        use crate::interpreter::instructions::*;

        let mut table = Self([None; 256]);
        for_each_opcode!([table, C] make_instruction_table_inner);
        table
    }
}

/// Converts instruction implementations to a normal instruction dispatch table.
#[inline]
#[cfg(not(feature = "nightly"))]
pub(crate) const fn make_normal_instruction_table<C: EvmConfig>() -> InstructionTable {
    let mut table = [dispatch::<C, 0> as InstructionFn; 256];
    for_each_opcode_value!([table, C, dispatch, InstructionFn] assign_instruction_table_entries);
    table
}

/// Converts instruction implementations to a tail-call instruction dispatch table.
#[inline]
#[cfg(feature = "nightly")]
pub(crate) const fn make_tail_instruction_table<C: EvmConfig>() -> TailInstructionTable {
    let mut table = [tail_dispatch::<C, 0> as TailInstructionFn; 256];
    for_each_opcode_value!([table, C, tail_dispatch, TailInstructionFn] assign_instruction_table_entries);
    table
}

impl<C: EvmConfig> dyn Instruction<C> {
    #[inline(always)]
    pub(crate) fn default_unknown() -> &'static Self {
        &crate::interpreter::instructions::unknown
    }
}

extern_table! {
    #[cfg(not(feature = "nightly"))]
    fn dispatch<C: EvmConfig, const OP: usize>(
        mut stack: Stack<'_>,
        pc: PcMut<'_>,
        gas: &mut Gas,
        state: &mut State<'_>,
    ) -> InstructionFnRet {
        let instr = C::INSTRUCTION_IMPLS.get(OP as u8).unwrap_or_else(<dyn Instruction<C>>::default_unknown);
        let r = instr.execute(&mut stack, pc, gas, state);
        (stack.len, r)
    }
}

extern_table! {
    #[cfg(feature = "nightly")]
    fn tail_dispatch<C: EvmConfig, const OP: usize>(
        mut stack: Stack<'_>,
        mut pc: Pc<'_>,
        gas: &mut Gas,
        state: &mut State<'_>,
        ret: *const (),
    ) -> TailInstructionFnRet {
        let instr = C::INSTRUCTION_IMPLS.get(OP as u8).unwrap_or_else(<dyn Instruction<C>>::default_unknown);
        if let Err(e) = instr.execute(&mut stack, pc.as_mut(), gas, state) {
            cold_path();
            tail_return!(tail_call_restore(
                stack,
                pc,
                gas,
                state,
                e as usize as *const (),
            ));
        }
        tail_return!(tail_call_next::<C>(stack, pc, gas, state, ret));
    }
}

extern_table! {
    #[inline(never)] // TODO: bench inlining this vs having a single dispatcher for all
    #[cfg(feature = "nightly")]
    fn tail_call_next<C: EvmConfig>(
        stack: Stack<'_>,
        mut pc: Pc<'_>,
        gas: &mut Gas,
        state: &mut State<'_>,
        _ret: *const (),
    ) -> TailInstructionFnRet {
        let op = match Interpreter::pre_step::<C>(pc.as_mut(), gas) {
            Ok(op) => op,
            Err(e) => {
                cold_path();
                tail_return!(tail_call_restore(
                    stack,
                    pc,
                    gas,
                    state,
                    e as usize as *const (),
                ));
            }
        };
        let instr = <C as InstructionTables>::TAIL_INSTRUCTIONS[op as usize];
        tail_return!(instr(stack, pc, gas, state, core::ptr::null()));
    }
}

extern_table! {
    #[inline(never)]
    #[cold]
    #[cfg(feature = "nightly")]
    fn tail_call_restore(
        stack: Stack<'_>,
        pc: Pc<'_>,
        _gas: &mut Gas,
        state: &mut State<'_>,
        ret: *const (), // Tail calls require same function signature, this is unused so we pass the return value here.
    ) -> TailInstructionFnRet {
        // SAFETY: `raw_interp` is valid for the duration of execution.
        let interp = unsafe { &mut *state.raw_interp.cast::<Interpreter>() };
        interp.pc = pc.get();
        interp.stack_len = stack.len;
        unsafe { core::mem::transmute::<u8, TailInstructionFnRet>(ret as usize as u8) }
    }
}

#[cfg(test)]
mod tests {
    use super::{GasTable, InstructionImplTable};
    use crate::{
        EvmConfig,
        bytecode::Bytecode,
        env::TxEnv,
        interpreter::{Message, SpecId, Word, instructions::tests::TestHost, op},
    };
    use alloy_primitives::Bytes;
    use evm2_macros::instruction;

    const CUSTOM_OPCODE: u8 = 0x0c;

    #[derive(Debug)]
    struct CustomConfig;

    impl EvmConfig for CustomConfig {
        type Tx = ();

        const SPEC_ID: SpecId = SpecId::OSAKA;
        const INSTRUCTION_IMPLS: InstructionImplTable<Self> = {
            let mut table = InstructionImplTable::new();
            table.set(CUSTOM_OPCODE, Some(&custom));
            table
        };
    }

    #[instruction]
    fn custom() -> out {
        *out = Word::from(0xdead_u64);
    }

    #[test]
    fn default_gas_table_matches_revm_static_costs() {
        let default_gas_table = GasTable::default_static();
        assert_eq!(default_gas_table[op::STOP as usize], 0);
        assert_eq!(default_gas_table[op::ADD as usize], 3);
        assert_eq!(default_gas_table[op::MUL as usize], 5);
        assert_eq!(default_gas_table[op::EXP as usize], 10);
        assert_eq!(default_gas_table[op::BALANCE as usize], 20);
        assert_eq!(default_gas_table[op::SLOAD as usize], 50);
        assert_eq!(default_gas_table[op::CALL as usize], 40);
        assert_eq!(default_gas_table[op::SELFDESTRUCT as usize], 0);
    }

    #[test]
    fn gas_table_applies_spec_static_costs() {
        let tangerine = GasTable::new_spec(SpecId::TANGERINE);
        assert_eq!(tangerine[op::SLOAD as usize], 200);
        assert_eq!(tangerine[op::BALANCE as usize], 400);
        assert_eq!(tangerine[op::SELFDESTRUCT as usize], 5000);

        let istanbul = GasTable::new_spec(SpecId::ISTANBUL);
        assert_eq!(istanbul[op::SLOAD as usize], 800);
        assert_eq!(istanbul[op::EXTCODEHASH as usize], 700);

        let berlin = GasTable::new_spec(SpecId::BERLIN);
        assert_eq!(berlin[op::SLOAD as usize], 100);
        assert_eq!(berlin[op::BALANCE as usize], 100);
        assert_eq!(berlin[op::CALL as usize], 100);
    }

    #[test]
    fn custom_instruction_table_opcode_runs() {
        let bytecode = Bytecode::new_legacy(Bytes::from_static(&[CUSTOM_OPCODE, op::STOP]));
        let mut interpreter = crate::interpreter::Interpreter::new(
            bytecode,
            TxEnv::default(),
            Message { gas_limit: 10_000, ..Message::default() },
        );
        let mut host = TestHost::default();

        let stop = interpreter.run::<CustomConfig>(&mut host);

        core::assert_matches!(stop, crate::interpreter::InstrStop::Stop);
        assert_eq!(interpreter.stack_len, 1);
        assert_eq!(interpreter.stack[0], Word::from(0xdead_u64));
    }
}
