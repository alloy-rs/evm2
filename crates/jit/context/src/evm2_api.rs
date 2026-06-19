//! evm2-facing runtime context.

use crate::{CallInput, EvmStack, EvmWord, Inputs, InstrStop};
use alloc::boxed::Box;
use alloy_primitives::Bytes;
use core::{
    fmt,
    ptr::{self, NonNull},
};
use evm2::{
    BaseEvmTypes, SpecId,
    bytecode::Bytecode,
    interpreter::{Gas as Evm2Gas, Host as Evm2Host, Interpreter, Memory, Word},
    version::GasParams,
};

const _: () = {
    assert!(core::mem::size_of::<EvmWord>() == core::mem::size_of::<Word>());
    assert!(core::mem::align_of::<EvmWord>() == core::mem::align_of::<Word>());
};

/// The evm2 bytecode compiler runtime context.
#[repr(C)]
pub struct EvmContext<'a> {
    /// Active interpreter frame.
    pub interpreter: NonNull<Interpreter<'a, BaseEvmTypes>>,
    /// Input information (target address, caller, input data, call value).
    pub input: *mut Inputs,
    /// The gas.
    pub gas: Evm2Gas,
    /// Host state consumed by host-touching builtins.
    pub host: &'a mut (dyn Evm2Host<BaseEvmTypes> + 'a),
    /// The return data.
    pub return_data: &'a [u8],
    /// Whether the context is static.
    pub is_static: bool,
    /// The spec ID for the current execution.
    pub spec_id: SpecId,
    /// The contract bytecode, for CODECOPY at runtime.
    pub bytecode: *const [u8],
    /// The size of the call input data, cached for CALLDATASIZE.
    pub calldatasize: usize,
    /// The result set by a builtin before exiting via `evm2_jit_exit`.
    pub exit_result: InstrStop,
    /// Saved RSP from the entry trampoline, used by `evm2_jit_exit` to unwind.
    pub exit_sp: *mut u8,
    /// Cached gas parameters for builtin gas accounting.
    pub gas_params: GasParams,
    /// Cached base pointer for the current memory context.
    pub mem_base: *mut u8,
    /// Cached length of the current memory context in bytes.
    pub mem_len: usize,
    /// Output produced by RETURN or REVERT.
    #[doc(hidden)]
    pub output: Bytes,
    input_scratch: Box<Inputs>,
}

const _: () = {
    use core::mem::offset_of;

    assert!(
        offset_of!(EvmContext<'_>, interpreter) == offset_of!(crate::EvmContext<'_>, interpreter)
    );
    assert!(offset_of!(EvmContext<'_>, input) == offset_of!(crate::EvmContext<'_>, input));
    assert!(offset_of!(EvmContext<'_>, gas) == offset_of!(crate::EvmContext<'_>, gas));
    assert!(offset_of!(EvmContext<'_>, host) == offset_of!(crate::EvmContext<'_>, host));
    assert!(
        offset_of!(EvmContext<'_>, return_data) == offset_of!(crate::EvmContext<'_>, return_data)
    );
    assert!(offset_of!(EvmContext<'_>, is_static) == offset_of!(crate::EvmContext<'_>, is_static));
    assert!(offset_of!(EvmContext<'_>, spec_id) == offset_of!(crate::EvmContext<'_>, spec_id));
    assert!(offset_of!(EvmContext<'_>, bytecode) == offset_of!(crate::EvmContext<'_>, bytecode));
    assert!(
        offset_of!(EvmContext<'_>, calldatasize) == offset_of!(crate::EvmContext<'_>, calldatasize)
    );
    assert!(
        offset_of!(EvmContext<'_>, exit_result) == offset_of!(crate::EvmContext<'_>, exit_result)
    );
    assert!(offset_of!(EvmContext<'_>, exit_sp) == offset_of!(crate::EvmContext<'_>, exit_sp));
    assert!(
        offset_of!(EvmContext<'_>, gas_params) == offset_of!(crate::EvmContext<'_>, gas_params)
    );
    assert!(offset_of!(EvmContext<'_>, mem_base) == offset_of!(crate::EvmContext<'_>, mem_base));
    assert!(offset_of!(EvmContext<'_>, mem_len) == offset_of!(crate::EvmContext<'_>, mem_len));
    assert!(offset_of!(EvmContext<'_>, output) == offset_of!(crate::EvmContext<'_>, output));
};

/// An evm2 bytecode function.
pub type EvmCompilerFn = crate::EvmCompilerFn;

impl crate::EvmCompilerFn {
    /// Calls the function by re-using an evm2 interpreter's resources.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the function is safe to call for this interpreter state.
    pub unsafe fn call_with_interpreter<'a, 'frame: 'a>(
        self,
        interpreter: &'a mut Interpreter<'frame, BaseEvmTypes>,
        host: &'a mut (dyn Evm2Host<BaseEvmTypes> + 'a),
    ) -> InstrStop {
        let (mut ecx, stack, stack_len) =
            EvmContext::from_interpreter_with_stack(interpreter, host);
        let result = unsafe { self.call_with_evm2_context(stack, stack_len, &mut ecx) };
        if result == InstrStop::OutOfGas {
            ecx.gas.spend_all();
        }

        ecx.finish_interpreter_run(result);
        result
    }

    /// Calls the function with an evm2-facing context.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the arguments are valid and that the function is safe to call.
    #[inline]
    pub unsafe fn call_with_evm2_context(
        self,
        stack: &mut EvmStack,
        stack_len: &mut usize,
        ecx: &mut EvmContext<'_>,
    ) -> InstrStop {
        let ecx = unsafe {
            NonNull::new_unchecked((ecx as *mut EvmContext<'_>).cast::<crate::EvmContext<'_>>())
        };
        unsafe {
            crate::evm2_jit_entry(
                ecx,
                NonNull::from(stack),
                NonNull::from(stack_len),
                self.into_inner(),
            )
        }
    }
}

impl fmt::Debug for EvmContext<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EvmContext").field("memory", &self.memory()).finish_non_exhaustive()
    }
}

impl<'a> EvmContext<'a> {
    /// Creates a new context from an interpreter.
    #[inline]
    pub fn from_interpreter<'frame: 'a>(
        interpreter: &'a mut Interpreter<'frame, BaseEvmTypes>,
        host: &'a mut (dyn Evm2Host<BaseEvmTypes> + 'a),
    ) -> Self {
        Self::from_interpreter_with_stack(interpreter, host).0
    }

    /// Creates a new context from an interpreter and returns the borrowed stack.
    #[inline]
    pub fn from_interpreter_with_stack<'frame: 'a>(
        interpreter: &'a mut Interpreter<'frame, BaseEvmTypes>,
        host: &'a mut (dyn Evm2Host<BaseEvmTypes> + 'a),
    ) -> (Self, &'a mut EvmStack, &'a mut usize) {
        let interpreter_ptr = ptr::from_mut(interpreter).cast::<Interpreter<'a, BaseEvmTypes>>();
        let interpreter_ptr = unsafe { NonNull::new_unchecked(interpreter_ptr) };
        let message = interpreter.message();
        let gas = interpreter.gas();
        let bytecode = interpreter.bytecode().as_slice() as *const [u8];
        let spec_id = interpreter.spec();
        let is_static = interpreter.is_static();
        let gas_params = interpreter.version().gas_params;
        let calldatasize = message.input.len();
        let mut input_scratch = Box::new(Inputs {
            target_address: message.destination,
            bytecode_address: Some(message.code_address),
            caller_address: message.caller,
            input: CallInput::Bytes(message.input.clone()),
            call_value: message.value,
        });
        let input = input_scratch.as_mut() as *mut Inputs;
        let return_data = unsafe { &*(interpreter.return_data().as_ref() as *const [u8]) };
        let (stack_ptr, stack_len) = interpreter.stack_mut().into_raw_parts();
        let stack = unsafe { EvmStack::from_mut_ptr(stack_ptr.cast()) };
        let mut this = Self {
            interpreter: interpreter_ptr,
            input,
            gas,
            host,
            return_data,
            is_static,
            spec_id,
            bytecode,
            calldatasize,
            exit_result: InstrStop::Stop,
            exit_sp: ptr::null_mut(),
            gas_params,
            mem_base: ptr::null_mut(),
            mem_len: 0,
            output: Bytes::new(),
            input_scratch,
        };
        this.refresh_memory_cache();
        (this, stack, stack_len)
    }

    /// Finishes state owned by the JIT context after compiled execution.
    #[inline]
    pub fn finish_interpreter_run(&mut self, result: InstrStop) {
        let gas = self.gas;
        let output =
            matches!(result, InstrStop::Return | InstrStop::Revert).then(|| self.output.clone());
        let interpreter = self.interpreter_mut();
        interpreter.set_gas(gas);
        if let Some(output) = output {
            interpreter.set_output_bytes_for_jit(&output);
        }
    }

    /// Refreshes the cached memory base pointer and length from the evm2 memory snapshot.
    #[inline]
    pub fn refresh_memory_cache(&mut self) {
        let (mem_base, mem_len) = {
            let slice = self.memory_mut().as_mut_slice();
            (slice.as_mut_ptr(), slice.len())
        };
        self.mem_base = mem_base;
        self.mem_len = mem_len;
    }

    /// Returns the input shim visible to compiled code.
    #[inline]
    pub fn input(&self) -> &Inputs {
        &self.input_scratch
    }

    #[inline]
    fn interpreter(&self) -> &Interpreter<'a, BaseEvmTypes> {
        unsafe { self.interpreter.as_ref() }
    }

    #[inline]
    fn interpreter_mut(&mut self) -> &mut Interpreter<'a, BaseEvmTypes> {
        unsafe { self.interpreter.as_mut() }
    }

    #[inline]
    fn memory(&self) -> &Memory {
        self.interpreter().memory_ref()
    }

    #[inline]
    fn memory_mut(&mut self) -> &mut Memory {
        self.interpreter_mut().memory_mut()
    }

    #[inline]
    #[cfg(test)]
    fn tx_env(&self) -> &'a evm2::env::TxEnv<BaseEvmTypes> {
        self.interpreter().tx_env()
    }

    #[inline]
    #[cfg(test)]
    fn block_env(&mut self) -> &evm2::env::BlockEnv<BaseEvmTypes> {
        self.host.block_env()
    }
}

/// Returns the bytecode bytes for CODECOPY-compatible runtime access.
#[inline]
pub fn bytecode_slice(bytecode: &Bytecode) -> &[u8] {
    bytecode.original_byte_slice()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{Address, Bytes as AlloyBytes};
    use core::mem::offset_of;
    use evm2::{
        BaseEvmConfigSelector, Evm, EvmConfigSelector, Precompiles,
        bytecode::Bytecode,
        env::{BlockEnv, TxEnv},
        ethereum::ethereum_tx_registry,
        evm::EmptyDB,
        interpreter::{Message, op},
    };

    #[test]
    fn evm2_context_matches_imported_context_offsets() {
        assert_eq!(offset_of!(EvmContext<'_>, input), offset_of!(crate::EvmContext<'_>, input));
        assert_eq!(offset_of!(EvmContext<'_>, gas), offset_of!(crate::EvmContext<'_>, gas));
        assert_eq!(offset_of!(EvmContext<'_>, spec_id), offset_of!(crate::EvmContext<'_>, spec_id));
        assert_eq!(
            offset_of!(EvmContext<'_>, mem_base),
            offset_of!(crate::EvmContext<'_>, mem_base)
        );
        assert_eq!(offset_of!(EvmContext<'_>, mem_len), offset_of!(crate::EvmContext<'_>, mem_len));
    }

    #[test]
    fn evm2_gas_is_used_directly() {
        let mut gas = Evm2Gas::new_with_regular_gas_and_reservoir(100, 20);
        gas.set_remaining(77);
        gas.set_state_gas_spent(11);
        gas.set_refunded(3);
        gas.memory_mut().words_num = 4;
        gas.memory_mut().expansion_cost = 12;

        assert_eq!(gas.limit(), 100);
        assert_eq!(gas.remaining(), 77);
        assert_eq!(gas.reservoir(), 20);
        assert_eq!(gas.state_gas_spent(), 11);
        assert_eq!(gas.refunded(), 3);
        assert_eq!(gas.memory().words_num, 4);
        assert_eq!(gas.memory().expansion_cost, 12);
    }

    #[test]
    fn evm2_context_host_context_uses_evm2_env() {
        let tx_origin = Address::from([0x11; 20]);
        let beneficiary = Address::from([0x22; 20]);
        let config = <BaseEvmConfigSelector as EvmConfigSelector<BaseEvmTypes>>::execution_config(
            SpecId::CANCUN,
        );
        let tx_env = TxEnv { origin: tx_origin, gas_price: Word::from(7), ..TxEnv::default() };
        let message = Message { gas_limit: 1_000_000, ..Default::default() };
        let mut interpreter = Interpreter::<BaseEvmTypes>::new(
            Bytecode::new_legacy(alloy_primitives::Bytes::from_static(&[op::STOP])),
            &tx_env,
            &message,
            false,
        );
        let mut host = Evm::<BaseEvmTypes>::new(
            SpecId::CANCUN,
            BlockEnv { number: Word::from(9), beneficiary, ..BlockEnv::default() },
            ethereum_tx_registry(SpecId::CANCUN),
            EmptyDB::default(),
            Precompiles::base(SpecId::CANCUN),
        );

        interpreter.prepare_jit_run(&config, &mut host);
        let mut ecx = EvmContext::from_interpreter(&mut interpreter, &mut host);

        assert_eq!(ecx.tx_env().origin, tx_origin);
        assert_eq!(ecx.tx_env().gas_price, Word::from(7));
        assert_eq!(ecx.block_env().number, Word::from(9));
        assert_eq!(ecx.block_env().beneficiary, beneficiary);
    }

    #[test]
    fn evm2_context_uses_interpreter_memory() {
        let config = <BaseEvmConfigSelector as EvmConfigSelector<BaseEvmTypes>>::execution_config(
            SpecId::CANCUN,
        );
        let tx_env = TxEnv::default();
        let message = Message { gas_limit: 1_000_000, ..Default::default() };
        let mut interpreter = Interpreter::<BaseEvmTypes>::new(
            Bytecode::new_legacy(alloy_primitives::Bytes::from_static(&[op::STOP])),
            &tx_env,
            &message,
            false,
        );
        let mut host = Evm::<BaseEvmTypes>::new(
            SpecId::CANCUN,
            BlockEnv::default(),
            ethereum_tx_registry(SpecId::CANCUN),
            EmptyDB::default(),
            Precompiles::base(SpecId::CANCUN),
        );

        interpreter.prepare_jit_run(&config, &mut host);
        {
            let memory = interpreter.memory_mut();
            memory.resize(0, 3).unwrap();
            memory.set(0, b"abc");
        }

        let mut ecx = EvmContext::from_interpreter(&mut interpreter, &mut host);
        assert_eq!(ecx.memory().as_slice(), b"abc");
        ecx.memory_mut().set(1, b"z");
        drop(ecx);

        assert_eq!(interpreter.memory_ref().slice(0, 3), b"azc");
    }

    unsafe extern "C" fn evm2_return_output(
        mut ecx: NonNull<crate::EvmContext<'_>>,
        _stack: NonNull<EvmStack>,
        _stack_len: NonNull<usize>,
    ) -> InstrStop {
        let ecx = unsafe { ecx.as_mut() };
        ecx.output = AlloyBytes::copy_from_slice(b"ok");
        InstrStop::Return
    }

    #[test]
    fn evm2_call_with_interpreter_maps_return_output() {
        let config = <BaseEvmConfigSelector as EvmConfigSelector<BaseEvmTypes>>::execution_config(
            SpecId::CANCUN,
        );
        let tx_env = TxEnv::default();
        let message = Message { gas_limit: 1_000_000, ..Default::default() };
        let mut interpreter = Interpreter::<BaseEvmTypes>::new(
            Bytecode::new_legacy(alloy_primitives::Bytes::from_static(&[op::STOP])),
            &tx_env,
            &message,
            false,
        );
        let mut host = Evm::<BaseEvmTypes>::new(
            SpecId::CANCUN,
            BlockEnv::default(),
            ethereum_tx_registry(SpecId::CANCUN),
            EmptyDB::default(),
            Precompiles::base(SpecId::CANCUN),
        );

        interpreter.prepare_jit_run(&config, &mut host);
        let stop = unsafe {
            EvmCompilerFn::new(evm2_return_output)
                .call_with_interpreter(&mut interpreter, &mut host)
        };

        assert_eq!(stop, InstrStop::Return);
        assert_eq!(interpreter.output(), b"ok");
    }
}
