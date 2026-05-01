//! Instruction dispatch tables.

use crate::{
    EvmConfig,
    interpreter::{
        Gas, InstrStop, Interpreter, Pc, PcMut, Result, SpecId, Stack, State,
        gas::{
            BASE, BLOCKHASH, EXP, HIGH, ISTANBUL_SLOAD_GAS, JUMPDEST, KECCAK256, LOG, LOW, MID,
            VERYLOW, WARM_STORAGE_READ_COST, ZERO,
        },
        opcode::{for_each_opcode, op},
    },
};
use core::hint::cold_path;

/// Normal instruction dispatch table.
pub(crate) type InstrTable<C> = InstructionImplTable<C>;

/// Tail instruction dispatch table.
pub(crate) type TailInstrTable<C> = [TailInstr<C>; 256];

/// Tail instruction return value.
pub(crate) type TailInstrFnRet = InstrStop;

/// Tail instruction function pointer.
pub(crate) type TailInstrFn<C> = extern_table!(
    fn(
        stack: Stack<'_>,
        pc: Pc<'_>,
        gas: &mut Gas,
        state: &mut State<'_>,
        gas_table: &GasTable,
        instr: Option<&'static dyn Instruction<C>>,
        instr_tablep: *const (),
    ) -> TailInstrFnRet
);

/// Tail instruction table entry.
pub(crate) struct TailInstr<C: EvmConfig> {
    /// Tail dispatch function.
    pub f: TailInstrFn<C>,
    /// Instruction implementation.
    pub instr: Option<&'static dyn Instruction<C>>,
}

impl<C: EvmConfig> Clone for TailInstr<C> {
    #[inline]
    fn clone(&self) -> Self {
        *self
    }
}

impl<C: EvmConfig> Copy for TailInstr<C> {}

/// Opcode gas table.
pub type GasTable = [u16; 256];

/// Instruction implementation table.
pub type InstructionImplTable<C> = [Option<&'static dyn Instruction<C>>; 256];

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

/// Creates an instruction implementation table.
pub(crate) const fn make_instruction_table<C: EvmConfig>() -> InstructionImplTable<C> {
    use crate::interpreter::instructions::*;

    let mut table = [None; 256];
    for_each_opcode!([table, C] make_instruction_table_inner);
    table
}

/// Creates a tail-call instruction dispatch table.
#[inline]
pub(crate) const fn make_tail_table<C: EvmConfig>(
    impls: InstructionImplTable<C>,
) -> TailInstrTable<C> {
    let mut table = [TailInstr { f: tail_dispatch::<C> as TailInstrFn<C>, instr: None }; 256];
    let mut i = 0;
    while i < table.len() {
        table[i].instr = impls[i];
        i += 1;
    }
    table
}

impl<C: EvmConfig> dyn Instruction<C> {
    #[inline(always)]
    pub(crate) fn default_unknown() -> &'static Self {
        &crate::interpreter::instructions::unknown
    }
}

extern_table! {
    fn tail_dispatch<C: EvmConfig>(
        mut stack: Stack<'_>,
        mut pc: Pc<'_>,
        gas: &mut Gas,
        state: &mut State<'_>,
        gast: &GasTable,
        instr: Option<&'static dyn Instruction<C>>,
        instrsp: *const (),
    ) -> TailInstrFnRet {
        let instr = instr.unwrap_or_else(<dyn Instruction<C>>::default_unknown);
        if let Err(e) = instr.execute(&mut stack, pc.as_mut(), gas, state) {
            cold_path();
            tail_return!(tail_call_restore::<C>(
                stack,
                pc,
                gas,
                state,
                gast,
                Some(instr),
                e as usize as *const (),
            ));
        }
        tail_return!(tail_call_next::<C>(stack, pc, gas, state, gast, Some(instr), instrsp));
    }
}

extern_table! {
    #[inline(never)] // TODO: bench inlining this vs having a single dispatcher for all
    fn tail_call_next<C: EvmConfig>(
        stack: Stack<'_>,
        mut pc: Pc<'_>,
        gas: &mut Gas,
        state: &mut State<'_>,
        gast: &GasTable,
        _instr: Option<&'static dyn Instruction<C>>,
        instrsp: *const (),
    ) -> TailInstrFnRet {
        let op = match Interpreter::<C>::pre_step(pc.as_mut(), gas, gast) {
            Ok(op) => op,
            Err(e) => {
                cold_path();
                tail_return!(tail_call_restore::<C>(
                    stack,
                    pc,
                    gas,
                    state,
                    gast,
                    None,
                    e as usize as *const (),
                ));
            }
        };
        // SAFETY: Restoring type-erased table pointer. See [`TailInstrFn`].
        let instrs = unsafe { &*instrsp.cast::<TailInstrTable<C>>() };
        let instr = instrs[op as usize];
        tail_return!((instr.f)(stack, pc, gas, state, gast, instr.instr, instrsp));
    }
}

extern_table! {
    #[inline(never)]
    #[cold]
    fn tail_call_restore<C: EvmConfig>(
        stack: Stack<'_>,
        pc: Pc<'_>,
        _gas: &mut Gas,
        state: &mut State<'_>,
        _gast: &GasTable,
        _instr: Option<&'static dyn Instruction<C>>,
        ret: *const (), // Tail calls require same function signature, this is unused so we pass the return value here.
    ) -> TailInstrFnRet {
        // SAFETY: `raw_interp` is valid for the duration of execution.
        let interp = unsafe { &mut *state.raw_interp.cast::<Interpreter<C>>() };
        interp.pc = pc.get();
        interp.stack_len = stack.len;
        unsafe { core::mem::transmute::<u8, TailInstrFnRet>(ret as usize as u8) }
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
