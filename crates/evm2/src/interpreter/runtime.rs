use super::{
    BytecodeRef, Gas, Host, InstrStop, Memory, Message, MessageKind, Pc, Result, StackBacking,
    StackMut, StackRef, Word,
};
use crate::{
    EvmTypesHost, ExecutionConfig, ExecutionError, HostError, SpecId, Version,
    bytecode::{Bytecode, chunks::CODE_CHUNK_SIZE},
    env::TxEnv,
    evm::inspector::Inspector,
    interpreter::dispatch,
    trustme,
    version::{EvmFeatures, GasParams},
};
use alloc::{boxed::Box, vec::Vec};
use alloy_primitives::{Address, B256, Bytes};
use core::{fmt, hint::cold_path, ops::Range, ptr::NonNull};
use derive_where::derive_where;

/// EVM interpreter.
#[derive_where(Debug)]
pub struct Interpreter<'frame, 'host, T: EvmTypesHost> {
    pub(in crate::interpreter) bytecode: Bytecode,
    // Borrows the immutable allocations owned by `bytecode`. Cleared before replacing it;
    // accessors must shorten the erased lifetime to the borrow of this interpreter.
    bytecode_ref: Option<BytecodeRef<'static>>,
    code_hash: B256,
    code_size: usize,
    code_chunk_index: u32,
    chunked_code: bool,
    pub(in crate::interpreter) memory: Memory,
    pub(in crate::interpreter) return_data: Bytes,

    pub(in crate::interpreter) pc: *const u8,
    output: Range<u32>,
    #[derive_where(skip)]
    tx_env: Option<&'frame TxEnv<T>>,
    #[derive_where(skip)]
    message: Option<&'frame Message<T>>,
    host: Option<NonNull<T::Host<'host>>>,
    inspector: Option<NonNull<dyn Inspector<T> + 'host>>,
    pub(in crate::interpreter) stack_len: usize,
    #[derive_where(skip)]
    pub(in crate::interpreter) stack: Box<StackBacking>,

    pub(in crate::interpreter) gas: Gas,
    pub(in crate::interpreter) result: Result,
    error: Option<ExecutionError>,
    spec: SpecId,
    features: EvmFeatures,
    is_static: bool,
}

// SAFETY: The interpreter's internal pointers are always valid. `pc` and `bytecode_ref` point into
// immutable owned bytecode and its jump table. Frame-local references are cleared before pooling,
// and host/inspector pointers are installed for
// execution and not used after the owning execution context is gone. The `Sync` bounds make the
// retained shared frame references safe to transfer between threads.
unsafe impl<T> Send for Interpreter<'_, '_, T>
where
    T: EvmTypesHost,
    T::MessageExt: Sync,
    T::TxEnvExt: Sync,
{
}

impl<'frame, 'host, T: EvmTypesHost> Interpreter<'frame, 'host, T> {
    /// Creates an interpreter from a transaction-global environment and a frame-local message,
    /// which carries the bytecode to run.
    pub fn new(tx_env: &'frame TxEnv<T>, message: &'frame Message<T>) -> Self {
        // SAFETY: Calling init right after.
        let mut interp = unsafe { Self::uninit() };
        interp.init(tx_env, message);
        interp
    }

    /// # Safety
    ///
    /// Must call `init` before use.
    unsafe fn uninit() -> Self {
        let bytecode = Bytecode::new();
        Self {
            pc: bytecode.original_byte_slice().as_ptr(),
            bytecode,
            bytecode_ref: None,
            code_hash: B256::ZERO,
            code_size: 0,
            code_chunk_index: 0,
            chunked_code: false,
            stack_len: 0,
            gas: Gas::new(0),
            memory: Memory::new(),
            result: Ok(()),
            error: None,
            output: 0..0,
            tx_env: None,
            message: None,
            is_static: false,
            return_data: Bytes::new(),
            host: None,
            inspector: None,
            spec: SpecId::DEFAULT,
            features: EvmFeatures::empty(),
            // SAFETY: `MaybeUninit<Word>` does not need initialization.
            stack: unsafe { Box::new_uninit().assume_init() },
        }
    }

    /// Initializes this interpreter for a new frame, retaining reusable allocations.
    fn init(&mut self, tx_env: &'frame TxEnv<T>, message: &'frame Message<T>) {
        let bytecode = message.code.clone();
        let gas_limit = message.gas_limit;
        let is_static = message.caller_is_static || matches!(message.kind, MessageKind::StaticCall);
        self.pc = bytecode.bytes_slice().as_ptr();
        self.bytecode_ref = None;
        self.bytecode = bytecode;
        self.code_hash = message.code_hash;
        self.code_size =
            if message.chunked_code { message.code_size as usize } else { self.bytecode.len() };
        self.code_chunk_index = message.code_chunk_index;
        self.chunked_code = message.chunked_code;
        self.stack_len = 0;
        self.gas = Gas::new_with_execution_gas_and_reservoir(gas_limit, message.reservoir);
        self.memory.clear();
        self.result = Ok(());
        self.error = None;
        self.output = 0..0;
        self.tx_env = Some(tx_env);
        self.message = Some(message);
        self.is_static = is_static;
        self.return_data = Bytes::new();
    }

    pub(crate) const fn clear_frame_refs(&mut self) {
        self.tx_env = None;
        self.message = None;
        self.host = None;
        self.inspector = None;
    }

    #[cfg(test)]
    pub(crate) fn into_parts(self) -> (Box<StackBacking>, usize, Gas, Memory, Range<u32>) {
        (self.stack, self.stack_len, self.gas, self.memory, self.output)
    }

    /// Returns output produced by `RETURN` or `REVERT`.
    #[inline]
    pub fn output(&self) -> &[u8] {
        let start = self.output.start as usize;
        self.memory.slice(start, self.output.len())
    }

    /// Returns the current frame output memory range.
    #[inline]
    pub const fn output_range(&self) -> &Range<u32> {
        &self.output
    }

    /// Sets the current frame output memory range.
    #[inline]
    pub const fn set_output(&mut self, output: Range<u32>) {
        self.output = output;
    }

    /// Returns the current program counter in the complete deployed bytecode.
    #[inline]
    pub fn pc(&self) -> usize {
        let chunk_offset =
            if self.chunked_code { self.code_chunk_index as usize * CODE_CHUNK_SIZE } else { 0 };
        // SAFETY: `pc` is always in bounds of `bytecode`.
        chunk_offset
            + unsafe { self.pc.offset_from(self.bytecode.original_byte_slice().as_ptr()) as usize }
    }

    /// Sets the current bytecode-relative program counter.
    ///
    /// # Panics
    ///
    /// Panics if `pc` is out of bounds of the active bytecode.
    #[inline]
    pub fn set_pc(&mut self, pc: usize) {
        let pc = if self.chunked_code {
            let chunk = pc / CODE_CHUNK_SIZE;
            assert_eq!(chunk, self.code_chunk_index as usize);
            pc % CODE_CHUNK_SIZE
        } else {
            pc
        };
        let bytecode = self.bytecode.bytes_slice();
        assert!(pc < bytecode.len());
        self.pc = unsafe { bytecode.as_ptr().add(pc) };
    }

    /// Returns the current opcode.
    #[inline]
    pub const fn opcode(&self) -> u8 {
        // SAFETY: `pc` is always in bounds of `bytecode`.
        unsafe { *self.pc }
    }

    /// Returns the active bytecode.
    #[inline]
    pub fn bytecode(&self) -> BytecodeRef<'_> {
        self.bytecode_ref.unwrap_or_else(|| BytecodeRef::new(&self.bytecode))
    }

    /// Returns the original active bytecode bytes.
    #[inline]
    pub fn original_bytecode(&self) -> Bytes {
        self.bytecode.original_bytes()
    }

    /// Returns the original active bytecode as a byte slice.
    #[inline]
    pub fn original_bytecode_slice(&self) -> &[u8] {
        self.bytecode.original_byte_slice()
    }

    /// Calculates or returns the cached hash of the original active bytecode.
    #[inline]
    pub fn original_bytecode_hash(&self) -> B256 {
        if self.chunked_code { self.code_hash } else { self.bytecode.hash_slow() }
    }

    /// Returns the current operand stack.
    #[inline]
    pub const fn stack(&self) -> StackRef<'_> {
        StackRef::new(&self.stack, self.stack_len)
    }

    /// Returns the current mutable operand stack.
    #[inline]
    pub const fn stack_mut(&mut self) -> StackMut<'_> {
        StackMut { stack: &mut self.stack, len: &mut self.stack_len }
    }

    /// Returns the current gas state.
    #[inline]
    pub const fn gas(&self) -> Gas {
        self.gas
    }

    /// Returns a reference to the current gas state.
    #[inline]
    pub const fn gas_mut(&mut self) -> &mut Gas {
        &mut self.gas
    }

    /// Sets the current interpreter gas state.
    #[inline]
    pub const fn set_gas(&mut self, gas: Gas) {
        self.gas = gas;
    }

    /// Returns the current linear memory.
    #[inline]
    pub const fn memory(&self) -> &Memory {
        &self.memory
    }

    /// Returns the current mutable linear memory.
    #[inline]
    pub const fn memory_mut(&mut self) -> &mut Memory {
        &mut self.memory
    }

    /// Returns the current interpreter result.
    #[inline]
    pub const fn result(&self) -> Result {
        self.result
    }

    /// Sets the interpreter result.
    #[inline]
    pub const fn set_result(&mut self, result: Result) {
        self.result = result;
    }

    /// Sets the current instruction result to `stop`.
    #[inline]
    pub const fn set_stop(&mut self, stop: InstrStop) {
        self.set_result(Err(stop));
    }

    /// Returns the active frame-local call/create message.
    #[inline]
    pub const fn message(&self) -> &'frame Message<T> {
        // SAFETY: `message` is initialized before inspected execution starts.
        unsafe { self.message.unwrap_unchecked() }
    }

    #[inline]
    #[doc(hidden)]
    pub const fn message_field_offset_for_jit() -> usize {
        core::mem::offset_of!(Self, message)
    }

    /// Returns the cached transaction-global environment.
    #[inline]
    pub const fn tx_env(&self) -> &'frame TxEnv<T> {
        // SAFETY: `tx_env` is initialized before execution starts.
        unsafe { self.tx_env.unwrap_unchecked() }
    }

    /// Returns return data from the last call-like operation.
    #[inline]
    pub const fn return_data(&self) -> &Bytes {
        &self.return_data
    }

    /// Returns a mutable reference to return data from the last call-like operation.
    #[inline]
    pub const fn return_data_mut(&mut self) -> &mut Bytes {
        &mut self.return_data
    }

    /// Borrows the operand stack, linear memory, and host independently for inspection.
    #[inline]
    pub const fn stack_memory_host(&mut self) -> (StackRef<'_>, &[u8], &mut T::Host<'host>) {
        // SAFETY: As in `host`, the host pointer is initialized during execution and points
        // outside the interpreter's owned stack and memory.
        let host = unsafe { self.host.unwrap_unchecked().as_mut() };
        (self.stack(), self.memory.as_slice(), host)
    }

    /// Returns the host implementation.
    #[inline]
    pub const fn host(&mut self) -> &mut T::Host<'host> {
        // SAFETY: `host` is initialized at the beginning of inspected execution.
        unsafe { self.host.unwrap_unchecked().as_mut() }
    }

    /// Returns the active base specification ID.
    #[inline]
    pub const fn spec(&self) -> SpecId {
        self.spec
    }

    /// Returns the active runtime version data.
    #[inline]
    pub fn version(&self) -> &Version {
        // SAFETY: The result is tied to `&self`, which excludes host calls through `self`.
        unsafe { self.version_detached() }
    }

    /// Returns the active runtime version data without borrowing the interpreter.
    ///
    /// # Safety
    ///
    /// The host must be initialized, and the result must not be used after the host is borrowed
    /// mutably.
    #[inline]
    unsafe fn version_detached(&self) -> &'host Version {
        unsafe { self.execution_config_detached() }.version()
    }

    /// Returns the active execution config without borrowing the interpreter.
    ///
    /// # Safety
    ///
    /// Same as [`Self::version_detached`].
    #[inline]
    unsafe fn execution_config_detached(&self) -> &'host ExecutionConfig<T> {
        unsafe { self.host.unwrap_unchecked().as_ref() }.execution_config()
    }

    /// Returns whether the active frame forbids state-changing operations.
    #[inline]
    pub const fn is_static(&self) -> bool {
        self.is_static
    }

    /// Sets the static-call flag for external execution adapters.
    #[inline]
    pub const fn set_static(&mut self, is_static: bool) {
        self.is_static = is_static;
    }

    /// Converts a host failure into an instruction stop, retaining external errors only in this
    /// frame.
    #[inline]
    pub fn fail(&mut self, error: impl Into<HostError>) -> InstrStop {
        match error.into() {
            HostError::Halt(stop) => stop,
            HostError::Execution(error) => {
                self.error = Some(error);
                InstrStop::FatalExternalError
            }
        }
    }

    /// Finishes a backend run, returning its owned error and clearing execution references.
    pub(crate) fn finish_run(&mut self, stop: InstrStop) -> Result<InstrStop, ExecutionError> {
        self.host = None;
        self.inspector = None;
        if let Some(error) = self.take_error() {
            return Err(error);
        }
        if stop.is_fatal() {
            return Err(ExecutionError::Fatal(
                "interpreter returned a fatal stop without an error".into(),
            ));
        }
        Ok(stop)
    }

    /// Runs the interpreter until it stops.
    #[inline]
    pub fn run(&mut self, host: &mut T::Host<'host>) -> Result<InstrStop, ExecutionError> {
        self.run_inner(host, None)
    }

    /// Runs the interpreter until it stops with an execution inspector.
    #[inline]
    pub fn run_inspect(
        &mut self,
        host: &mut T::Host<'host>,
        inspector: &mut (dyn Inspector<T> + 'host),
    ) -> Result<InstrStop, ExecutionError> {
        self.run_inner(host, Some(NonNull::from(inspector)))
    }

    /// Prepares this interpreter for external execution.
    #[inline]
    #[doc(hidden)]
    pub fn prepare_run(&mut self, host: &mut T::Host<'host>) {
        if self.bytecode_ref.is_none() {
            // SAFETY: The view borrows immutable allocations retained by our owned `Bytecode`,
            // so moving the interpreter does not invalidate it. `init` clears the view before
            // replacing the owner, and accessors tie the returned view to `&self`.
            let bytecode = unsafe { trustme::decouple_lt(&self.bytecode) };
            self.bytecode_ref = Some(BytecodeRef::new(bytecode));
        }
        let config = host.execution_config();
        let version = config.version();
        self.memory.set_memory_limit(version.memory_limit);
        self.spec = config.base_spec_id();
        self.features = version.features;
        self.host = Some(NonNull::from(host));
        self.inspector = None;
    }

    #[inline(never)]
    fn run_inner(
        &mut self,
        host: &mut T::Host<'host>,
        inspector: Option<NonNull<dyn Inspector<T> + 'host>>,
    ) -> Result<InstrStop, ExecutionError> {
        self.prepare_run(host);
        self.inspector = inspector;

        let stop = if self.error.is_some() {
            InstrStop::FatalExternalError
        } else {
            // Load the table only after setup: passing it in keeps it in a callee-saved register
            // across setup, which makes the dispatch loop spill the stack pointer.
            // SAFETY: `prepare_run` initialized the host, and the config is only read here.
            let config = unsafe { self.execution_config_detached() };
            let instructions =
                if inspector.is_some() { config.inspect_instructions } else { config.instructions };
            dispatch::run(self, instructions)
        };
        self.finish_run(stop)
    }

    /// Takes an owned error recorded by an instruction or inspector hook.
    pub(crate) const fn take_error(&mut self) -> Option<ExecutionError> {
        self.error.take()
    }
}

/// Interpreter state exposed to instruction implementations.
#[repr(transparent)]
pub struct InterpreterState<'frame, 'host, T: EvmTypesHost>(
    pub(crate) Interpreter<'frame, 'host, T>,
);

impl<T: EvmTypesHost> fmt::Debug for InterpreterState<'_, '_, T> {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl<'frame, 'host, T: EvmTypesHost> InterpreterState<'frame, 'host, T> {
    /// Converts a host failure into an instruction stop in this frame.
    #[inline]
    pub fn fail(&mut self, error: impl Into<HostError>) -> InstrStop {
        self.0.fail(error)
    }

    #[inline]
    pub(crate) const fn wrap_mut<'a>(
        interp: &'a mut Interpreter<'frame, 'host, T>,
    ) -> &'a mut Self {
        // SAFETY: `InterpreterState` is a transparent wrapper over `Interpreter`.
        unsafe { core::mem::transmute::<&mut Interpreter<'frame, 'host, T>, &mut Self>(interp) }
    }

    #[inline]
    pub(in crate::interpreter) const unsafe fn gas_from_state_ptr(state: *mut Self) -> *mut Gas {
        // SAFETY: The caller upholds that `state` points to a valid interpreter state.
        unsafe { &raw mut (*state).0.gas }
    }

    #[inline]
    pub(crate) const fn gas_mut(&mut self) -> &mut Gas {
        &mut self.0.gas
    }

    #[inline(always)]
    pub(crate) const fn set_result(&mut self, result: Result) {
        self.0.result = result;
    }

    #[inline]
    pub(crate) const fn result(&self) -> Result {
        self.0.result
    }

    #[inline]
    #[cfg(not(tco))]
    pub(crate) const fn is_inspecting(&self) -> bool {
        self.0.inspector.is_some()
    }

    #[inline]
    pub(crate) const fn set_pc_stack_len(&mut self, pc: *const u8, stack_len: usize) {
        self.0.pc = pc;
        self.0.stack_len = stack_len;
    }

    /// Returns the cached transaction-global environment.
    #[inline]
    pub const fn tx(&self) -> &'frame TxEnv<T> {
        // SAFETY: `tx_env` is initialized at the beginning of `run` and remains set for
        // instruction execution.
        unsafe { self.0.tx_env.unwrap_unchecked() }
    }

    /// Returns the active base specification ID.
    #[inline]
    pub const fn spec(&self) -> SpecId {
        self.0.spec
    }

    /// Returns the active bytecode.
    #[inline]
    pub const fn bytecode(&self) -> BytecodeRef<'_> {
        // SAFETY: `prepare_run` initializes the view before instruction execution. It remains
        // valid across call/resume cycles and is only cleared when initializing a new frame.
        unsafe { self.0.bytecode_ref.unwrap_unchecked() }
    }

    /// Returns the host implementation.
    #[inline]
    pub const fn host(&mut self) -> &mut T::Host<'host> {
        // SAFETY: `host` is initialized at the beginning of `run` and cleared before the
        // method returns. Instruction execution is synchronous, so the pointer cannot outlive the
        // `run` host borrow.
        unsafe { self.0.host.unwrap_unchecked().as_mut() }
    }

    /// Returns the active runtime version data.
    #[inline]
    pub fn version(&self) -> &Version {
        self.0.version()
    }

    /// Returns the active frame-local call/create message.
    #[inline]
    pub const fn message(&self) -> &'frame Message<T> {
        // SAFETY: `message` is initialized at the beginning of `run` and remains set for
        // instruction execution.
        unsafe { self.0.message.unwrap_unchecked() }
    }

    /// Returns the complete deployed code size for a chunked frame.
    #[inline]
    pub const fn code_size(&self) -> usize {
        self.0.code_size
    }

    /// Returns the current deployed-code chunk index.
    #[inline]
    pub const fn code_chunk_index(&self) -> u32 {
        self.0.code_chunk_index
    }

    /// Returns whether this frame executes independently loaded code chunks.
    #[inline]
    pub const fn is_chunked_code(&self) -> bool {
        self.0.chunked_code
    }

    /// Returns the global program counter represented by a local dispatch pointer.
    #[inline]
    pub const fn global_pc(&self, pc: Pc) -> usize {
        let local = self.bytecode().pc_offset(pc);
        if self.0.chunked_code {
            self.0.code_chunk_index as usize * CODE_CHUNK_SIZE + local
        } else {
            local
        }
    }

    /// Replaces the active independently executable chunk.
    #[inline]
    pub(crate) fn activate_code_chunk(&mut self, index: u32, bytecode: Bytecode) {
        self.0.bytecode_ref = None;
        self.0.bytecode = bytecode;
        self.0.code_chunk_index = index;
        let bytecode = unsafe { trustme::decouple_lt(&self.0.bytecode) };
        self.0.bytecode_ref = Some(BytecodeRef::new(bytecode));
    }

    /// Returns whether the active frame forbids state-changing operations.
    #[inline]
    pub const fn is_static(&self) -> bool {
        self.0.is_static
    }

    /// Returns `true` if the active feature set contains `feature`.
    #[inline]
    pub const fn feature(&self, feature: EvmFeatures) -> bool {
        self.0.features.contains(feature)
    }

    /// Returns the active dynamic gas parameters.
    #[inline]
    pub fn gas_params(&self) -> &GasParams {
        &self.version().gas_params
    }

    /// Returns the active dynamic gas parameters without borrowing the interpreter state.
    ///
    /// # Safety
    ///
    /// The result must not be used after the host is borrowed mutably.
    #[inline]
    pub(in crate::interpreter) unsafe fn gas_params_detached(&self) -> &'host GasParams {
        unsafe { &self.0.version_detached().gas_params }
    }

    /// Returns linear memory.
    #[inline]
    pub const fn memory(&mut self) -> &mut Memory {
        &mut self.0.memory
    }

    /// Resizes linear memory using the active runtime gas parameters.
    #[inline]
    pub fn resize_memory(&mut self, gas: &mut Gas, offset: usize, len: usize) -> Result {
        // SAFETY: Resizing memory makes no host calls.
        let gas_params = unsafe { self.gas_params_detached() };
        self.0.memory.resize_evm(gas, gas_params, offset, len)
    }

    /// Returns return data from the last call-like operation.
    #[inline]
    pub const fn return_data(&self) -> &Bytes {
        &self.0.return_data
    }

    /// Returns a mutable reference to return data from the last call-like operation.
    #[inline]
    pub const fn return_data_mut(&mut self) -> &mut Bytes {
        &mut self.0.return_data
    }

    /// Clears return data from the last call-like operation.
    #[inline]
    pub fn clear_return_data(&mut self) {
        self.0.return_data.clear();
    }

    /// Swaps return data from the last call-like operation.
    #[inline]
    pub(crate) const fn swap_return_data(&mut self, return_data: &mut Bytes) {
        core::mem::swap(&mut self.0.return_data, return_data);
    }

    /// Returns the current frame output memory range.
    #[inline]
    pub const fn output_range(&self) -> &Range<u32> {
        &self.0.output
    }

    /// Sets the current frame output memory range.
    #[inline]
    pub const fn set_output(&mut self, output: Range<u32>) {
        self.0.output = output;
    }

    #[inline]
    pub(crate) fn inspect_step(&mut self, pc: Pc, stack_len: usize) {
        self.0.pc = pc.as_ptr();
        self.0.stack_len = stack_len;
        unsafe {
            let mut inspector = self.0.inspector.unwrap_unchecked();
            inspector.as_mut().step(&mut self.0);
        }
    }

    #[inline]
    pub(crate) fn inspect_step_end(&mut self, pc: Pc, stack_len: usize) {
        self.0.pc = pc.as_ptr();
        self.0.stack_len = stack_len;
        if self.0.result.is_err_and(InstrStop::is_out_of_gas) {
            cold_path();
            // Failed charges may leave a wrapped counter until frame settlement.
            self.0.gas.set_remaining(0);
        }
        unsafe {
            let mut inspector = self.0.inspector.unwrap_unchecked();
            inspector.as_mut().step_end(&mut self.0);
        }
    }

    #[inline]
    pub(crate) fn inspect_selfdestruct(
        &mut self,
        contract: &Address,
        target: &Address,
        value: &Word,
    ) {
        if let Some(mut inspector) = self.0.inspector {
            unsafe {
                let mut host = self.0.host.unwrap_unchecked();
                inspector.as_mut().selfdestruct(contract, target, value, host.as_mut());
            }
        }
    }

    #[inline]
    pub(crate) fn inspect_extcodecopy(
        &mut self,
        address: Address,
        code: &Bytecode,
        offset: usize,
        len: usize,
    ) {
        if let Some(mut inspector) = self.0.inspector {
            // SAFETY: As with the other hooks, the inspector outlives this execution.
            unsafe { inspector.as_mut().extcodecopy(address, code, offset, len) };
        }
    }
}

#[derive(Default)]
pub(crate) struct InterpreterPool<T: EvmTypesHost> {
    frames: Vec<Box<Interpreter<'static, 'static, T>>>,
}

impl<T: EvmTypesHost> InterpreterPool<T> {
    pub(crate) const fn new() -> Self {
        Self { frames: Vec::new() }
    }

    pub(crate) fn pop<'frame, 'host>(
        &mut self,
        tx_env: &'frame TxEnv<T>,
        message: &'frame Message<T>,
    ) -> Box<Interpreter<'frame, 'host, T>> {
        let frame = match self.frames.pop() {
            Some(mut frame) => {
                frame.init(tx_env, message);
                frame
            }
            None => Box::new(Interpreter::new(tx_env, message)),
        };
        // SAFETY: Frames stored in the pool have their frame-local references cleared before they
        // are erased to `'static`. Rebinding the lifetime is only used to initialize the next
        // frame.
        unsafe { trustme::decouple_lt_box(frame) }
    }

    pub(crate) fn push<'pool, 'frame, 'host>(
        &'pool mut self,
        mut frame: Box<Interpreter<'frame, 'host, T>>,
    ) -> &'pool mut Interpreter<'frame, 'host, T> {
        frame.clear_frame_refs();
        // SAFETY: `clear_frame_refs` removes every reference carrying `'frame`, so the boxed
        // interpreter can be stored in the pool with the erased `'static` lifetime.
        let frame = unsafe { trustme::decouple_lt_box(frame) };
        let frame = self.frames.push_mut(frame);
        // SAFETY: The returned borrow is tied to `&mut self`; the erased frame references are
        // empty.
        unsafe { trustme::decouple_interpreter_lt_mut(frame) }
    }

    pub(crate) fn last_mut<'frame, 'host>(&mut self) -> Option<&mut Interpreter<'frame, 'host, T>> {
        let frame = self.frames.last_mut()?.as_mut();
        // SAFETY: Frames stored in the pool have had their frame-local references cleared by
        // `push`, and this borrow is tied to the pool borrow.
        Some(unsafe { trustme::decouple_interpreter_lt_mut(frame) })
    }
}

#[cfg(test)]
mod owned_error_tests {
    use super::*;
    use crate::{DatabaseError, env::TxEnvExt, interpreter::MessageExt, test_utils::TestTypes};

    #[test]
    fn owned_error_is_taken_on_exit() {
        let tx = TxEnvExt::default();
        let message = MessageExt::default();
        let mut interpreter = Interpreter::<TestTypes>::new(&tx, &message);
        let error = DatabaseError::new(core::fmt::Error, false);
        let stop = interpreter.fail(error.clone());
        let result = interpreter.finish_run(stop);
        assert_eq!(result, Err(ExecutionError::Database(error)));
        assert!(interpreter.error.is_none());
        assert_eq!(interpreter.finish_run(InstrStop::Stop), Ok(InstrStop::Stop));
    }
}

#[cfg(test)]
mod bytecode_padding_tests {
    use super::*;
    use crate::{
        interpreter::op,
        test_utils::{RunConfig, run},
    };

    #[test]
    fn empty_code_reads_padding_stop() {
        let interp = run(RunConfig::default());
        assert_eq!(interp.err, InstrStop::Stop);
    }

    #[test]
    fn push_past_end_reads_zero_padding() {
        let interp = run(RunConfig::new([op::PUSH2, 0x01]));
        assert_eq!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [Word::from(0x0100)]);
    }

    #[test]
    fn jump_to_final_jumpdest_reads_padding_stop() {
        let interp = run(RunConfig::new([op::PUSH1, 3, op::JUMP, op::JUMPDEST]));
        assert_eq!(interp.err, InstrStop::Stop);
        assert!(interp.stack().is_empty());
    }
}
