//! Match dispatch: the whole frame runs in one function.
//!
//! The loop matches on the opcode and every arm holds the handler inlined with a constant opcode,
//! so the program counter, stack length, gas and stack base stay in registers across
//! instructions, with no call between them. Each arm ends in its own exit check and jumps back to
//! the match, which lets the compiler copy the match's indirect jump into the arms so that each
//! one gets its own branch prediction history.
//!
//! Only frames that run without an inspector use it, on the config its function was
//! instantiated for. The others keep the table loop.

use super::{Cursor as _, dispatch_inner, finish_run, imp};
use crate::{
    EvmConfig, EvmTypesHost,
    interpreter::{InstrStop, Interpreter, Pc, Stack, dispatch::run_state, op},
};

/// Runs a frame without an inspector to its stop.
pub(crate) type MatchLoop<T> = fn(&mut Interpreter<'_, '_, T>) -> InstrStop;

pub(in crate::interpreter) fn run_match<T: EvmTypesHost, C: EvmConfig<T>>(
    interpreter: &mut Interpreter<'_, '_, T>,
) -> InstrStop {
    let (state, mut pc, mut raw_stack) = run_state(interpreter);
    // SAFETY: This is the only stack borrow outside inspector callbacks.
    let backing = unsafe { raw_stack.borrow() };
    let (backing, mut len) = (backing.stack, backing.len);
    let mut gas = imp::loop_state(state.gas_mut());

    macro_rules! arms {
        ($($op:literal),* $(,)?) => {
            loop {
                match pc.op() {
                    $(
                        $op => {
                            // The table maps the unknown opcodes of a spec to `INVALID`.
                            let opcode = if C::OPCODE_CONFIG.is_unknown_opcode($op) {
                                op::INVALID
                            } else {
                                $op
                            };
                            let mut stack = Stack::new(&mut *backing, len);
                            let (next_pc, next_gas) = dispatch_inner::<T, C, super::NoInspector, _>(
                                pc,
                                stack.as_mut(),
                                gas,
                                state,
                                opcode,
                            );
                            len = stack.len;
                            gas = next_gas;
                            if next_pc.as_ptr().is_null() {
                                break;
                            }
                            pc = next_pc;
                        }
                    )*
                }
            }
        };
    }
    for_each_opcode_value!(arms);

    finish_run(state, imp::Cursor::new(Pc::new(core::ptr::null()), len), gas)
}
