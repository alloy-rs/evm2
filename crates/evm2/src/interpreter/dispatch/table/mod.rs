use super::{InspectMode, UNKNOWN_OP, inc_pc, run_state, unknown_instruction};
use crate::{
    EvmTypes,
    interpreter::{InstrStop, Interpreter},
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

pub(in crate::interpreter) fn run<T: EvmTypes>(
    interpreter: &mut Interpreter<'_, T>,
    instructions: &RawInstrTable<T>,
) -> InstrStop {
    let (state, mut pc, mut stack) = run_state(interpreter);
    let mut loop_state = imp::loop_state(state.gas_mut());
    loop {
        let op = pc.op();
        let instr = instructions[op as usize];
        let (next_pc, next_stack_len) =
            imp::dispatch_loop_call(instr, pc, stack.reborrow(), state, &mut loop_state);
        pc = next_pc;
        stack.len = next_stack_len;

        if pc.as_ptr().is_null() {
            cold_path();
            state.set_pc_stack_len(pc.as_ptr(), stack.len);
            imp::finish_loop(state.gas_mut(), loop_state);
            return state.result().unwrap_err();
        }
    }
}
