use super::super::super::{Cursor as _, dispatch_inner, finish_run, imp};
use crate::{
    EvmConfig, EvmTypesHost,
    interpreter::{
        InstrStop, Interpreter, Pc, Stack,
        dispatch::{NoInspector, run_state},
        op,
    },
};

pub(in crate::interpreter) fn run<T: EvmTypesHost, C: EvmConfig<T>>(
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
                            let (next_pc, next_gas) = dispatch_inner::<T, C, NoInspector, _>(
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
