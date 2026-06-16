//! evm2-facing runtime context.

use crate::{EvmStack, EvmWord, ResumeAt};
use alloc::{borrow::Cow, boxed::Box};
use core::{
    fmt,
    marker::PhantomData,
    mem,
    ptr::{self, NonNull},
};
use evm2::{
    BaseEvmTypes, EvmFeatures, EvmTypes, SpecId, Version,
    bytecode::Bytecode,
    env::{BlockEnv, TxEnv},
    interpreter::{
        Gas as Evm2Gas, Host as Evm2Host, InstrStop, Interpreter, Memory, Message, Word,
    },
};
use revm_interpreter::{
    CallInput, Gas as RevmGas, Host as RevmHost, InputsImpl, InstructionResult, InterpreterAction,
    SStoreResult as RevmSStoreResult, SelfDestructResult as RevmSelfDestructResult,
    StateLoad as RevmStateLoad, bytecode::Bytecode as RevmBytecode,
    context_interface::cfg::GasParams as RevmGasParams, host::LoadError,
    state::AccountInfo as RevmAccountInfo,
};
use revm_primitives::{Bytes as RevmBytes, hardfork::SpecId as RevmSpecId};

const _: () = {
    assert!(core::mem::size_of::<EvmWord>() == core::mem::size_of::<Word>());
    assert!(core::mem::align_of::<EvmWord>() == core::mem::align_of::<Word>());
};

/// Serialized revm host trait object slot used to keep the imported context layout.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
#[doc(hidden)]
pub struct RevmHostPtr {
    data: *mut (),
    vtable: *mut (),
}

impl RevmHostPtr {
    fn from_host(host: &mut dyn RevmHost) -> Self {
        unsafe { mem::transmute::<&mut dyn RevmHost, Self>(host) }
    }

    #[cfg(test)]
    unsafe fn as_host_mut<'a>(self) -> &'a mut dyn RevmHost {
        unsafe { mem::transmute::<Self, &'a mut dyn RevmHost>(self) }
    }
}

struct RevmHostAdapter<'a, T: EvmTypes> {
    host: NonNull<T::Host>,
    block_env: BlockEnv<T>,
    tx_env: &'a TxEnv<T>,
    version: &'a Version,
    gas_params: RevmGasParams,
    _marker: PhantomData<&'a mut T::Host>,
}

impl<'a, T: EvmTypes> RevmHostAdapter<'a, T> {
    fn new(
        host: &'a mut T::Host,
        tx_env: &'a TxEnv<T>,
        version: &'a Version,
        spec_id: RevmSpecId,
    ) -> Self {
        let block_env = *host.block_env();
        Self {
            host: NonNull::from(host),
            block_env,
            tx_env,
            version,
            gas_params: RevmGasParams::new_spec(spec_id),
            _marker: PhantomData,
        }
    }

    fn host_mut(&mut self) -> &mut T::Host {
        unsafe { self.host.as_ptr().as_mut().unwrap_unchecked() }
    }
}

impl<T: EvmTypes> RevmHost for RevmHostAdapter<'_, T> {
    fn basefee(&self) -> revm_primitives::U256 {
        self.block_env.basefee
    }

    fn blob_gasprice(&self) -> revm_primitives::U256 {
        self.block_env.blob_basefee
    }

    fn gas_limit(&self) -> revm_primitives::U256 {
        self.block_env.gas_limit
    }

    fn difficulty(&self) -> revm_primitives::U256 {
        self.block_env.difficulty
    }

    fn prevrandao(&self) -> Option<revm_primitives::U256> {
        Some(self.block_env.prevrandao)
    }

    fn block_number(&self) -> revm_primitives::U256 {
        self.block_env.number
    }

    fn timestamp(&self) -> revm_primitives::U256 {
        self.block_env.timestamp
    }

    fn beneficiary(&self) -> alloy_primitives::Address {
        self.block_env.beneficiary
    }

    fn slot_num(&self) -> revm_primitives::U256 {
        self.block_env.slot_num
    }

    fn chain_id(&self) -> revm_primitives::U256 {
        self.tx_env.chain_id
    }

    fn effective_gas_price(&self) -> revm_primitives::U256 {
        self.tx_env.gas_price
    }

    fn caller(&self) -> alloy_primitives::Address {
        self.tx_env.origin
    }

    fn blob_hash(&self, number: usize) -> Option<revm_primitives::U256> {
        self.tx_env.blob_hashes.get(number).copied()
    }

    fn max_initcode_size(&self) -> usize {
        self.version.max_initcode_size
    }

    fn gas_params(&self) -> &RevmGasParams {
        &self.gas_params
    }

    fn is_amsterdam_eip8037_enabled(&self) -> bool {
        self.version.features.contains(EvmFeatures::EIP8037)
    }

    fn block_hash(&mut self, number: u64) -> Option<alloy_primitives::B256> {
        self.host_mut().block_hash(&Word::from(number)).ok().flatten()
    }

    fn selfdestruct(
        &mut self,
        address: alloy_primitives::Address,
        target: alloy_primitives::Address,
        skip_cold_load: bool,
    ) -> Result<RevmStateLoad<RevmSelfDestructResult>, LoadError> {
        let result = self
            .host_mut()
            .selfdestruct(&address, &target, skip_cold_load)
            .map_err(|stop| load_error(stop, skip_cold_load))?;
        Ok(RevmStateLoad::new(
            RevmSelfDestructResult {
                had_value: result.had_value,
                target_exists: !result.target_is_empty,
                previously_destroyed: result.previously_destroyed,
            },
            result.is_cold,
        ))
    }

    fn log(&mut self, log: alloy_primitives::Log) {
        self.host_mut().log(log);
    }

    fn sstore_skip_cold_load(
        &mut self,
        address: alloy_primitives::Address,
        key: revm_primitives::U256,
        value: revm_primitives::U256,
        skip_cold_load: bool,
    ) -> Result<RevmStateLoad<RevmSStoreResult>, LoadError> {
        let result = self
            .host_mut()
            .sstore(&address, &key, &value, skip_cold_load)
            .map_err(|stop| load_error(stop, skip_cold_load))?;
        Ok(RevmStateLoad::new(
            RevmSStoreResult {
                original_value: result.original_value,
                present_value: result.present_value,
                new_value: result.new_value,
            },
            result.is_cold,
        ))
    }

    fn sload_skip_cold_load(
        &mut self,
        address: alloy_primitives::Address,
        key: revm_primitives::U256,
        skip_cold_load: bool,
    ) -> Result<RevmStateLoad<revm_primitives::U256>, LoadError> {
        let result = self
            .host_mut()
            .sload(&address, &key, skip_cold_load)
            .map_err(|stop| load_error(stop, skip_cold_load))?;
        Ok(RevmStateLoad::new(result.value, result.is_cold))
    }

    fn tstore(
        &mut self,
        address: alloy_primitives::Address,
        key: revm_primitives::U256,
        value: revm_primitives::U256,
    ) {
        self.host_mut().tstore(&address, &key, &value);
    }

    fn tload(
        &mut self,
        address: alloy_primitives::Address,
        key: revm_primitives::U256,
    ) -> revm_primitives::U256 {
        self.host_mut().tload(&address, &key)
    }

    fn load_account_info_skip_cold_load(
        &mut self,
        address: alloy_primitives::Address,
        load_code: bool,
        skip_cold_load: bool,
    ) -> Result<revm_interpreter::context_interface::journaled_state::AccountInfoLoad<'_>, LoadError>
    {
        let account = self
            .host_mut()
            .load_account(&address, load_code, skip_cold_load)
            .map_err(|stop| load_error(stop, skip_cold_load))?;
        let info = RevmAccountInfo {
            balance: account.balance,
            nonce: 0,
            code_hash: account.code_hash,
            account_id: None,
            code: Some(revm_bytecode_from_evm2(&account.code)),
        };
        Ok(revm_interpreter::context_interface::journaled_state::AccountInfoLoad {
            account: Cow::Owned(info),
            is_cold: account.is_cold,
            is_empty: account.is_empty,
        })
    }
}

fn load_error(stop: InstrStop, skip_cold_load: bool) -> LoadError {
    if skip_cold_load && stop == InstrStop::OutOfGas {
        LoadError::ColdLoadSkipped
    } else {
        LoadError::DBError
    }
}

fn revm_bytecode_from_evm2(bytecode: &Bytecode) -> RevmBytecode {
    RevmBytecode::new_raw(RevmBytes::copy_from_slice(bytecode.original_byte_slice()))
}

/// The evm2 bytecode compiler runtime context.
///
/// This mirrors the imported revmc context ABI, but sources frame state from evm2's
/// [`Interpreter`] and [`Message`] types. Host-touching builtins still need an evm2-native port.
#[repr(C)]
pub struct EvmContext<'a, T: EvmTypes = BaseEvmTypes> {
    /// The memory.
    pub memory: *mut Memory,
    /// Input information (target address, caller, input data, call value).
    pub input: *mut InputsImpl,
    /// The gas.
    pub gas: RevmGas,
    /// Placeholder for the imported revm host trait object slot.
    pub host: RevmHostPtr,
    /// Placeholder for the imported revm next-action slot.
    pub next_action: *mut (),
    /// The return data.
    pub return_data: &'a [u8],
    /// Whether the context is static.
    pub is_static: bool,
    /// The revm ABI spec ID for the current execution.
    pub spec_id: RevmSpecId,
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
    pub exit_result: InstructionResult,
    /// Saved RSP from the entry trampoline, used by `revmc_exit` to unwind.
    pub exit_sp: *mut u8,
    /// Cached gas parameters for the imported revm builtin ABI.
    pub gas_params: RevmGasParams,
    /// Cached base pointer for the current memory context.
    pub mem_base: *mut u8,
    /// Cached length of the current memory context in bytes.
    pub mem_len: usize,
    /// Transaction-global environment.
    #[doc(hidden)]
    pub tx_env: &'a TxEnv<T>,
    /// Frame-local call/create message.
    #[doc(hidden)]
    pub message: &'a Message<T>,
    input_scratch: Box<InputsImpl>,
    next_action_scratch: Box<Option<InterpreterAction>>,
    _host_adapter: Box<RevmHostAdapter<'a, T>>,
}

const _: () = {
    use core::mem::{offset_of, size_of};

    assert!(size_of::<RevmHostPtr>() == size_of::<&mut dyn revm_interpreter::Host>());
    assert!(
        offset_of!(EvmContext<'_, BaseEvmTypes>, memory)
            == offset_of!(crate::EvmContext<'_>, memory)
    );
    assert!(
        offset_of!(EvmContext<'_, BaseEvmTypes>, input) == offset_of!(crate::EvmContext<'_>, input)
    );
    assert!(
        offset_of!(EvmContext<'_, BaseEvmTypes>, gas) == offset_of!(crate::EvmContext<'_>, gas)
    );
    assert!(
        offset_of!(EvmContext<'_, BaseEvmTypes>, host) == offset_of!(crate::EvmContext<'_>, host)
    );
    assert!(
        offset_of!(EvmContext<'_, BaseEvmTypes>, next_action)
            == offset_of!(crate::EvmContext<'_>, next_action)
    );
    assert!(
        offset_of!(EvmContext<'_, BaseEvmTypes>, return_data)
            == offset_of!(crate::EvmContext<'_>, return_data)
    );
    assert!(
        offset_of!(EvmContext<'_, BaseEvmTypes>, is_static)
            == offset_of!(crate::EvmContext<'_>, is_static)
    );
    assert!(
        offset_of!(EvmContext<'_, BaseEvmTypes>, spec_id)
            == offset_of!(crate::EvmContext<'_>, spec_id)
    );
    assert!(
        offset_of!(EvmContext<'_, BaseEvmTypes>, resume_at)
            == offset_of!(crate::EvmContext<'_>, resume_at)
    );
    assert!(
        offset_of!(EvmContext<'_, BaseEvmTypes>, bytecode)
            == offset_of!(crate::EvmContext<'_>, bytecode)
    );
    assert!(
        offset_of!(EvmContext<'_, BaseEvmTypes>, on_log)
            == offset_of!(crate::EvmContext<'_>, on_log)
    );
    assert!(
        offset_of!(EvmContext<'_, BaseEvmTypes>, calldatasize)
            == offset_of!(crate::EvmContext<'_>, calldatasize)
    );
    assert!(
        offset_of!(EvmContext<'_, BaseEvmTypes>, exit_result)
            == offset_of!(crate::EvmContext<'_>, exit_result)
    );
    assert!(
        offset_of!(EvmContext<'_, BaseEvmTypes>, exit_sp)
            == offset_of!(crate::EvmContext<'_>, exit_sp)
    );
    assert!(
        offset_of!(EvmContext<'_, BaseEvmTypes>, gas_params)
            == offset_of!(crate::EvmContext<'_>, gas_params)
    );
    assert!(
        offset_of!(EvmContext<'_, BaseEvmTypes>, mem_base)
            == offset_of!(crate::EvmContext<'_>, mem_base)
    );
    assert!(
        offset_of!(EvmContext<'_, BaseEvmTypes>, mem_len)
            == offset_of!(crate::EvmContext<'_>, mem_len)
    );
};

/// Interpreter state copied out of a JIT context after compiled execution.
#[derive(Clone, Copy, Debug)]
#[doc(hidden)]
pub struct InterpreterState {
    gas: Evm2Gas,
    pc: usize,
    clear_return_data: bool,
}

impl InterpreterState {
    /// Stores this state back into an evm2 interpreter.
    #[inline]
    pub fn store<T: EvmTypes>(self, interpreter: &mut Interpreter<'_, T>) {
        interpreter.set_gas(self.gas);
        interpreter.set_pc(self.pc);
        if self.clear_return_data {
            interpreter.return_data_mut().clear();
        }
    }
}

/// The raw function signature of an evm2 bytecode function.
///
/// The ABI intentionally matches [`crate::RawEvmCompilerFn`].
pub type RawEvmCompilerFn<T = BaseEvmTypes> = unsafe extern "C" fn(
    ecx: NonNull<EvmContext<'_, T>>,
    stack: NonNull<EvmStack>,
    stack_len: NonNull<usize>,
) -> InstructionResult;

/// An evm2 bytecode function.
#[derive(Clone, Copy, Debug, Hash)]
pub struct EvmCompilerFn<T: EvmTypes = BaseEvmTypes>(RawEvmCompilerFn<T>);

impl<T: EvmTypes> EvmCompilerFn<T> {
    /// Wraps the function.
    #[inline]
    pub const fn new(f: RawEvmCompilerFn<T>) -> Self {
        Self(f)
    }

    /// Rewraps an ABI-compatible compiled function for evm2 calls.
    #[inline]
    pub fn from_abi_compatible(f: crate::EvmCompilerFn) -> Self {
        Self(unsafe {
            mem::transmute::<crate::RawEvmCompilerFn, RawEvmCompilerFn<T>>(f.into_inner())
        })
    }

    /// Unwraps the function.
    #[inline]
    pub const fn into_inner(self) -> RawEvmCompilerFn<T> {
        self.0
    }

    /// Calls the function by re-using an evm2 interpreter's resources.
    ///
    /// Returns `None` if the compiled function suspended for CALL/CREATE handling. That path
    /// still needs an evm2-native action bridge.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the function is safe to call for this interpreter state.
    pub unsafe fn call_with_interpreter<'a, 'frame: 'a>(
        self,
        interpreter: &'a mut Interpreter<'frame, T>,
        host: &'a mut T::Host,
    ) -> Option<InstrStop> {
        let (mut ecx, stack, stack_len) =
            EvmContext::from_interpreter_with_stack(interpreter, host);
        let result = unsafe { self.call(stack, stack_len, &mut ecx) };
        if result == InstructionResult::OutOfGas {
            ecx.gas.spend_all();
        }

        let stop = instr_stop_from_instruction_result(result);
        let state = ecx.interpreter_state();
        drop(ecx);
        state.store(interpreter);
        stop
    }

    /// Calls the function.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the arguments are valid and that the function is safe to call.
    #[inline]
    pub unsafe fn call(
        self,
        stack: &mut EvmStack,
        stack_len: &mut usize,
        ecx: &mut EvmContext<'_, T>,
    ) -> InstructionResult {
        let ecx = unsafe {
            NonNull::new_unchecked((ecx as *mut EvmContext<'_, T>).cast::<crate::EvmContext<'_>>())
        };
        let f = unsafe { mem::transmute::<RawEvmCompilerFn<T>, crate::RawEvmCompilerFn>(self.0) };
        unsafe { crate::revmc_entry(ecx, NonNull::from(stack), NonNull::from(stack_len), f) }
    }
}

impl<T: EvmTypes> fmt::Debug for EvmContext<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EvmContext").field("memory", &self.memory).finish_non_exhaustive()
    }
}

impl<'a, T: EvmTypes> EvmContext<'a, T> {
    /// Creates a new context from an interpreter.
    #[inline]
    pub fn from_interpreter<'frame: 'a>(
        interpreter: &'a mut Interpreter<'frame, T>,
        host: &'a mut T::Host,
    ) -> Self {
        Self::from_interpreter_with_stack(interpreter, host).0
    }

    /// Creates a new context from an interpreter and returns the borrowed stack.
    #[inline]
    pub fn from_interpreter_with_stack<'frame: 'a>(
        interpreter: &'a mut Interpreter<'frame, T>,
        host: &'a mut T::Host,
    ) -> (Self, &'a mut EvmStack, &'a mut usize) {
        let resume_at = ResumeAt::from(interpreter.pc());
        let parts = interpreter.jit_context_parts_mut();
        let stack = unsafe { EvmStack::from_mut_ptr(parts.stack.cast()) };
        let bytecode = parts.bytecode.original_byte_slice() as *const [u8];
        let calldatasize = parts.message.input.len();
        let spec_id = to_revm_spec_id(parts.spec);
        let mut input_scratch = Box::new(InputsImpl {
            target_address: parts.message.destination,
            bytecode_address: Some(parts.message.code_address),
            caller_address: parts.message.caller,
            input: CallInput::Bytes(RevmBytes::copy_from_slice(parts.message.input.as_ref())),
            call_value: parts.message.value,
        });
        let input = input_scratch.as_mut() as *mut InputsImpl;
        let mut next_action_scratch = Box::new(None);
        let next_action = next_action_scratch.as_mut() as *mut Option<InterpreterAction> as *mut ();
        let mut host_adapter =
            Box::new(RevmHostAdapter::new(host, parts.tx_env, parts.version, spec_id));
        let revm_host = RevmHostPtr::from_host(host_adapter.as_mut());
        let mut this = Self {
            memory: parts.memory,
            input,
            gas: revm_gas_from_evm2(parts.gas),
            host: revm_host,
            next_action,
            return_data: parts.return_data.as_ref(),
            is_static: parts.is_static,
            spec_id,
            resume_at,
            bytecode,
            on_log: None,
            calldatasize,
            exit_result: InstructionResult::Stop,
            exit_sp: ptr::null_mut(),
            gas_params: RevmGasParams::new_spec(spec_id),
            mem_base: ptr::null_mut(),
            mem_len: 0,
            tx_env: parts.tx_env,
            message: parts.message,
            input_scratch,
            next_action_scratch,
            _host_adapter: host_adapter,
        };
        this.refresh_memory_cache();
        (this, stack, parts.stack_len)
    }

    /// Returns the context state that must be copied back into an interpreter.
    #[inline]
    pub fn interpreter_state(&self) -> InterpreterState {
        InterpreterState {
            gas: evm2_gas_from_revm(self.gas),
            pc: self.resume_at.get(),
            clear_return_data: self.return_data.is_empty(),
        }
    }

    /// Stores context state back into an interpreter after compiled execution.
    #[inline]
    pub fn store_interpreter_state(self, interpreter: &mut Interpreter<'_, T>) {
        self.interpreter_state().store(interpreter);
    }

    /// Refreshes the cached memory base pointer and length from [`Memory`].
    #[inline]
    pub fn refresh_memory_cache(&mut self) {
        let slice = unsafe { &mut *self.memory }.as_mut_slice();
        self.mem_base = slice.as_mut_ptr();
        self.mem_len = slice.len();
    }

    /// Returns the input shim visible to compiled code.
    #[inline]
    pub fn input(&self) -> &InputsImpl {
        &self.input_scratch
    }
}

/// Returns the bytecode bytes for CODECOPY-compatible runtime access.
#[inline]
pub fn bytecode_slice(bytecode: &Bytecode) -> &[u8] {
    bytecode.original_byte_slice()
}

fn revm_gas_from_evm2(gas: Evm2Gas) -> RevmGas {
    let mut revm_gas = RevmGas::new_with_regular_gas_and_reservoir(gas.limit(), gas.reservoir());
    revm_gas.set_remaining(gas.remaining());
    revm_gas.set_state_gas_spent(i64::try_from(gas.state_gas_spent()).unwrap_or(i64::MAX));
    revm_gas.set_refund(gas.refunded());
    revm_gas.memory_mut().words_num = gas.memory().words_num;
    revm_gas.memory_mut().expansion_cost = gas.memory().expansion_cost;
    revm_gas
}

fn evm2_gas_from_revm(gas: RevmGas) -> Evm2Gas {
    let mut evm2_gas = Evm2Gas::new_with_regular_gas_and_reservoir(gas.limit(), gas.reservoir());
    evm2_gas.set_remaining(gas.remaining());
    evm2_gas.set_state_gas_spent(u64::try_from(gas.state_gas_spent()).unwrap_or(0));
    evm2_gas.set_refunded(gas.refunded());
    evm2_gas.memory_mut().words_num = gas.memory().words_num;
    evm2_gas.memory_mut().expansion_cost = gas.memory().expansion_cost;
    evm2_gas
}

fn to_revm_spec_id(spec_id: SpecId) -> RevmSpecId {
    let spec_id = u8::try_from(u32::from(spec_id)).expect("evm2 SpecId does not fit in u8");
    RevmSpecId::try_from_u8(spec_id).expect("evm2 SpecId has no revm equivalent")
}

/// Converts a revm-style compiled-code return into an evm2 instruction stop.
///
/// Compiled functions currently return revm's [`InstructionResult`] ABI. Do not cast the raw `u8`
/// value to [`InstrStop`]: evm2 intentionally uses a different layout for some invalid-opcode
/// variants.
#[inline]
pub const fn instr_stop_from_instruction_result(result: InstructionResult) -> Option<InstrStop> {
    Some(match result {
        InstructionResult::Stop => InstrStop::Stop,
        InstructionResult::Return => InstrStop::Return,
        InstructionResult::SelfDestruct => InstrStop::SelfDestruct,
        InstructionResult::Suspend => return None,
        InstructionResult::Revert => InstrStop::Revert,
        InstructionResult::CallTooDeep => InstrStop::CallTooDeep,
        InstructionResult::OutOfFunds => InstrStop::OutOfFunds,
        InstructionResult::CreateInitCodeStartingEF00 => InstrStop::CreateInitCodeStartingEF00,
        InstructionResult::InvalidEOFInitCode => InstrStop::InvalidEOFInitCode,
        InstructionResult::InvalidExtDelegateCallTarget => InstrStop::InvalidExtDelegateCallTarget,
        InstructionResult::OutOfGas => InstrStop::OutOfGas,
        InstructionResult::MemoryOOG => InstrStop::MemoryOOG,
        InstructionResult::MemoryLimitOOG => InstrStop::MemoryLimitOOG,
        InstructionResult::PrecompileOOG => InstrStop::PrecompileOOG,
        InstructionResult::InvalidOperandOOG => InstrStop::InvalidOperandOOG,
        InstructionResult::ReentrancySentryOOG => InstrStop::ReentrancySentryOOG,
        InstructionResult::OpcodeNotFound | InstructionResult::InvalidFEOpcode => {
            InstrStop::InvalidOpcode
        }
        InstructionResult::CallNotAllowedInsideStatic => InstrStop::CallNotAllowedInsideStatic,
        InstructionResult::StateChangeDuringStaticCall => InstrStop::StateChangeDuringStaticCall,
        InstructionResult::InvalidJump => InstrStop::InvalidJump,
        InstructionResult::NotActivated => InstrStop::NotActivated,
        InstructionResult::StackUnderflow => InstrStop::StackUnderflow,
        InstructionResult::StackOverflow => InstrStop::StackOverflow,
        InstructionResult::OutOfOffset => InstrStop::OutOfOffset,
        InstructionResult::CreateCollision => InstrStop::CreateCollision,
        InstructionResult::OverflowPayment => InstrStop::OverflowPayment,
        InstructionResult::PrecompileError => InstrStop::PrecompileError,
        InstructionResult::NonceOverflow => InstrStop::NonceOverflow,
        InstructionResult::CreateContractSizeLimit => InstrStop::CreateContractSizeLimit,
        InstructionResult::CreateContractStartingWithEF => InstrStop::CreateContractStartingWithEF,
        InstructionResult::CreateInitCodeSizeLimit => InstrStop::CreateInitCodeSizeLimit,
        InstructionResult::FatalExternalError => InstrStop::FatalExternalError,
        InstructionResult::InvalidImmediateEncoding => InstrStop::InvalidImmediateEncoding,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::Address;
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
    fn maps_instruction_result_to_instr_stop_by_semantics() {
        assert_eq!(
            instr_stop_from_instruction_result(InstructionResult::Stop),
            Some(InstrStop::Stop)
        );
        assert_eq!(
            instr_stop_from_instruction_result(InstructionResult::OpcodeNotFound),
            Some(InstrStop::InvalidOpcode)
        );
        assert_eq!(
            instr_stop_from_instruction_result(InstructionResult::InvalidFEOpcode),
            Some(InstrStop::InvalidOpcode)
        );
        assert_eq!(instr_stop_from_instruction_result(InstructionResult::Suspend), None);
    }

    #[test]
    fn evm2_context_matches_imported_context_offsets() {
        assert_eq!(
            offset_of!(EvmContext<'_, BaseEvmTypes>, input),
            offset_of!(crate::EvmContext<'_>, input)
        );
        assert_eq!(
            offset_of!(EvmContext<'_, BaseEvmTypes>, gas),
            offset_of!(crate::EvmContext<'_>, gas)
        );
        assert_eq!(
            offset_of!(EvmContext<'_, BaseEvmTypes>, spec_id),
            offset_of!(crate::EvmContext<'_>, spec_id)
        );
        assert_eq!(
            offset_of!(EvmContext<'_, BaseEvmTypes>, mem_base),
            offset_of!(crate::EvmContext<'_>, mem_base)
        );
        assert_eq!(
            offset_of!(EvmContext<'_, BaseEvmTypes>, mem_len),
            offset_of!(crate::EvmContext<'_>, mem_len)
        );
    }

    #[test]
    fn converts_gas_between_evm2_and_jit_abi() {
        let mut gas = Evm2Gas::new_with_regular_gas_and_reservoir(100, 20);
        gas.set_remaining(77);
        gas.set_state_gas_spent(11);
        gas.set_refunded(3);
        gas.memory_mut().words_num = 4;
        gas.memory_mut().expansion_cost = 12;

        let revm_gas = revm_gas_from_evm2(gas);
        assert_eq!(revm_gas.limit(), 100);
        assert_eq!(revm_gas.remaining(), 77);
        assert_eq!(revm_gas.reservoir(), 20);
        assert_eq!(revm_gas.state_gas_spent(), 11);
        assert_eq!(revm_gas.refunded(), 3);
        assert_eq!(revm_gas.memory().words_num, 4);
        assert_eq!(revm_gas.memory().expansion_cost, 12);

        let gas = evm2_gas_from_revm(revm_gas);
        assert_eq!(gas.limit(), 100);
        assert_eq!(gas.remaining(), 77);
        assert_eq!(gas.reservoir(), 20);
        assert_eq!(gas.state_gas_spent(), 11);
        assert_eq!(gas.refunded(), 3);
        assert_eq!(gas.memory().words_num, 4);
        assert_eq!(gas.memory().expansion_cost, 12);
    }

    #[test]
    fn evm2_context_revm_host_slot_uses_evm2_env() {
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
        let ecx = EvmContext::from_interpreter(&mut interpreter, &mut host);
        assert!(!ecx.next_action.is_null());

        let revm_host = unsafe { ecx.host.as_host_mut() };
        assert_eq!(revm_host.caller(), tx_origin);
        assert_eq!(revm_host.effective_gas_price(), Word::from(7));
        assert_eq!(revm_host.block_number(), Word::from(9));
        assert_eq!(revm_host.beneficiary(), beneficiary);
    }
}
