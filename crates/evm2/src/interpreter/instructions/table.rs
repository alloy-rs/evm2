//! Instruction dispatch tables.

use super::*;
use crate::interpreter::{
    Gas, InstrErr, Interpreter, Pc, PcMut, Result, SpecId, Stack, State,
    gas::{
        BASE, BLOCKHASH, EXP, HIGH, ISTANBUL_SLOAD_GAS, JUMPDEST, KECCAK256, LOG, LOW, MID,
        VERYLOW, WARM_STORAGE_READ_COST, ZERO,
    },
    opcode::{for_each_opcode, op},
};
use core::mem;

/// Normal instruction return value.
pub(in crate::interpreter) type InstrFnRet = (usize, Result);
/// Normal instruction function pointer.
pub(in crate::interpreter) type InstrFn = extern_table!(
    fn(stack: Stack<'_>, pc: &mut PcMut<'_>, gas: &mut Gas, state: &mut State<'_>) -> InstrFnRet
);
/// Normal instruction dispatch table.
pub(in crate::interpreter) type InstrTable = [InstrFn; 256];

/// Tail instruction return value.
pub(in crate::interpreter) type TailInstrFnRet = InstrErr;
/// Tail instruction function pointer.
pub(in crate::interpreter) type TailInstrFn = extern_table!(
    fn(
        stack: Stack<'_>,
        pc: Pc<'_>,
        gas: Gas,
        state: &mut State<'_>,
        gas_table: &GasTable,
        instr_tablep: *const (),
    ) -> TailInstrFnRet
);
/// Tail instruction dispatch table.
pub(in crate::interpreter) type TailInstrTable = [TailInstrFn; 256];

/// Opcode gas table.
pub(in crate::interpreter) type GasTable = [u16; 256];

/// Instruction execution context.
#[derive(Debug)]
pub(in crate::interpreter) struct InstructionCx<'a, 'ctrl, 'state> {
    /// Program counter state.
    pub pc: &'a mut PcMut<'ctrl>,
    /// Gas state.
    pub gas: &'a mut Gas,
    /// Interpreter state.
    pub state: &'a mut State<'state>,
}

/// Default normal dispatch table.
pub(in crate::interpreter) static DEFAULT_TABLE: InstrTable = make_table();
/// Default tail dispatch table.
pub(in crate::interpreter) static DEFAULT_TAIL_TABLE: TailInstrTable = make_tail_table();

pub(crate) trait Instruction {
    fn new() -> Self;
    fn execute(
        self,
        stack: &mut Stack<'_>,
        pc: &mut PcMut<'_>,
        gas: &mut Gas,
        state: &mut State<'_>,
    ) -> Result;
}

impl<F: FnOnce(&mut Stack<'_>, &mut PcMut<'_>, &mut Gas, &mut State<'_>) -> Result> Instruction
    for F
{
    #[inline(always)]
    fn new() -> Self {
        const {
            assert!(core::mem::size_of::<Self>() == 0);
            unsafe { core::mem::zeroed::<Self>() }
        }
    }

    #[inline(always)]
    fn execute(
        self,
        stack: &mut Stack<'_>,
        pc: &mut PcMut<'_>,
        gas: &mut Gas,
        state: &mut State<'_>,
    ) -> Result {
        self(stack, pc, gas, state)
    }
}

/// Creates a gas table for `spec`.
#[inline]
pub(in crate::interpreter) const fn new_gas_table(spec: SpecId) -> GasTable {
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

macro_rules! make_table_inner {
    ([$table:expr, $mk_dispatch:expr] $(
        ($op:ident, $fn:expr),
    )*) => {
        $(
            $table[op::$op as usize] = $mk_dispatch($fn);
        )*
    };
}
macro_rules! make_table_m {
    ($mk_dispatch:expr) => {{
        let mut table = [$mk_dispatch(invalid); 256];
        for_each_opcode!([table, $mk_dispatch] make_table_inner);
        table
    }};
}

/// Creates the normal instruction dispatch table.
pub(in crate::interpreter) const fn make_table() -> InstrTable {
    make_table_m!(mk_dispatch)
}

pub(crate) const fn mk_dispatch<I: Instruction>(f: I) -> InstrFn {
    mem::forget(f);
    dispatch::<I>
}

/// Creates the tail instruction dispatch table.
pub(in crate::interpreter) const fn make_tail_table() -> TailInstrTable {
    make_table_m!(mk_tail_dispatch)
}

pub(crate) const fn mk_tail_dispatch<I: Instruction>(f: I) -> TailInstrFn {
    mem::forget(f);
    tail_dispatch::<I>
}

extern_table! {
    fn dispatch<I: Instruction>(
        mut stack: Stack<'_>,
        pc: &mut PcMut<'_>,
        gas: &mut Gas,
        state: &mut State<'_>,
    ) -> InstrFnRet {
        let r = I::new().execute(&mut stack, pc, gas, state);
        (stack.len, r)
    }
}

extern_table! {
    fn tail_dispatch<I: Instruction>(
        mut stack: Stack<'_>,
        mut pc: Pc<'_>,
        mut gas: Gas,
        state: &mut State<'_>,
        gast: &GasTable,
        instrsp: *const (),
    ) -> TailInstrFnRet {
        let mut pc_mut = pc.as_mut();
        if let Err(e) = I::new().execute(&mut stack, &mut pc_mut, &mut gas, state) {
            tail_return!(tail_call_restore(stack, pc, gas, state, gast, e as usize as *const ()));
        }
        tail_return!(tail_call_next(stack, pc, gas, state, gast, instrsp));
    }
}

extern_table! {
    #[inline(never)] // TODO: bench inlining this vs having a single dispatcher for all
    fn tail_call_next(
        stack: Stack<'_>,
        mut pc: Pc<'_>,
        mut gas: Gas,
        state: &mut State<'_>,
        gast: &GasTable,
        instrsp: *const (),
    ) -> TailInstrFnRet {
        let mut pc_mut = pc.as_mut();
        let op = match Interpreter::pre_step(&mut pc_mut, &mut gas, gast) {
            Ok(op) => op,
            Err(e) => {
                tail_return!(tail_call_restore(stack, pc, gas, state, gast, e as usize as *const ()));
            }
        };
        // SAFETY: Restoring type-erased table pointer. See [`TailInstrFn`].
        let instrs = unsafe { &*instrsp.cast::<TailInstrTable>() };
        tail_return!(instrs[op as usize](stack, pc, gas, state, gast, instrsp));
    }
}

extern_table! {
    #[inline(never)]
    #[cold]
    fn tail_call_restore(
        stack: Stack<'_>,
        pc: Pc<'_>,
        gas: Gas,
        state: &mut State<'_>,
        _gast: &GasTable,
        ret: *const (), // Tail calls require same function signature, this is unused so we pass the return value here.
    ) -> TailInstrFnRet {
        // SAFETY: `raw_interp` is valid for the duration of execution.
        let interp = unsafe { &mut *state.raw_interp };
        interp.pc = pc.get();
        interp.gas = gas;
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
