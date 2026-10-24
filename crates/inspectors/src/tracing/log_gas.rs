//! Complete LOG requirements for traces when execution stops at an earlier charge.

use evm2::{
    EvmTypes,
    interpreter::{InstrStop, Interpreter, opcode::op},
};

/// Gas not necessarily reached by the interpreter's staged LOG deductions.
#[derive(Clone, Copy, Debug)]
pub(super) struct PendingLogGas {
    stack_len: usize,
    dynamic: u64,
    memory: u64,
}

impl PendingLogGas {
    pub(super) fn capture<T: EvmTypes>(interp: &Interpreter<'_, '_, T>) -> Option<Self> {
        let opcode = interp.opcode();
        if !(op::LOG0..=op::LOG4).contains(&opcode) {
            return None;
        }
        let topics = opcode - op::LOG0;
        let stack = interp.stack();
        let stack = stack.as_slice();
        if stack.len() < usize::from(topics) + 2 {
            return None;
        }
        let len = usize::try_from(stack[stack.len() - 2]).ok()?;
        let params = &interp.version().gas_params;
        let memory = if len == 0 {
            0
        } else {
            let offset = usize::try_from(stack[stack.len() - 1]).ok()?;
            let words = offset.checked_add(len)?.div_ceil(32);
            words.checked_mul(32)?;
            params.memory_cost(words).saturating_sub(interp.gas().memory().expansion_cost)
        };
        Some(Self { stack_len: stack.len(), dynamic: params.log_cost(topics, len), memory })
    }

    pub(super) fn failed_cost<T: EvmTypes>(self, interp: &Interpreter<'_, '_, T>) -> Option<u64> {
        if interp.result() != Err(InstrStop::OutOfGas) {
            return None;
        }
        let attempted = interp.failed_charge()?;
        // The static charge precedes the stack preamble. After that succeeds, LOG pops
        // offset/length and attempts its topic/data charge before expanding memory.
        // Retain the actual static charge, including custom opcode gas schedules.
        let dynamic = if interp.stack().len() == self.stack_len { self.dynamic } else { 0 };
        Some(attempted.saturating_add(dynamic).saturating_add(self.memory))
    }
}
