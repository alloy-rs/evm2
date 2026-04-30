use super::*;
use crate::interpreter::{
    Ctrl, CtrlRef, Gas, GasRef, InstrErr, Interpreter, Result, SpecId, Stack, State,
    opcode::{for_each_opcode, op},
};
use core::mem;

/// Normal instruction return value.
pub type InstrFnRet = (usize, Result);
/// Normal instruction function pointer.
pub type InstrFn = extern_table!(
    fn(ctrl: CtrlRef<'_>, stack: Stack<'_>, gas: GasRef<'_>, state: &mut State<'_>) -> InstrFnRet
);
/// Normal instruction dispatch table.
pub type InstrTable = [InstrFn; 256];

/// Tail instruction return value.
pub type TailInstrFnRet = InstrErr;
/// Tail instruction function pointer.
pub type TailInstrFn = extern_table!(
    fn(
        ctrl: Ctrl<'_>,
        stack: Stack<'_>,
        gas: Gas,
        state: &mut State<'_>,
        gas_table: &GasTable,
        instr_tablep: *const (),
    ) -> TailInstrFnRet
);
/// Tail instruction dispatch table.
pub type TailInstrTable = [TailInstrFn; 256];

/// Opcode gas table.
pub type GasTable = [u16; 256];

/// Instruction execution context.
#[allow(dead_code)]
#[derive(Debug)]
pub struct InstructionCx<'a, 'ctrl, 'state> {
    /// Bytecode control reference.
    pub ctrl: &'a mut CtrlRef<'ctrl>,
    /// Gas state.
    pub gas: GasRef<'a>,
    /// Interpreter state.
    pub state: &'a mut State<'state>,
}

/// Default normal dispatch table.
pub static DEFAULT_TABLE: InstrTable = make_table();
/// Default tail dispatch table.
pub static DEFAULT_TAIL_TABLE: TailInstrTable = make_tail_table();

/// Default opcode gas table.
pub static DEFAULT_GAS_TABLE: GasTable = [3; 256];

pub(crate) trait Instruction {
    fn new() -> Self;
    fn execute(
        self,
        ctrl: CtrlRef<'_>,
        stack: &mut Stack<'_>,
        gas: &mut Gas,
        state: &mut State<'_>,
    ) -> Result;
}

impl<F: FnOnce(CtrlRef<'_>, &mut Stack<'_>, &mut Gas, &mut State<'_>) -> Result> Instruction for F {
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
        ctrl: CtrlRef<'_>,
        stack: &mut Stack<'_>,
        gas: &mut Gas,
        state: &mut State<'_>,
    ) -> Result {
        self(ctrl, stack, gas, state)
    }
}

/// Creates a gas table for `spec`.
pub fn new_gas_table(spec: SpecId) -> GasTable {
    let mut t = DEFAULT_GAS_TABLE;
    if spec >= SpecId::Homestead {
        t[op::ADD as usize] = 69;
    }
    t
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
pub const fn make_table() -> InstrTable {
    make_table_m!(mk_dispatch)
}

pub(crate) const fn mk_dispatch<I: Instruction>(f: I) -> InstrFn {
    mem::forget(f);
    dispatch::<I>
}

/// Creates the tail instruction dispatch table.
pub const fn make_tail_table() -> TailInstrTable {
    make_table_m!(mk_tail_dispatch)
}

pub(crate) const fn mk_tail_dispatch<I: Instruction>(f: I) -> TailInstrFn {
    mem::forget(f);
    tail_dispatch::<I>
}

extern_table! {
    fn dispatch<I: Instruction>(
        ctrl: CtrlRef<'_>,
        mut stack: Stack<'_>,
        gas: GasRef<'_>,
        state: &mut State<'_>,
    ) -> InstrFnRet {
        let r = I::new().execute(ctrl, &mut stack, gas, state);
        (stack.len, r)
    }
}

extern_table! {
    fn tail_dispatch<I: Instruction>(
        mut ctrl: Ctrl<'_>,
        mut stack: Stack<'_>,
        mut gas: Gas,
        state: &mut State<'_>,
        gast: &GasTable,
        instrsp: *const (),
    ) -> TailInstrFnRet {
        if let Err(e) = I::new().execute(ctrl.as_mut(), &mut stack, &mut gas, state) {
            tail_return!(tail_call_restore(ctrl, stack, gas, state, gast, e as usize as *const ()));
        }
        tail_return!(tail_call_next(ctrl, stack, gas, state, gast, instrsp));
    }
}

extern_table! {
    #[inline(never)] // TODO: bench inlining this vs having a single dispatcher for all
    fn tail_call_next(
        mut ctrl: Ctrl<'_>,
        stack: Stack<'_>,
        mut gas: Gas,
        state: &mut State<'_>,
        gast: &GasTable,
        instrsp: *const (),
    ) -> TailInstrFnRet {
        let op = match Interpreter::pre_step(ctrl.as_mut(), &mut gas, gast) {
            Ok(op) => op,
            Err(e) => {
                tail_return!(tail_call_restore(ctrl, stack, gas, state, gast, e as usize as *const ()));
            }
        };
        // SAFETY: Restoring type-erased table pointer. See [`TailInstrFn`].
        let instrs = unsafe { &*instrsp.cast::<TailInstrTable>() };
        tail_return!(instrs[op as usize](ctrl, stack, gas, state, gast, instrsp));
    }
}

extern_table! {
    #[inline(never)]
    #[cold]
    fn tail_call_restore(
        ctrl: Ctrl<'_>,
        stack: Stack<'_>,
        gas: Gas,
        state: &mut State<'_>,
        _gast: &GasTable,
        ret: *const (), // Tail calls require same function signature, this is unused so we pass the return value here.
    ) -> TailInstrFnRet {
        // SAFETY: `raw_interp` is valid for the duration of execution.
        let interp = unsafe { &mut *state.raw_interp };
        interp.pc = ctrl.pc;
        interp.gas = gas;
        interp.stack_len = stack.len;
        unsafe { core::mem::transmute::<u8, TailInstrFnRet>(ret as usize as u8) }
    }
}
