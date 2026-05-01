//! Instruction dispatch tables.

#[cfg(feature = "nightly")]
use crate::interpreter::{InstrStop, Interpreter, Pc};
use crate::{
    EvmConfig,
    interpreter::{
        Gas, PcMut, Result, SpecId, Stack, State,
        gas::{
            BASE, BLOCKHASH, EXP, HIGH, ISTANBUL_SLOAD_GAS, JUMPDEST, KECCAK256, LOG, LOW, MID,
            VERYLOW, WARM_STORAGE_READ_COST, ZERO,
        },
        opcode::{for_each_opcode, op},
    },
};
#[cfg(feature = "nightly")]
use core::hint::cold_path;
use core::marker::PhantomData;

/// Opcode gas table.
pub type GasTable = [u16; 256];

/// Instruction implementation table.
pub type InstructionImplTable<C> = [Option<&'static dyn Instruction<C>>; 256];

/// Normal instruction return value.
#[cfg(not(feature = "nightly"))]
pub(crate) type InstructionFnRet = (usize, Result);

/// Normal instruction function pointer.
#[cfg(not(feature = "nightly"))]
pub(crate) type InstructionFn = extern_table!(
    fn(stack: Stack<'_>, pc: PcMut<'_>, gas: &mut Gas, state: &mut State<'_>) -> InstructionFnRet
);

/// Normal instruction table entry.
#[cfg(not(feature = "nightly"))]
pub(crate) struct InstructionEntry<C: EvmConfig> {
    /// Dispatch function.
    pub(crate) f: InstructionFn,
    _marker: PhantomData<fn() -> C>,
}

#[cfg(not(feature = "nightly"))]
impl<C: EvmConfig> core::fmt::Debug for InstructionEntry<C> {
    #[inline]
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("InstructionEntry").finish_non_exhaustive()
    }
}

#[cfg(not(feature = "nightly"))]
impl<C: EvmConfig> Clone for InstructionEntry<C> {
    #[inline]
    fn clone(&self) -> Self {
        *self
    }
}

#[cfg(not(feature = "nightly"))]
impl<C: EvmConfig> Copy for InstructionEntry<C> {}

/// Normal instruction dispatch table.
#[cfg(not(feature = "nightly"))]
pub(crate) type InstructionTable<C> = [InstructionEntry<C>; 256];

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
        instr_tablep: *const (),
        ret: *const (),
    ) -> TailInstructionFnRet
);

/// Tail instruction table entry.
#[cfg(feature = "nightly")]
pub(crate) struct TailInstructionEntry<C: EvmConfig> {
    /// Tail dispatch function.
    pub(crate) f: TailInstructionFn,
    _marker: PhantomData<fn() -> C>,
}

#[cfg(feature = "nightly")]
impl<C: EvmConfig> core::fmt::Debug for TailInstructionEntry<C> {
    #[inline]
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("TailInstructionEntry").finish_non_exhaustive()
    }
}

#[cfg(feature = "nightly")]
impl<C: EvmConfig> Clone for TailInstructionEntry<C> {
    #[inline]
    fn clone(&self) -> Self {
        *self
    }
}

#[cfg(feature = "nightly")]
impl<C: EvmConfig> Copy for TailInstructionEntry<C> {}

/// Tail instruction dispatch table.
#[cfg(feature = "nightly")]
pub(crate) type TailInstructionTable<C> = [TailInstructionEntry<C>; 256];

/// Instruction execution context.
#[derive(Debug)]
pub(crate) struct InstructionCx<'a, 'ctrl, 'state> {
    /// Program counter state.
    pub pc: PcMut<'ctrl>,
    /// Gas state.
    pub gas: &'a mut Gas,
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

/// Creates a gas table for `spec`.
#[inline]
pub(crate) const fn new_gas_table(spec: SpecId) -> GasTable {
    let mut table = make_gas_table();

    if spec.enables(SpecId::TANGERINE) {
        table[op::SLOAD as usize] = 200;
        table[op::BALANCE as usize] = 400;
        table[op::EXTCODESIZE as usize] = 700;
        table[op::EXTCODECOPY as usize] = 700;
        table[op::CALL as usize] = 700;
        table[op::CALLCODE as usize] = 700;
        table[op::DELEGATECALL as usize] = 700;
        table[op::STATICCALL as usize] = 700;
        table[op::SELFDESTRUCT as usize] = 5000;
    }

    if spec.enables(SpecId::ISTANBUL) {
        table[op::SLOAD as usize] = ISTANBUL_SLOAD_GAS as u16;
        table[op::BALANCE as usize] = 700;
        table[op::EXTCODEHASH as usize] = 700;
    }

    if spec.enables(SpecId::BERLIN) {
        table[op::SLOAD as usize] = WARM_STORAGE_READ_COST as u16;
        table[op::BALANCE as usize] = WARM_STORAGE_READ_COST as u16;
        table[op::EXTCODESIZE as usize] = WARM_STORAGE_READ_COST as u16;
        table[op::EXTCODEHASH as usize] = WARM_STORAGE_READ_COST as u16;
        table[op::EXTCODECOPY as usize] = WARM_STORAGE_READ_COST as u16;
        table[op::CALL as usize] = WARM_STORAGE_READ_COST as u16;
        table[op::CALLCODE as usize] = WARM_STORAGE_READ_COST as u16;
        table[op::DELEGATECALL as usize] = WARM_STORAGE_READ_COST as u16;
        table[op::STATICCALL as usize] = WARM_STORAGE_READ_COST as u16;
    }

    table
}

/// Creates the default opcode gas table.
#[inline]
pub(crate) const fn make_gas_table() -> GasTable {
    let mut table = [0; 256];

    table[op::STOP as usize] = ZERO as u16;
    table[op::ADD as usize] = VERYLOW as u16;
    table[op::MUL as usize] = LOW as u16;
    table[op::SUB as usize] = VERYLOW as u16;
    table[op::DIV as usize] = 5;
    table[op::SDIV as usize] = 5;
    table[op::MOD as usize] = 5;
    table[op::SMOD as usize] = 5;
    table[op::ADDMOD as usize] = MID as u16;
    table[op::MULMOD as usize] = 8;
    table[op::EXP as usize] = EXP as u16;
    table[op::SIGNEXTEND as usize] = 5;

    table[op::LT as usize] = 3;
    table[op::GT as usize] = 3;
    table[op::SLT as usize] = 3;
    table[op::SGT as usize] = 3;
    table[op::EQ as usize] = 3;
    table[op::ISZERO as usize] = 3;
    table[op::AND as usize] = 3;
    table[op::OR as usize] = 3;
    table[op::XOR as usize] = 3;
    table[op::NOT as usize] = 3;
    table[op::BYTE as usize] = 3;
    table[op::SHL as usize] = 3;
    table[op::SHR as usize] = 3;
    table[op::SAR as usize] = 3;
    table[op::CLZ as usize] = 5;

    table[op::KECCAK256 as usize] = KECCAK256 as u16;

    table[op::ADDRESS as usize] = BASE as u16;
    table[op::BALANCE as usize] = 20;
    table[op::ORIGIN as usize] = 2;
    table[op::CALLER as usize] = 2;
    table[op::CALLVALUE as usize] = 2;
    table[op::CALLDATALOAD as usize] = 3;
    table[op::CALLDATASIZE as usize] = 2;
    table[op::CALLDATACOPY as usize] = 3;
    table[op::CODESIZE as usize] = 2;
    table[op::CODECOPY as usize] = 3;
    table[op::GASPRICE as usize] = 2;
    table[op::EXTCODESIZE as usize] = 20;
    table[op::EXTCODECOPY as usize] = 20;
    table[op::RETURNDATASIZE as usize] = 2;
    table[op::RETURNDATACOPY as usize] = 3;
    table[op::EXTCODEHASH as usize] = 400;
    table[op::BLOCKHASH as usize] = BLOCKHASH as u16;
    table[op::COINBASE as usize] = 2;
    table[op::TIMESTAMP as usize] = 2;
    table[op::NUMBER as usize] = 2;
    table[op::DIFFICULTY as usize] = 2;
    table[op::GASLIMIT as usize] = 2;
    table[op::CHAINID as usize] = 2;
    table[op::SELFBALANCE as usize] = 5;
    table[op::BASEFEE as usize] = 2;
    table[op::BLOBHASH as usize] = 3;
    table[op::BLOBBASEFEE as usize] = 2;
    table[op::SLOTNUM as usize] = 2;

    table[op::POP as usize] = 2;
    table[op::MLOAD as usize] = 3;
    table[op::MSTORE as usize] = 3;
    table[op::MSTORE8 as usize] = 3;
    table[op::SLOAD as usize] = 50;
    table[op::SSTORE as usize] = 0;
    table[op::JUMP as usize] = 8;
    table[op::JUMPI as usize] = HIGH as u16;
    table[op::PC as usize] = 2;
    table[op::MSIZE as usize] = 2;
    table[op::GAS as usize] = 2;
    table[op::JUMPDEST as usize] = JUMPDEST as u16;
    table[op::TLOAD as usize] = 100;
    table[op::TSTORE as usize] = 100;
    table[op::MCOPY as usize] = 3;

    table[op::PUSH0 as usize] = 2;
    table[op::PUSH1 as usize] = 3;
    table[op::PUSH2 as usize] = 3;
    table[op::PUSH3 as usize] = 3;
    table[op::PUSH4 as usize] = 3;
    table[op::PUSH5 as usize] = 3;
    table[op::PUSH6 as usize] = 3;
    table[op::PUSH7 as usize] = 3;
    table[op::PUSH8 as usize] = 3;
    table[op::PUSH9 as usize] = 3;
    table[op::PUSH10 as usize] = 3;
    table[op::PUSH11 as usize] = 3;
    table[op::PUSH12 as usize] = 3;
    table[op::PUSH13 as usize] = 3;
    table[op::PUSH14 as usize] = 3;
    table[op::PUSH15 as usize] = 3;
    table[op::PUSH16 as usize] = 3;
    table[op::PUSH17 as usize] = 3;
    table[op::PUSH18 as usize] = 3;
    table[op::PUSH19 as usize] = 3;
    table[op::PUSH20 as usize] = 3;
    table[op::PUSH21 as usize] = 3;
    table[op::PUSH22 as usize] = 3;
    table[op::PUSH23 as usize] = 3;
    table[op::PUSH24 as usize] = 3;
    table[op::PUSH25 as usize] = 3;
    table[op::PUSH26 as usize] = 3;
    table[op::PUSH27 as usize] = 3;
    table[op::PUSH28 as usize] = 3;
    table[op::PUSH29 as usize] = 3;
    table[op::PUSH30 as usize] = 3;
    table[op::PUSH31 as usize] = 3;
    table[op::PUSH32 as usize] = 3;

    table[op::DUP1 as usize] = 3;
    table[op::DUP2 as usize] = 3;
    table[op::DUP3 as usize] = 3;
    table[op::DUP4 as usize] = 3;
    table[op::DUP5 as usize] = 3;
    table[op::DUP6 as usize] = 3;
    table[op::DUP7 as usize] = 3;
    table[op::DUP8 as usize] = 3;
    table[op::DUP9 as usize] = 3;
    table[op::DUP10 as usize] = 3;
    table[op::DUP11 as usize] = 3;
    table[op::DUP12 as usize] = 3;
    table[op::DUP13 as usize] = 3;
    table[op::DUP14 as usize] = 3;
    table[op::DUP15 as usize] = 3;
    table[op::DUP16 as usize] = 3;

    table[op::SWAP1 as usize] = 3;
    table[op::SWAP2 as usize] = 3;
    table[op::SWAP3 as usize] = 3;
    table[op::SWAP4 as usize] = 3;
    table[op::SWAP5 as usize] = 3;
    table[op::SWAP6 as usize] = 3;
    table[op::SWAP7 as usize] = 3;
    table[op::SWAP8 as usize] = 3;
    table[op::SWAP9 as usize] = 3;
    table[op::SWAP10 as usize] = 3;
    table[op::SWAP11 as usize] = 3;
    table[op::SWAP12 as usize] = 3;
    table[op::SWAP13 as usize] = 3;
    table[op::SWAP14 as usize] = 3;
    table[op::SWAP15 as usize] = 3;
    table[op::SWAP16 as usize] = 3;

    table[op::DUPN as usize] = 3;
    table[op::SWAPN as usize] = 3;
    table[op::EXCHANGE as usize] = 3;

    table[op::LOG0 as usize] = LOG as u16;
    table[op::LOG1 as usize] = LOG as u16;
    table[op::LOG2 as usize] = LOG as u16;
    table[op::LOG3 as usize] = LOG as u16;
    table[op::LOG4 as usize] = LOG as u16;

    table[op::CREATE as usize] = 0;
    table[op::CALL as usize] = 40;
    table[op::CALLCODE as usize] = 40;
    table[op::RETURN as usize] = 0;
    table[op::DELEGATECALL as usize] = 40;
    table[op::CREATE2 as usize] = 0;
    table[op::STATICCALL as usize] = 40;
    table[op::REVERT as usize] = 0;
    table[op::INVALID as usize] = 0;
    table[op::SELFDESTRUCT as usize] = 0;

    table
}

macro_rules! make_instruction_table_inner {
    ([$table:expr, $config:ty] $(
        ($op:ident, $instr:path),
    )*) => {
        $(
            $table[op::$op as usize] = Some(&$instr as &'static dyn Instruction<$config>);
        )*
    };
}

macro_rules! assign_instruction_table_entries {
    ([$table:expr, $config:ty, $entry:ident, $dispatch:ident, $instr_fn:ty] $($op:literal,)*) => {
        $(
            $table[$op] = $entry { f: $dispatch::<$config, $op> as $instr_fn, _marker: PhantomData };
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

/// Creates an instruction implementation table.
pub(crate) const fn make_instruction_table<C: EvmConfig>() -> InstructionImplTable<C> {
    use crate::interpreter::instructions::*;

    let mut table = [None; 256];
    for_each_opcode!([table, C] make_instruction_table_inner);
    table
}

/// Converts instruction implementations to a normal instruction dispatch table.
#[inline]
#[cfg(not(feature = "nightly"))]
pub(crate) const fn make_normal_instruction_table<C: EvmConfig>() -> InstructionTable<C> {
    let mut table =
        [InstructionEntry { f: dispatch::<C, 0> as InstructionFn, _marker: PhantomData }; 256];
    for_each_opcode_value!([table, C, InstructionEntry, dispatch, InstructionFn] assign_instruction_table_entries);
    table
}

/// Converts instruction implementations to a tail-call instruction dispatch table.
#[inline]
#[cfg(feature = "nightly")]
pub(crate) const fn make_tail_instruction_table<C: EvmConfig>() -> TailInstructionTable<C> {
    let mut table = [TailInstructionEntry {
        f: tail_dispatch::<C, 0> as TailInstructionFn,
        _marker: PhantomData,
    }; 256];
    for_each_opcode_value!([table, C, TailInstructionEntry, tail_dispatch, TailInstructionFn] assign_instruction_table_entries);
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
        let instr = C::INSTRUCTION_IMPLS[OP].unwrap_or_else(<dyn Instruction<C>>::default_unknown);
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
        instrsp: *const (),
        ret: *const (),
    ) -> TailInstructionFnRet {
        let instr = C::INSTRUCTION_IMPLS[OP].unwrap_or_else(<dyn Instruction<C>>::default_unknown);
        if let Err(e) = instr.execute(&mut stack, pc.as_mut(), gas, state) {
            cold_path();
            tail_return!(tail_call_restore(
                stack,
                pc,
                gas,
                state,
                instrsp,
                e as usize as *const (),
            ));
        }
        tail_return!(tail_call_next::<C>(stack, pc, gas, state, instrsp, ret));
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
        instrsp: *const (),
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
                    instrsp,
                    e as usize as *const (),
                ));
            }
        };
        // SAFETY: Restoring type-erased table pointer. See [`TailInstructionFn`].
        let instrs = unsafe { &*instrsp.cast::<TailInstructionTable<C>>() };
        let instr = instrs[op as usize];
        tail_return!((instr.f)(stack, pc, gas, state, instrsp, core::ptr::null()));
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
        _instrsp: *const (),
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
    use super::{make_gas_table, new_gas_table};
    use crate::interpreter::{SpecId, op};

    #[test]
    fn default_gas_table_matches_revm_static_costs() {
        let default_gas_table = make_gas_table();
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
        let tangerine = new_gas_table(SpecId::TANGERINE);
        assert_eq!(tangerine[op::SLOAD as usize], 200);
        assert_eq!(tangerine[op::BALANCE as usize], 400);
        assert_eq!(tangerine[op::SELFDESTRUCT as usize], 5000);

        let istanbul = new_gas_table(SpecId::ISTANBUL);
        assert_eq!(istanbul[op::SLOAD as usize], 800);
        assert_eq!(istanbul[op::EXTCODEHASH as usize], 700);

        let berlin = new_gas_table(SpecId::BERLIN);
        assert_eq!(berlin[op::SLOAD as usize], 100);
        assert_eq!(berlin[op::BALANCE as usize], 100);
        assert_eq!(berlin[op::CALL as usize], 100);
    }
}
