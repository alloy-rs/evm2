//! evm2-facing runtime context.

use crate::{EvmStack, EvmWord, InstrStop};
use alloc::{borrow::Cow, boxed::Box, vec::Vec};
use alloy_primitives::{Bytes as RevmBytes, U256};
use core::{
    cmp::min,
    fmt,
    marker::PhantomData,
    mem,
    ops::Range,
    ptr::{self, NonNull},
};
use evm2::{
    BaseEvmTypes, EvmFeatures, EvmTypes, SpecId, Version,
    bytecode::Bytecode,
    env::{BlockEnv, TxEnv},
    interpreter::{Gas as Evm2Gas, Host as Evm2Host, Interpreter, Message, MessageKind, Word},
    version::GasId,
};
use revm_interpreter::{
    CallInput, Gas as RevmGas, Host as RevmHost, InputsImpl, SStoreResult as RevmSStoreResult,
    SelfDestructResult as RevmSelfDestructResult, SharedMemory, StateLoad as RevmStateLoad,
    bytecode::Bytecode as RevmBytecode,
    context_interface::{
        cfg::GasParams as RevmGasParams, primitives::hardfork::SpecId as RevmSpecId,
    },
    host::LoadError,
    interpreter_types::MemoryTr,
    state::AccountInfo as RevmAccountInfo,
};

const _: () = {
    assert!(core::mem::size_of::<EvmWord>() == core::mem::size_of::<Word>());
    assert!(core::mem::align_of::<EvmWord>() == core::mem::align_of::<Word>());
};

/// Serialized host trait object slot.
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
    fn basefee(&self) -> U256 {
        self.block_env.basefee
    }

    fn blob_gasprice(&self) -> U256 {
        self.block_env.blob_basefee
    }

    fn gas_limit(&self) -> U256 {
        self.block_env.gas_limit
    }

    fn difficulty(&self) -> U256 {
        self.block_env.difficulty
    }

    fn prevrandao(&self) -> Option<U256> {
        Some(self.block_env.prevrandao)
    }

    fn block_number(&self) -> U256 {
        self.block_env.number
    }

    fn timestamp(&self) -> U256 {
        self.block_env.timestamp
    }

    fn beneficiary(&self) -> alloy_primitives::Address {
        self.block_env.beneficiary
    }

    fn slot_num(&self) -> U256 {
        self.block_env.slot_num
    }

    fn chain_id(&self) -> U256 {
        self.tx_env.chain_id
    }

    fn effective_gas_price(&self) -> U256 {
        self.tx_env.gas_price
    }

    fn caller(&self) -> alloy_primitives::Address {
        self.tx_env.origin
    }

    fn blob_hash(&self, number: usize) -> Option<U256> {
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
        key: U256,
        value: U256,
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
        key: U256,
        skip_cold_load: bool,
    ) -> Result<RevmStateLoad<U256>, LoadError> {
        let result = self
            .host_mut()
            .sload(&address, &key, skip_cold_load)
            .map_err(|stop| load_error(stop, skip_cold_load))?;
        Ok(RevmStateLoad::new(result.value, result.is_cold))
    }

    fn tstore(&mut self, address: alloy_primitives::Address, key: U256, value: U256) {
        self.host_mut().tstore(&address, &key, &value);
    }

    fn tload(&mut self, address: alloy_primitives::Address, key: U256) -> U256 {
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
#[repr(C)]
pub struct EvmContext<'a, T: EvmTypes = BaseEvmTypes> {
    /// The memory.
    pub memory: *mut SharedMemory,
    /// Input information (target address, caller, input data, call value).
    pub input: *mut InputsImpl,
    /// The gas.
    pub gas: RevmGas,
    /// Host trait object slot consumed by host-touching builtins.
    pub host: RevmHostPtr,
    /// The return data.
    pub return_data: &'a [u8],
    /// Whether the context is static.
    pub is_static: bool,
    /// The raw spec ID for the current execution.
    pub spec_id: u8,
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
    /// Cached gas parameters for builtin gas accounting.
    pub gas_params: RevmGasParams,
    /// Cached base pointer for the current memory context.
    pub mem_base: *mut u8,
    /// Cached length of the current memory context in bytes.
    pub mem_len: usize,
    /// Output produced by RETURN or REVERT.
    #[doc(hidden)]
    pub output: RevmBytes,
    /// Recursive evm2 call/create dispatch used by call-like builtins.
    #[doc(hidden)]
    pub evm2_recursion: crate::Evm2Recursion,
    /// Transaction-global environment.
    #[doc(hidden)]
    pub tx_env: &'a TxEnv<T>,
    /// Frame-local call/create message.
    #[doc(hidden)]
    pub message: &'a Message<T>,
    return_data_scratch: RevmBytes,
    memory_scratch: Box<SharedMemory>,
    input_scratch: Box<InputsImpl>,
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
    assert!(
        offset_of!(EvmContext<'_, BaseEvmTypes>, output)
            == offset_of!(crate::EvmContext<'_>, output)
    );
    assert!(
        offset_of!(EvmContext<'_, BaseEvmTypes>, evm2_recursion)
            == offset_of!(crate::EvmContext<'_>, evm2_recursion)
    );
};

/// Interpreter state copied out of a JIT context after compiled execution.
#[derive(Clone, Debug)]
#[doc(hidden)]
pub struct InterpreterState {
    gas: Evm2Gas,
    return_data: Vec<u8>,
    memory: Vec<u8>,
    output: Option<Vec<u8>>,
}

impl InterpreterState {
    /// Stores this state back into an evm2 interpreter.
    #[inline]
    pub fn store<T: EvmTypes>(self, interpreter: &mut Interpreter<'_, T>) {
        interpreter.set_gas(self.gas);
        interpreter.set_return_data(self.return_data.into());
        let parts = interpreter.jit_context_parts_mut();
        parts.memory.clear();
        parts
            .memory
            .resize(0, self.memory.len())
            .expect("JIT memory snapshot exceeds evm2 memory limit");
        parts.memory.set(0, &self.memory);
        if let Some(output) = self.output {
            interpreter.set_output_bytes_for_jit(&output);
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
) -> InstrStop;

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
    /// # Safety
    ///
    /// The caller must ensure that the function is safe to call for this interpreter state.
    pub unsafe fn call_with_interpreter<'a, 'frame: 'a>(
        self,
        interpreter: &'a mut Interpreter<'frame, T>,
        host: &'a mut T::Host,
    ) -> InstrStop {
        let (mut ecx, stack, stack_len) =
            EvmContext::from_interpreter_with_stack(interpreter, host);
        let result = unsafe { self.call(stack, stack_len, &mut ecx) };
        if result == InstrStop::OutOfGas {
            ecx.gas.spend_all();
        }

        let mut state = ecx.interpreter_state();
        if matches!(result, InstrStop::Return | InstrStop::Revert) {
            state.output = Some(ecx.output.to_vec());
        }
        drop(ecx);
        state.store(interpreter);
        result
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
    ) -> InstrStop {
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
        let parts = interpreter.jit_context_parts_mut();
        let stack = unsafe { EvmStack::from_mut_ptr(parts.stack.cast()) };
        let mut memory_scratch = Box::new(SharedMemory::new());
        memory_scratch.set_memory_limit(parts.version.memory_limit);
        let memory_bytes = parts.memory.as_slice();
        memory_scratch.resize(memory_bytes.len());
        memory_scratch.set(0, memory_bytes);
        let memory = memory_scratch.as_mut() as *mut SharedMemory;
        let bytecode = parts.bytecode.original_byte_slice() as *const [u8];
        let calldatasize = parts.message.input.len();
        let spec_id = spec_id_byte(parts.spec);
        let revm_spec_id = to_revm_spec_id(parts.spec);
        let mut input_scratch = Box::new(InputsImpl {
            target_address: parts.message.destination,
            bytecode_address: Some(parts.message.code_address),
            caller_address: parts.message.caller,
            input: CallInput::Bytes(RevmBytes::copy_from_slice(parts.message.input.as_ref())),
            call_value: parts.message.value,
        });
        let input = input_scratch.as_mut() as *mut InputsImpl;
        let mut host_adapter =
            Box::new(RevmHostAdapter::new(host, parts.tx_env, parts.version, revm_spec_id));
        let revm_host = RevmHostPtr::from_host(host_adapter.as_mut());
        let mut this = Self {
            memory,
            input,
            gas: revm_gas_from_evm2(parts.gas),
            host: revm_host,
            return_data: parts.return_data.as_ref(),
            is_static: parts.is_static,
            spec_id,
            bytecode,
            on_log: None,
            calldatasize,
            exit_result: InstrStop::Stop,
            exit_sp: ptr::null_mut(),
            gas_params: RevmGasParams::new_spec(revm_spec_id),
            mem_base: ptr::null_mut(),
            mem_len: 0,
            output: RevmBytes::new(),
            evm2_recursion: crate::Evm2Recursion::new(
                evm2_recursive_create::<T>,
                evm2_recursive_call::<T>,
            ),
            tx_env: parts.tx_env,
            message: parts.message,
            return_data_scratch: RevmBytes::new(),
            memory_scratch,
            input_scratch,
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
            return_data: self.return_data.to_vec(),
            memory: unsafe { &*self.memory }.context_memory().to_vec(),
            output: None,
        }
    }

    /// Stores context state back into an interpreter after compiled execution.
    #[inline]
    pub fn store_interpreter_state(self, interpreter: &mut Interpreter<'_, T>) {
        self.interpreter_state().store(interpreter);
    }

    /// Refreshes the cached memory base pointer and length from the evm2 memory snapshot.
    #[inline]
    pub fn refresh_memory_cache(&mut self) {
        let mut slice = unsafe { &mut *self.memory }.context_memory_mut();
        self.mem_base = slice.as_mut_ptr();
        self.mem_len = slice.len();
    }

    /// Returns the input shim visible to compiled code.
    #[inline]
    pub fn input(&self) -> &InputsImpl {
        &self.input_scratch
    }

    fn set_return_data(&mut self, data: RevmBytes) {
        self.return_data_scratch = data;
        self.return_data = unsafe { &*(self.return_data_scratch.as_ref() as *const [u8]) };
    }
}

/// Returns the bytecode bytes for CODECOPY-compatible runtime access.
#[inline]
pub fn bytecode_slice(bytecode: &Bytecode) -> &[u8] {
    bytecode.original_byte_slice()
}

unsafe fn evm2_recursive_call<T: EvmTypes>(
    ecx: &mut crate::EvmContext<'_>,
    sp: *mut EvmWord,
    call_kind: u8,
) -> Result<(), InstrStop> {
    let ecx = unsafe { evm2_context_from_base::<T>(ecx) };
    call_kind_from_u8(call_kind).and_then(|kind| call_inner(ecx, sp, kind))
}

unsafe fn evm2_recursive_create<T: EvmTypes>(
    ecx: &mut crate::EvmContext<'_>,
    sp: *mut EvmWord,
    create_kind: u8,
) -> Result<(), InstrStop> {
    let ecx = unsafe { evm2_context_from_base::<T>(ecx) };
    create_kind_from_u8(create_kind).and_then(|is_create2| create_inner(ecx, sp, is_create2))
}

unsafe fn evm2_context_from_base<'a, 'ctx, T: EvmTypes>(
    ecx: &'a mut crate::EvmContext<'ctx>,
) -> &'a mut EvmContext<'ctx, T> {
    unsafe { &mut *(ptr::from_mut(ecx).cast::<EvmContext<'ctx, T>>()) }
}

fn call_kind_from_u8(kind: u8) -> Result<MessageKind, InstrStop> {
    match kind {
        0 => Ok(MessageKind::Call),
        1 => Ok(MessageKind::CallCode),
        2 => Ok(MessageKind::DelegateCall),
        3 => Ok(MessageKind::StaticCall),
        _ => Err(InstrStop::FatalExternalError),
    }
}

fn create_kind_from_u8(kind: u8) -> Result<bool, InstrStop> {
    match kind {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(InstrStop::FatalExternalError),
    }
}

fn word_to_usize(value: EvmWord) -> Result<usize, InstrStop> {
    value.to_u256().try_into().map_err(|_| InstrStop::InvalidOperandOOG)
}

fn word_to_usize_saturated(value: EvmWord) -> usize {
    value.to_u256().try_into().unwrap_or(usize::MAX)
}

fn ensure_memory<T: EvmTypes>(
    ecx: &mut EvmContext<'_, T>,
    offset: usize,
    len: usize,
) -> Result<(), InstrStop> {
    crate::resize_memory(&mut ecx.gas, unsafe { &mut *ecx.memory }, &ecx.gas_params, offset, len)?;
    ecx.refresh_memory_cache();
    Ok(())
}

fn resize_memory_range<T: EvmTypes>(
    ecx: &mut EvmContext<'_, T>,
    offset: EvmWord,
    len: EvmWord,
) -> Result<Range<usize>, InstrStop> {
    let len = word_to_usize(len)?;
    let offset = if len != 0 {
        let offset = word_to_usize(offset)?;
        ensure_memory(ecx, offset, len)?;
        offset
    } else {
        usize::MAX
    };
    Ok(offset..offset + len)
}

fn memory_range_bytes<T: EvmTypes>(ecx: &mut EvmContext<'_, T>, range: Range<usize>) -> RevmBytes {
    if range.is_empty() {
        return RevmBytes::new();
    }
    RevmBytes::copy_from_slice(&unsafe { &*ecx.memory }.slice(range))
}

fn get_memory_input_and_out_ranges<T: EvmTypes>(
    ecx: &mut EvmContext<'_, T>,
    input_offset: EvmWord,
    input_len: EvmWord,
    return_offset: EvmWord,
    return_len: EvmWord,
) -> Result<(Range<usize>, Range<usize>), InstrStop> {
    let input = resize_memory_range(ecx, input_offset, input_len)?;
    let output = resize_memory_range(ecx, return_offset, return_len)?;
    Ok((input, output))
}

fn spend(gas: &mut RevmGas, cost: u64) -> Result<(), InstrStop> {
    gas.record_regular_cost(cost).then_some(()).ok_or(InstrStop::OutOfGas)
}

fn should_charge_new_account_gas(
    eip161: bool,
    transfers_value: bool,
    target_is_empty_for_new_account_gas: bool,
) -> bool {
    target_is_empty_for_new_account_gas && (!eip161 || transfers_value)
}

fn load_acc_and_calc_gas<T: EvmTypes>(
    ecx: &mut EvmContext<'_, T>,
    to: alloy_primitives::Address,
    transfers_value: bool,
    create_empty_account: bool,
    stack_gas_limit: u64,
) -> Result<(u64, Bytecode, alloy_primitives::Address, bool), InstrStop> {
    if transfers_value {
        spend(
            &mut ecx.gas,
            ecx._host_adapter.version.gas_params.get(GasId::TransferValueCost).into(),
        )?;
    }

    let additional_cold_cost = ecx._host_adapter.version.gas_params.cold_account_additional_cost();
    let remaining_gas = ecx.gas.remaining();
    let skip_cold_load = remaining_gas < additional_cold_cost;
    let account = ecx._host_adapter.host_mut().load_account(&to, true, skip_cold_load)?;

    let mut cost = 0;
    if account.is_cold {
        cost += additional_cold_cost;
    }
    let mut code = account.code;
    let mut code_address = to;
    if ecx._host_adapter.version.features.contains(EvmFeatures::EIP7702)
        && let Some(delegated_address) = code.eip7702_address()
    {
        cost += u64::from(ecx._host_adapter.version.gas_params.get(GasId::WarmStorageReadCost));
        if cost > remaining_gas {
            return Err(InstrStop::OutOfGas);
        }
        let skip_cold_load = remaining_gas < cost.saturating_add(additional_cold_cost);
        let delegated_account =
            ecx._host_adapter.host_mut().load_account(&delegated_address, true, skip_cold_load)?;
        if delegated_account.is_cold {
            cost += additional_cold_cost;
        }
        code = delegated_account.code;
        code_address = delegated_address;
    }
    let features = ecx._host_adapter.version.features;
    if create_empty_account
        && should_charge_new_account_gas(
            features.contains(EvmFeatures::EIP161),
            transfers_value,
            ecx._host_adapter.host_mut().target_is_empty_for_new_account_gas(&to, features)?,
        )
    {
        cost += u64::from(ecx._host_adapter.version.gas_params.get(GasId::NewAccountCost));
    }
    spend(&mut ecx.gas, cost)?;

    let mut gas_limit = if ecx._host_adapter.version.features.contains(EvmFeatures::EIP150) {
        min(
            ecx._host_adapter.version.gas_params.call_stipend_reduction(ecx.gas.remaining()),
            stack_gas_limit,
        )
    } else {
        stack_gas_limit
    };
    spend(&mut ecx.gas, gas_limit)?;

    if transfers_value {
        gas_limit = gas_limit
            .saturating_add(ecx._host_adapter.version.gas_params.get(GasId::CallStipend).into());
    }

    let disable_precompiles = code_address != to;
    Ok((gas_limit, code, code_address, disable_precompiles))
}

unsafe fn pop_word(sp: &mut *mut EvmWord) -> EvmWord {
    *sp = unsafe { (*sp).sub(1) };
    unsafe { **sp }
}

fn call_inner<T: EvmTypes>(
    ecx: &mut EvmContext<'_, T>,
    sp: *mut EvmWord,
    kind: MessageKind,
) -> Result<(), InstrStop> {
    let inputs = match kind {
        MessageKind::Call | MessageKind::CallCode => 7,
        MessageKind::DelegateCall | MessageKind::StaticCall => 6,
        _ => unreachable!("invalid call message kind"),
    };
    let mut cursor = unsafe { sp.add(inputs) };
    let local_gas_limit = unsafe { pop_word(&mut cursor) };
    let to = unsafe { pop_word(&mut cursor) }.to_address();
    let value = if matches!(kind, MessageKind::Call | MessageKind::CallCode) {
        unsafe { pop_word(&mut cursor) }.to_u256()
    } else {
        Word::ZERO
    };
    let input_offset = unsafe { pop_word(&mut cursor) };
    let input_len = unsafe { pop_word(&mut cursor) };
    let return_offset = unsafe { pop_word(&mut cursor) };
    let return_len = unsafe { pop_word(&mut cursor) };

    let has_transfer = !value.is_zero();
    if ecx.is_static && kind == MessageKind::Call && has_transfer {
        return Err(InstrStop::CallNotAllowedInsideStatic);
    }

    let local_gas_limit = word_to_usize_saturated(local_gas_limit) as u64;
    let (input_range, return_memory_range) =
        get_memory_input_and_out_ranges(ecx, input_offset, input_len, return_offset, return_len)?;
    let (gas_limit, loaded_code, resolved_code_address, disable_precompiles) =
        load_acc_and_calc_gas(ecx, to, has_transfer, kind == MessageKind::Call, local_gas_limit)?;
    let input = memory_range_bytes(ecx, input_range);

    let current = ecx.message;
    let (destination, caller, call_value, code_address) = match kind {
        MessageKind::Call => (to, current.destination, value, resolved_code_address),
        MessageKind::CallCode => {
            (current.destination, current.destination, value, resolved_code_address)
        }
        MessageKind::DelegateCall => {
            (current.destination, current.caller, current.value, resolved_code_address)
        }
        MessageKind::StaticCall => (to, current.destination, Word::ZERO, resolved_code_address),
        _ => unreachable!("invalid call message kind"),
    };
    let mut message = Message {
        kind,
        depth: current.depth.saturating_add(1),
        gas_limit,
        destination,
        caller,
        input,
        value: call_value,
        code_address,
        disable_precompiles,
        salt: alloy_primitives::B256::ZERO,
        ext: T::MessageExt::default(),
        _non_exhaustive: (),
    };

    let caller_is_static = ecx.is_static;
    let mut result = ecx._host_adapter.host_mut().execute_message(
        ecx.tx_env,
        loaded_code,
        &mut message,
        caller_is_static,
    );
    ecx.gas.erase_cost(result.gas_returned_to_parent());
    ecx.gas.record_refund(result.refund_propagated_to_parent());

    let copy_len = min(return_memory_range.len(), result.output.len());
    if copy_len != 0 {
        unsafe { &mut *ecx.memory }.set(return_memory_range.start, &result.output[..copy_len]);
    }
    let success = EvmWord::from(Word::from(u8::from(result.stop.is_success())));
    unsafe {
        sp.write(success);
    }
    ecx.set_return_data(core::mem::take(&mut result.output));
    Ok(())
}

fn create_inner<T: EvmTypes>(
    ecx: &mut EvmContext<'_, T>,
    sp: *mut EvmWord,
    is_create2: bool,
) -> Result<(), InstrStop> {
    if ecx.is_static {
        return Err(InstrStop::StateChangeDuringStaticCall);
    }

    let inputs = if is_create2 { 4 } else { 3 };
    let mut cursor = unsafe { sp.add(inputs) };
    let value = unsafe { pop_word(&mut cursor) }.to_u256();
    let offset = unsafe { pop_word(&mut cursor) };
    let len_word = unsafe { pop_word(&mut cursor) };
    let salt = if is_create2 { Some(unsafe { pop_word(&mut cursor) }) } else { None };

    let len = word_to_usize(len_word)?;
    if ecx._host_adapter.version.features.contains(EvmFeatures::EIP3860) {
        if len > ecx._host_adapter.version.max_initcode_size {
            return Err(InstrStop::CreateInitCodeSizeLimit);
        }
        spend(&mut ecx.gas, ecx._host_adapter.version.gas_params.initcode_cost(len))?;
    }
    let code_range = resize_memory_range(ecx, offset, EvmWord::from(Word::from(len)))?;
    let input = memory_range_bytes(ecx, code_range);
    let create_cost = if is_create2 {
        ecx._host_adapter.version.gas_params.create2_cost(len)
    } else {
        ecx._host_adapter.version.gas_params.get(GasId::Create).into()
    };
    spend(&mut ecx.gas, create_cost)?;
    let gas_limit = if ecx._host_adapter.version.features.contains(EvmFeatures::EIP150) {
        ecx._host_adapter.version.gas_params.call_stipend_reduction(ecx.gas.remaining())
    } else {
        ecx.gas.remaining()
    };
    spend(&mut ecx.gas, gas_limit)?;

    let current = ecx.message;
    let mut message = Message {
        kind: if is_create2 { MessageKind::Create2 } else { MessageKind::Create },
        depth: current.depth.saturating_add(1),
        gas_limit,
        destination: current.destination,
        caller: current.destination,
        input,
        value,
        code_address: current.destination,
        disable_precompiles: false,
        salt: salt.map(|salt| alloy_primitives::B256::from(salt.to_be_bytes())).unwrap_or_default(),
        ext: T::MessageExt::default(),
        _non_exhaustive: (),
    };
    let bytecode = Bytecode::new_legacy(message.input.clone());
    let result =
        ecx._host_adapter.host_mut().execute_message(ecx.tx_env, bytecode, &mut message, false);
    ecx.gas.erase_cost(result.gas_returned_to_parent());
    ecx.gas.record_refund(result.refund_propagated_to_parent());

    let return_data =
        if result.stop == InstrStop::Revert { result.output } else { RevmBytes::new() };
    let address = result
        .created_address
        .filter(|_| result.stop.is_success())
        .map(|address| EvmWord::from_be_slice(address.as_slice()))
        .unwrap_or_default();
    unsafe {
        sp.write(address);
    }
    ecx.set_return_data(return_data);
    Ok(())
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

fn spec_id_byte(spec_id: SpecId) -> u8 {
    u8::try_from(u32::from(spec_id)).expect("evm2 SpecId does not fit in u8")
}

fn to_revm_spec_id(spec_id: SpecId) -> RevmSpecId {
    let spec_id = spec_id_byte(spec_id);
    RevmSpecId::try_from_u8(spec_id).expect("evm2 SpecId has no revm equivalent")
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{Address, B256, Bytes as AlloyBytes, Log};
    use core::mem::offset_of;
    use evm2::{
        BaseEvmConfigSelector, Evm, EvmConfigSelector, EvmFeatures, EvmTypes, Precompiles,
        bytecode::Bytecode,
        env::{BlockEnv, TxEnv},
        ethereum::ethereum_tx_registry,
        evm::{AccountLoad, EmptyDB, SLoad, SStore, SelfDestructResult},
        interpreter::{GasTracker, Host, Message, MessageKind, MessageResult, op},
    };

    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
    struct TestTypes;

    impl EvmTypes for TestTypes {
        type ConfigSelector = BaseEvmConfigSelector;
        type SpecId = SpecId;
        type Tx = ();
        type MessageExt = ();
        type MessageResultExt = ();
        type TxEnvExt = ();
        type TxResultExt = ();
        type BlockEnvExt = ();
        type Host = TestHost;
    }

    #[derive(Debug)]
    struct TestHost {
        block: BlockEnv<TestTypes>,
        code: AlloyBytes,
        execute_result: MessageResult<TestTypes>,
        calls: Vec<Message<TestTypes>>,
        call_static_flags: Vec<bool>,
    }

    impl Default for TestHost {
        fn default() -> Self {
            Self {
                block: BlockEnv::default(),
                code: AlloyBytes::new(),
                execute_result: MessageResult {
                    stop: InstrStop::Return,
                    ..MessageResult::default()
                },
                calls: Vec::new(),
                call_static_flags: Vec::new(),
            }
        }
    }

    impl Host<TestTypes> for TestHost {
        fn spec_id(&self) -> SpecId {
            SpecId::CANCUN
        }

        fn block_env(&mut self) -> &BlockEnv<TestTypes> {
            &self.block
        }

        fn load_account(
            &mut self,
            address: &Address,
            load_code: bool,
            _skip_cold_load: bool,
        ) -> Result<AccountLoad, InstrStop> {
            Ok(AccountLoad {
                balance: address.into_word().into(),
                code_hash: B256::ZERO,
                code: if load_code {
                    Bytecode::new_legacy(self.code.clone())
                } else {
                    Bytecode::default()
                },
                exists: true,
                is_empty: false,
                is_cold: false,
                _non_exhaustive: (),
            })
        }

        fn target_is_empty_for_new_account_gas(
            &mut self,
            _address: &Address,
            _features: EvmFeatures,
        ) -> Result<bool, InstrStop> {
            Ok(false)
        }

        fn block_hash(&mut self, number: &Word) -> Result<Option<B256>, InstrStop> {
            Ok(Some(B256::with_last_byte(number.wrapping_to::<u8>())))
        }

        fn sload(
            &mut self,
            _address: &Address,
            _key: &Word,
            _skip_cold_load: bool,
        ) -> Result<SLoad, InstrStop> {
            Ok(SLoad { value: Word::ZERO, is_cold: false, _non_exhaustive: () })
        }

        fn sstore(
            &mut self,
            _address: &Address,
            _key: &Word,
            value: &Word,
            _skip_cold_load: bool,
        ) -> Result<SStore, InstrStop> {
            Ok(SStore {
                original_value: Word::ZERO,
                present_value: Word::ZERO,
                new_value: *value,
                is_cold: false,
                _non_exhaustive: (),
            })
        }

        fn tload(&mut self, _address: &Address, _key: &Word) -> Word {
            Word::ZERO
        }

        fn tstore(&mut self, _address: &Address, _key: &Word, _value: &Word) {}

        fn log(&mut self, _log: Log) {}

        fn execute_message(
            &mut self,
            _tx_env: &TxEnv<TestTypes>,
            _bytecode: Bytecode,
            message: &mut Message<TestTypes>,
            caller_is_static: bool,
        ) -> MessageResult<TestTypes> {
            self.call_static_flags
                .push(caller_is_static || message.kind == MessageKind::StaticCall);
            self.calls.push(message.clone());
            self.execute_result.clone()
        }

        fn selfdestruct(
            &mut self,
            _contract: &Address,
            _target: &Address,
            _skip_cold_load: bool,
        ) -> Result<SelfDestructResult, InstrStop> {
            Ok(SelfDestructResult::default())
        }
    }

    struct PreparedJitFrame<'a> {
        ecx: EvmContext<'a, TestTypes>,
        stack: &'a mut EvmStack,
    }

    fn address_word(address: &Address) -> EvmWord {
        EvmWord::from_be_slice(address.as_slice())
    }

    fn prepare_frame<'a>(
        interpreter: &'a mut Interpreter<'_, TestTypes>,
        host: &'a mut TestHost,
    ) -> PreparedJitFrame<'a> {
        let config = <BaseEvmConfigSelector as EvmConfigSelector<TestTypes>>::execution_config(
            SpecId::CANCUN,
        );
        interpreter.prepare_jit_run(&config, host);
        let (ecx, stack, _stack_len) = EvmContext::from_interpreter_with_stack(interpreter, host);
        PreparedJitFrame { ecx, stack }
    }

    fn base_context<'a, 'ctx, T: EvmTypes>(
        ecx: &'a mut EvmContext<'ctx, T>,
    ) -> &'a mut crate::EvmContext<'ctx> {
        unsafe { &mut *ptr::from_mut(ecx).cast::<crate::EvmContext<'_>>() }
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

        let revm_host = unsafe { ecx.host.as_host_mut() };
        assert_eq!(revm_host.caller(), tx_origin);
        assert_eq!(revm_host.effective_gas_price(), Word::from(7));
        assert_eq!(revm_host.block_number(), Word::from(9));
        assert_eq!(revm_host.beneficiary(), beneficiary);
    }

    #[test]
    fn evm2_context_uses_shared_memory_scratch() {
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
            let parts = interpreter.jit_context_parts_mut();
            parts.memory.resize(0, 3).unwrap();
            parts.memory.set(0, b"abc");
        }

        let ecx = EvmContext::from_interpreter(&mut interpreter, &mut host);
        assert_eq!(unsafe { &*ecx.memory }.context_memory().to_vec(), b"abc");
        unsafe { &mut *ecx.memory }.set(1, b"z");
        let state = ecx.interpreter_state();
        drop(ecx);
        state.store(&mut interpreter);

        assert_eq!(interpreter.memory_ref().slice(0, 3), b"azc");
    }

    #[test]
    fn evm2_recursive_call_executes_message() {
        let target = Address::from([0x22; 20]);
        let caller = Address::from([0x11; 20]);
        let child_output = AlloyBytes::from_static(&[0xaa, 0xbb, 0xcc]);
        let mut host = TestHost {
            execute_result: MessageResult {
                stop: InstrStop::Return,
                gas: GasTracker::new(37),
                output: child_output.clone(),
                ..MessageResult::default()
            },
            ..TestHost::default()
        };
        let tx_env = TxEnv::default();
        let message = Message { gas_limit: 1_000_000, destination: caller, ..Message::default() };
        let mut interpreter = Interpreter::<TestTypes>::new(
            Bytecode::new_legacy(AlloyBytes::from_static(&[op::STOP])),
            &tx_env,
            &message,
            false,
        );

        {
            let mut frame = prepare_frame(&mut interpreter, &mut host);
            unsafe { &mut *frame.ecx.memory }.resize(16);
            unsafe { &mut *frame.ecx.memory }.set(4, b"in");
            frame.stack.set(0, EvmWord::from(Word::from(2)));
            frame.stack.set(1, EvmWord::from(Word::from(8)));
            frame.stack.set(2, EvmWord::from(Word::from(2)));
            frame.stack.set(3, EvmWord::from(Word::from(4)));
            frame.stack.set(4, EvmWord::ZERO);
            frame.stack.set(5, address_word(&target));
            frame.stack.set(6, EvmWord::from(Word::from(50_000)));

            let sp = frame.stack.as_mut_ptr();
            let result =
                unsafe { evm2_recursive_call::<TestTypes>(base_context(&mut frame.ecx), sp, 0) };

            assert_eq!(result, Ok(()));
            assert_eq!(unsafe { frame.stack.get_unchecked(0) }.to_u256(), Word::from(1));
            assert_eq!(frame.ecx.return_data, child_output.as_ref());
            assert_eq!(&*unsafe { &*frame.ecx.memory }.slice(8..10), &[0xaa, 0xbb]);
            frame.ecx.output = RevmBytes::copy_from_slice(b"frame-output");
            assert_eq!(frame.ecx.return_data, child_output.as_ref());
        }

        assert_eq!(host.calls.len(), 1);
        assert_eq!(host.calls[0].kind, MessageKind::Call);
        assert_eq!(host.calls[0].destination, target);
        assert_eq!(host.calls[0].caller, caller);
        assert_eq!(host.calls[0].input.as_ref(), b"in");
        assert!(!host.call_static_flags[0]);
    }

    #[test]
    fn evm2_recursive_create_executes_message() {
        let created = Address::from([0x77; 20]);
        let initcode = [op::STOP];
        let mut host = TestHost {
            execute_result: MessageResult {
                stop: InstrStop::Return,
                gas: GasTracker::new(11),
                created_address: Some(created),
                ..MessageResult::default()
            },
            ..TestHost::default()
        };
        let tx_env = TxEnv::default();
        let message = Message { gas_limit: 1_000_000, ..Message::default() };
        let mut interpreter = Interpreter::<TestTypes>::new(
            Bytecode::new_legacy(AlloyBytes::from_static(&[op::STOP])),
            &tx_env,
            &message,
            false,
        );

        {
            let mut frame = prepare_frame(&mut interpreter, &mut host);
            unsafe { &mut *frame.ecx.memory }.resize(1);
            unsafe { &mut *frame.ecx.memory }.set(0, &initcode);
            frame.stack.set(0, EvmWord::from(Word::from(initcode.len())));
            frame.stack.set(1, EvmWord::ZERO);
            frame.stack.set(2, EvmWord::ZERO);

            let sp = frame.stack.as_mut_ptr();
            let result =
                unsafe { evm2_recursive_create::<TestTypes>(base_context(&mut frame.ecx), sp, 0) };

            assert_eq!(result, Ok(()));
            assert_eq!(unsafe { frame.stack.get_unchecked(0) }, &address_word(&created));
            assert!(frame.ecx.return_data.is_empty());
        }

        assert_eq!(host.calls.len(), 1);
        assert_eq!(host.calls[0].kind, MessageKind::Create);
        assert_eq!(host.calls[0].input.as_ref(), initcode);
    }

    unsafe extern "C" fn evm2_return_output(
        mut ecx: NonNull<EvmContext<'_, BaseEvmTypes>>,
        _stack: NonNull<EvmStack>,
        _stack_len: NonNull<usize>,
    ) -> InstrStop {
        let ecx = unsafe { ecx.as_mut() };
        ecx.output = RevmBytes::copy_from_slice(b"ok");
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
