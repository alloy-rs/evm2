use super::{
    BytecodeRef, Gas, InstrStop, Memory, Message, MessageKind, Pc, Result, StackBacking, StackMut,
    StackRef, Word,
};
use crate::{
    EvmTypes, ExecutionConfig, SpecId, Version,
    bytecode::Bytecode,
    env::TxEnv,
    evm::inspector::Inspector,
    interpreter::dispatch::{self, InstrTable},
    trustme,
    version::{EvmFeatures, GasParams},
};
use alloc::{boxed::Box, vec::Vec};
use alloy_primitives::{Address, Bytes, Log};
use core::{fmt, ops::Range, ptr::NonNull};
use derive_where::derive_where;

/// EVM interpreter.
#[derive_where(Debug)]
pub struct Interpreter<'frame, T: EvmTypes> {
    pub(in crate::interpreter) bytecode: Bytecode,
    pub(in crate::interpreter) memory: Memory,
    pub(in crate::interpreter) return_data: Bytes,

    pub(in crate::interpreter) pc: *const u8,
    output: Range<u32>,
    #[derive_where(skip)]
    tx_env: Option<&'frame TxEnv<T>>,
    #[derive_where(skip)]
    message: Option<&'frame Message<T>>,
    host: Option<NonNull<T::Host>>,
    inspector: Option<NonNull<dyn Inspector<T>>>,
    version: *const Version,
    pub(in crate::interpreter) stack_len: usize,
    #[derive_where(skip)]
    pub(in crate::interpreter) stack: Box<StackBacking>,

    pub(in crate::interpreter) gas: Gas,
    pub(in crate::interpreter) result: Result,
    spec: SpecId,
    features: EvmFeatures,
    is_static: bool,
}

// SAFETY: The interpreter's internal pointers are always valid. `pc` points into owned bytecode,
// frame-local references are cleared before pooling, and host/inspector/version pointers are
// installed for execution and not used after the owning execution context is gone.
unsafe impl<T: EvmTypes> Send for Interpreter<'_, T> {}

impl<T: EvmTypes> Default for Interpreter<'_, T> {
    fn default() -> Self {
        let bytecode = Bytecode::new();
        Self {
            pc: bytecode.original_byte_slice().as_ptr(),
            bytecode,
            stack_len: 0,
            gas: Gas::new(0),
            memory: Memory::new(),
            result: Ok(()),
            output: 0..0,
            tx_env: None,
            message: None,
            is_static: false,
            return_data: Bytes::new(),
            host: None,
            inspector: None,
            version: core::ptr::null(),
            spec: SpecId::DEFAULT,
            features: EvmFeatures::empty(),
            // SAFETY: `MaybeUninit<Word>` does not need initialization.
            stack: unsafe { Box::new_uninit().assume_init() },
        }
    }
}

impl<'frame, T: EvmTypes> Interpreter<'frame, T> {
    /// Creates an interpreter from analyzed bytecode, a transaction-global environment, and a
    /// frame-local message.
    pub fn new(bytecode: Bytecode, tx_env: &'frame TxEnv<T>, message: &'frame Message<T>) -> Self {
        let mut interp = Self::default();
        interp.init(bytecode, tx_env, message);
        interp
    }

    /// Initializes this interpreter for a new frame, retaining reusable allocations.
    pub(crate) fn init(
        &mut self,
        bytecode: Bytecode,
        tx_env: &'frame TxEnv<T>,
        message: &'frame Message<T>,
    ) {
        let gas_limit = message.gas_limit;
        let is_static = message.caller_is_static || matches!(message.kind, MessageKind::StaticCall);
        self.pc = bytecode.original_byte_slice().as_ptr();
        self.bytecode = bytecode;
        self.stack_len = 0;
        self.gas = Gas::new(gas_limit);
        self.memory.clear();
        self.result = Ok(());
        self.output = 0..0;
        self.tx_env = Some(tx_env);
        self.message = Some(message);
        self.is_static = is_static;
        self.return_data = Bytes::new();
    }

    pub(crate) const fn clear_frame_refs(&mut self) {
        self.tx_env = None;
        self.message = None;
    }

    #[cfg(test)]
    pub(crate) const fn memory_len(&self) -> usize {
        self.memory.len()
    }

    #[cfg(test)]
    pub(crate) fn set_return_data(&mut self, return_data: Bytes) {
        self.return_data = return_data;
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

    /// Returns the current bytecode-relative program counter.
    #[inline]
    pub fn pc(&self) -> usize {
        unsafe { self.pc.offset_from(self.bytecode.original_byte_slice().as_ptr()) as usize }
    }

    /// Returns the current opcode.
    #[inline]
    pub const fn opcode(&self) -> u8 {
        unsafe { *self.pc }
    }

    /// Returns the active bytecode.
    #[inline]
    pub fn bytecode(&self) -> BytecodeRef<'_> {
        BytecodeRef::new(&self.bytecode)
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

    /// Stops the interpreter with `stop`.
    #[inline]
    pub const fn set_stop(&mut self, stop: InstrStop) {
        self.result = Err(stop);
    }

    /// Returns the current linear memory.
    #[inline]
    pub const fn memory_ref(&self) -> &Memory {
        &self.memory
    }

    /// Returns the current gas state.
    #[inline]
    pub const fn gas(&self) -> Gas {
        self.gas
    }

    /// Returns the current instruction result.
    #[inline]
    pub const fn result(&self) -> Result {
        self.result
    }

    /// Returns a reference to the current gas state.
    #[inline]
    pub const fn gas_mut(&mut self) -> &mut Gas {
        &mut self.gas
    }

    /// Returns the active frame-local call/create message.
    #[inline]
    pub const fn message(&self) -> &'frame Message<T> {
        // SAFETY: `message` is initialized before inspected execution starts.
        unsafe { self.message.unwrap_unchecked() }
    }

    /// Returns return data from the last call-like operation.
    #[inline]
    pub const fn return_data(&self) -> &Bytes {
        &self.return_data
    }

    /// Returns the host implementation.
    #[inline]
    pub const fn host(&mut self) -> &mut T::Host {
        // SAFETY: `host` is initialized at the beginning of inspected execution.
        unsafe { self.host.unwrap_unchecked().as_mut() }
    }

    /// Returns the active base specification ID.
    #[inline]
    pub const fn spec(&self) -> SpecId {
        self.spec
    }

    /// Runs the interpreter until it stops.
    #[inline]
    pub fn run(&mut self, config: &ExecutionConfig<T>, host: &mut T::Host) -> InstrStop {
        self.run_inner(config.base_spec_id(), config.version(), host, None, config.instructions)
    }

    /// Runs the interpreter until it stops with an execution inspector.
    #[inline]
    pub fn run_inspect(
        &mut self,
        config: &ExecutionConfig<T>,
        host: &mut T::Host,
        inspector: &mut dyn Inspector<T>,
    ) -> InstrStop {
        self.run_inner(
            config.base_spec_id(),
            config.version(),
            host,
            Some(NonNull::from(inspector)),
            config.inspect_instructions,
        )
    }

    #[inline(never)]
    fn run_inner(
        &mut self,
        spec: SpecId,
        version: &Version,
        host: &mut T::Host,
        inspector: Option<NonNull<dyn Inspector<T>>>,
        instructions: &InstrTable<T>,
    ) -> InstrStop {
        self.memory.set_memory_limit(version.memory_limit);

        self.host = Some(NonNull::from(host));
        self.inspector = inspector;
        self.version = version;
        self.spec = spec;
        self.features = version.features;

        dispatch::run(self, instructions)
    }

    #[inline]
    pub(crate) fn set_inspection_context(
        &mut self,
        spec: SpecId,
        version: &Version,
        host: &mut T::Host,
    ) {
        self.host = Some(NonNull::from(host));
        self.version = version;
        self.spec = spec;
        self.features = version.features;
    }
}

/// Interpreter state exposed to instruction implementations.
#[repr(transparent)]
pub struct InterpreterState<'frame, T: EvmTypes>(pub(crate) Interpreter<'frame, T>);

impl<T: EvmTypes> fmt::Debug for InterpreterState<'_, T> {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl<'frame, T: EvmTypes> InterpreterState<'frame, T> {
    #[inline]
    pub(crate) const fn wrap_mut<'a>(interp: &'a mut Interpreter<'frame, T>) -> &'a mut Self {
        // SAFETY: `InterpreterState` is a transparent wrapper over `Interpreter`.
        unsafe { core::mem::transmute::<&mut Interpreter<'frame, T>, &mut Self>(interp) }
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

    /// Returns the active bytecode.
    #[inline]
    pub fn bytecode(&self) -> BytecodeRef<'_> {
        BytecodeRef::new(&self.0.bytecode)
    }

    /// Returns the host implementation.
    #[inline]
    pub const fn host(&mut self) -> &mut T::Host {
        // SAFETY: `host` is initialized at the beginning of `run` and cleared before the
        // method returns. Instruction execution is synchronous, so the pointer cannot outlive the
        // `run` host borrow.
        unsafe { self.0.host.unwrap_unchecked().as_mut() }
    }

    /// Returns the active runtime version data.
    #[inline]
    pub const fn version(&self) -> &Version {
        // SAFETY: `version` is initialized at the beginning of `run` and points into the
        // `Version` borrowed by the current run.
        unsafe { &*self.0.version }
    }

    /// Returns the active frame-local call/create message.
    #[inline]
    pub const fn message(&self) -> &'frame Message<T> {
        // SAFETY: `message` is initialized at the beginning of `run` and remains set for
        // instruction execution.
        unsafe { self.0.message.unwrap_unchecked() }
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
    pub const fn gas_params(&self) -> &GasParams {
        &self.version().gas_params
    }

    /// Returns linear memory.
    #[inline]
    pub const fn memory(&mut self) -> &mut Memory {
        &mut self.0.memory
    }

    /// Resizes linear memory using the active runtime gas parameters.
    #[inline]
    pub(crate) fn resize_memory(&mut self, gas: &mut Gas, offset: usize, len: usize) -> Result {
        let gas_params = *self.gas_params();
        super::memory::resize_memory(gas, &mut self.0.memory, &gas_params, offset, len)
    }

    /// Returns return data from the last call-like operation.
    #[inline]
    pub const fn return_data(&self) -> &Bytes {
        &self.0.return_data
    }

    /// Sets return data from the last call-like operation.
    #[inline]
    pub fn set_return_data(&mut self, return_data: Bytes) {
        self.0.return_data = return_data;
    }

    /// Swaps return data from the last call-like operation.
    #[inline]
    pub(crate) const fn swap_return_data(&mut self, return_data: &mut Bytes) {
        core::mem::swap(&mut self.0.return_data, return_data);
    }

    /// Sets the current frame output.
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
        unsafe {
            let mut inspector = self.0.inspector.unwrap_unchecked();
            inspector.as_mut().step_end(&mut self.0);
        }
    }

    #[inline]
    pub(crate) fn inspect_log(&mut self, log: &Log) {
        if let Some(mut inspector) = self.0.inspector {
            unsafe {
                let mut host = self.0.host.unwrap_unchecked();
                inspector.as_mut().log(log, host.as_mut());
            }
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
}

#[derive(Default)]
pub(crate) struct InterpreterPool<T: EvmTypes> {
    frames: Vec<Box<Interpreter<'static, T>>>,
}

impl<T: EvmTypes> InterpreterPool<T> {
    pub(crate) const fn new() -> Self {
        Self { frames: Vec::new() }
    }

    pub(crate) fn pop<'frame>(&mut self) -> Box<Interpreter<'frame, T>> {
        let frame = self.frames.pop().unwrap_or_default();
        // SAFETY: Frames stored in the pool have their frame-local references cleared before they
        // are erased to `'static`. Rebinding the lifetime is only used to initialize the next
        // frame.
        unsafe { trustme::decouple_lt_box(frame) }
    }

    pub(crate) fn push<'pool, 'frame>(
        &'pool mut self,
        mut frame: Box<Interpreter<'frame, T>>,
    ) -> &'pool mut Interpreter<'frame, T> {
        frame.clear_frame_refs();
        // SAFETY: `clear_frame_refs` removes every reference carrying `'frame`, so the boxed
        // interpreter can be stored in the pool with the erased `'static` lifetime.
        let frame = unsafe { trustme::decouple_lt_box(frame) };
        let frame = self.frames.push_mut(frame);
        // SAFETY: The returned borrow is tied to `&mut self`; the erased frame references are
        // empty.
        unsafe { trustme::decouple_interpreter_lt_mut(frame) }
    }

    pub(crate) fn last_mut<'frame>(&mut self) -> Option<&mut Interpreter<'frame, T>> {
        let frame = self.frames.last_mut()?.as_mut();
        // SAFETY: Frames stored in the pool have had their frame-local references cleared by
        // `push`, and this borrow is tied to the pool borrow.
        Some(unsafe { trustme::decouple_interpreter_lt_mut(frame) })
    }
}
