//! evm2-facing runtime context.

use crate::{EvmStack, EvmWord, ResumeAt};
use core::{fmt, ptr};
use evm2::{
    BaseEvmTypes, EvmTypes, SpecId,
    bytecode::Bytecode,
    env::TxEnv,
    interpreter::{Gas, InstrStop, Interpreter, Memory, Message, Word},
    version::GasParams,
};

const _: () = {
    assert!(core::mem::size_of::<EvmWord>() == core::mem::size_of::<Word>());
    assert!(core::mem::align_of::<EvmWord>() == core::mem::align_of::<Word>());
};

/// The evm2 bytecode compiler runtime context.
///
/// This mirrors the imported revmc context, but sources frame state from evm2's
/// [`Interpreter`] and [`Message`] types.
#[repr(C)]
pub struct EvmContext<'a, T: EvmTypes = BaseEvmTypes> {
    /// The memory.
    pub memory: &'a mut Memory,
    /// Frame-local call/create message.
    pub message: &'a Message<T>,
    /// The gas.
    pub gas: Gas,
    /// The host.
    pub host: &'a mut T::Host,
    /// Transaction-global environment.
    pub tx_env: &'a TxEnv<T>,
    /// The return data.
    pub return_data: &'a [u8],
    /// Whether the context is static.
    pub is_static: bool,
    /// The spec ID for the current execution.
    pub spec_id: SpecId,
    /// Index that tracks where execution should resume after a CALL/CREATE suspension.
    #[doc(hidden)]
    pub resume_at: ResumeAt,
    /// The contract bytecode, for CODECOPY at runtime.
    pub bytecode: *const [u8],
    /// Optional callback invoked by the LOG builtin after constructing the log.
    #[doc(hidden)]
    pub on_log: Option<&'a mut (dyn FnMut(&alloy_primitives::Log) + 'a)>,
    /// The size of the call input data, cached for CALLDATASIZE.
    pub calldatasize: usize,
    /// The result set by a builtin before exiting via `revmc_exit`.
    pub exit_result: InstrStop,
    /// Saved RSP from the entry trampoline, used by `revmc_exit` to unwind.
    pub exit_sp: *mut u8,
    /// Cached gas parameters from the active version.
    pub gas_params: GasParams,
    /// Cached base pointer for the current memory context.
    pub mem_base: *mut u8,
    /// Cached length of the current memory context in bytes.
    pub mem_len: usize,
}

impl<T: EvmTypes> fmt::Debug for EvmContext<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EvmContext").field("memory", &self.memory).finish_non_exhaustive()
    }
}

impl<'a, T: EvmTypes> EvmContext<'a, T> {
    /// Creates a new context from an interpreter.
    #[inline]
    pub fn from_interpreter(
        interpreter: &'a mut Interpreter<'a, T>,
        host: &'a mut T::Host,
    ) -> Self {
        Self::from_interpreter_with_stack(interpreter, host).0
    }

    /// Creates a new context from an interpreter and returns the borrowed stack.
    #[inline]
    pub fn from_interpreter_with_stack(
        interpreter: &'a mut Interpreter<'a, T>,
        host: &'a mut T::Host,
    ) -> (Self, &'a mut EvmStack, &'a mut usize) {
        let resume_at = ResumeAt::from(interpreter.pc());
        let parts = interpreter.jit_context_parts_mut();
        let stack = unsafe { EvmStack::from_mut_ptr(parts.stack.cast()) };
        let bytecode = parts.bytecode.original_byte_slice() as *const [u8];
        let calldatasize = parts.message.input.len();
        let mut this = Self {
            memory: parts.memory,
            message: parts.message,
            gas: parts.gas,
            host,
            tx_env: parts.tx_env,
            return_data: parts.return_data.as_ref(),
            is_static: parts.is_static,
            spec_id: parts.spec,
            resume_at,
            bytecode,
            on_log: None,
            calldatasize,
            exit_result: InstrStop::Stop,
            exit_sp: ptr::null_mut(),
            gas_params: parts.version.gas_params,
            mem_base: ptr::null_mut(),
            mem_len: 0,
        };
        this.refresh_memory_cache();
        (this, stack, parts.stack_len)
    }

    /// Stores context state back into an interpreter after compiled execution.
    #[inline]
    pub fn store_interpreter_state(self, interpreter: &mut Interpreter<'_, T>) {
        interpreter.set_gas(self.gas);
        interpreter.set_pc(self.resume_at.get());
        if self.return_data.is_empty() {
            interpreter.return_data_mut().clear();
        }
    }

    /// Refreshes the cached memory base pointer and length from [`Memory`].
    #[inline]
    pub fn refresh_memory_cache(&mut self) {
        let slice = self.memory.as_mut_slice();
        self.mem_base = slice.as_mut_ptr();
        self.mem_len = slice.len();
    }
}

/// Returns the bytecode bytes for CODECOPY-compatible runtime access.
#[inline]
pub fn bytecode_slice(bytecode: &Bytecode) -> &[u8] {
    bytecode.original_byte_slice()
}
