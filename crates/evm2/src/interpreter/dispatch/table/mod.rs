use super::{InspectMode, UNKNOWN_OP, inc_pc, unknown_instruction};
use crate::{
    EvmTypes,
    interpreter::{InstrStop, Interpreter, InterpreterState, Pc, Stack},
    trustme,
};
use core::hint::cold_path;

cfg_if::cfg_if! {
    if #[cfg(dispatch_packed)] {
        mod packed;
        use packed as imp;
    } else if #[cfg(dispatch_single_return)] {
        mod single_return;
        use single_return as imp;
    } else {
        mod unpacked;
        use unpacked as imp;
    }
}

pub(super) use imp::{RawInstrFn, RawInstrTable, dispatch, unknown_dispatch};

pub(in crate::interpreter) fn run_table_loop<T: EvmTypes>(
    interpreter: &mut Interpreter<'_, T>,
    instructions: &RawInstrTable<T>,
) -> InstrStop {
    // SAFETY: Only the active interpreter lifetime is erased; this stays as a raw pointer so
    // the dispatch loop does not create an extra `&mut` alias for `interpreter`.
    let raw = unsafe { trustme::decouple_lt_mut_ptr(interpreter as *mut Interpreter<'_, T>) };
    // SAFETY: Instruction methods must not access the stack through `InterpreterState` while
    // the separate stack view is live.
    let state = InterpreterState::wrap_mut(unsafe { &mut *raw });
    let mut pc = Pc::new(interpreter.pc);
    let mut stack = Stack::new(&mut interpreter.stack, interpreter.stack_len);
    let mut loop_state = imp::loop_state(&interpreter.gas);
    loop {
        let op = pc.op();
        let instr = instructions[op as usize];
        let (next_pc, next_stack_len) =
            imp::dispatch_loop_call(instr, pc, stack.reborrow(), state, &mut loop_state);
        pc = next_pc;
        stack.len = next_stack_len;

        if pc.as_ptr().is_null() {
            cold_path();
            interpreter.pc = pc.as_ptr();
            interpreter.stack_len = stack.len;
            imp::finish_loop(&mut interpreter.gas, loop_state);
            return interpreter.result.unwrap_err();
        }
    }
}
