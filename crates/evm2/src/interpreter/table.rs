use core::mem;

use super::{
    Gas, GasRef, Interpreter, Pc, PcRef, SpecId, Stack, State,
    instruction::{
        GasTable, InstrFn, InstrFnRet, InstrTable, Instruction, TailInstrFn, TailInstrFnRet,
        TailInstrTable,
    },
    instructions::{add, balance, invalid, push, stop},
    opcode::{for_each_opcode, op},
};

pub static DEFAULT_TABLE: InstrTable = make_table();
pub static DEFAULT_TAIL_TABLE: TailInstrTable = make_tail_table();

pub static DEFAULT_GAS_TABLE: GasTable = [3; 256];

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

pub const fn make_table() -> InstrTable {
    make_table_m!(mk_dispatch)
}

pub const fn mk_dispatch<I: Instruction<T>, T>(f: I) -> InstrFn {
    mem::forget(f);
    dispatch::<I, T>
}

pub const fn make_tail_table() -> TailInstrTable {
    make_table_m!(mk_tail_dispatch)
}

pub const fn mk_tail_dispatch<I: Instruction<T>, T>(f: I) -> TailInstrFn {
    mem::forget(f);
    tail_dispatch::<I, T>
}

extern_table! {
    fn dispatch<I: Instruction<T>, T>(
        pc: PcRef<'_>,
        mut stack: Stack<'_>,
        gas: GasRef<'_>,
        state: &mut State,
    ) -> InstrFnRet {
        let r = I::new().execute(pc, &mut stack, gas, state);
        (stack.len, r)
    }
}

extern_table! {
    fn tail_dispatch<I: Instruction<T>, T>(
        mut pc: Pc<'_>,
        mut stack: Stack<'_>,
        mut gas: Gas,
        state: &mut State,
        gast: &GasTable,
        instrsp: *const (),
    ) -> TailInstrFnRet {
        if let Err(e) = I::new().execute(pc.as_mut(), &mut stack, &mut gas, state) {
            tail_return!(tail_call_restore(pc, stack, gas, state, gast, e as usize as *const ()));
        }
        tail_return!(tail_call_next(pc, stack, gas, state, gast, instrsp));
    }
}

extern_table! {
    #[inline(never)] // TODO: bench inlining this vs having a single dispatcher for all
    fn tail_call_next(
        mut pc: Pc<'_>,
        stack: Stack<'_>,
        mut gas: Gas,
        state: &mut State,
        gast: &GasTable,
        instrsp: *const (),
    ) -> TailInstrFnRet {
        let op = match Interpreter::pre_step(pc.as_mut(), &mut gas, gast) {
            Ok(op) => op,
            Err(e) => {
                tail_return!(tail_call_restore(pc, stack, gas, state, gast, e as usize as *const ()));
            }
        };
        // SAFETY: Restoring type-erased table pointer. See [`TailInstrFn`].
        let instrs = unsafe { &*instrsp.cast::<TailInstrTable>() };
        tail_return!(instrs[op as usize](pc, stack, gas, state, gast, instrsp));
    }
}

extern_table! {
    #[inline(never)]
    #[cold]
    fn tail_call_restore(
        pc: Pc<'_>,
        stack: Stack<'_>,
        gas: Gas,
        state: &mut State,
        _gast: &GasTable,
        ret: *const (), // Tail calls require same function signature, this is unused so we pass the return value here.
    ) -> TailInstrFnRet {
        // SAFETY: `raw_interp` is valid for the duration of execution.
        let interp = unsafe { &mut *state.raw_interp };
        interp.pc = pc.pc;
        interp.gas = gas;
        interp.stack_len = stack.len;
        unsafe { core::mem::transmute::<u8, TailInstrFnRet>(ret as usize as u8) }
    }
}
