#[cfg(tco)]
use super::gas::RemainingGas;
use super::{
    BytecodeRef, Gas, InstrStop, Memory, Message, MessageKind, MessageResult, Pc, Result, Stack,
    StackBacking, StackMut, StackRef, Word,
};
use crate::{
    EvmTypes, ExecutionConfig, SpecId, Version,
    bytecode::Bytecode,
    env::TxEnv,
    evm::inspector::Inspector,
    interpreter::instructions::table::InstrTable,
    trustme,
    version::{EvmFeatures, GasParams},
};
use alloc::{boxed::Box, vec::Vec};
use alloy_primitives::{Address, Bytes, Log};
#[cfg(not(tco))]
use core::hint::cold_path;
use core::{fmt, ptr::NonNull};
use derive_where::derive_where;

/// EVM interpreter.
#[derive_where(Debug)]
pub struct Interpreter<'frame, T: EvmTypes> {
    bytecode: Bytecode,
    memory: Memory,
    return_data: Bytes,

    pc: *const u8,
    output: *const [u8],
    #[derive_where(skip)]
    tx_env: Option<&'frame TxEnv<T>>,
    #[derive_where(skip)]
    message: Option<&'frame Message<T>>,
    host: Option<NonNull<T::Host>>,
    inspector: Option<NonNull<dyn Inspector<T>>>,
    version: *const Version,
    stack_len: usize,
    #[derive_where(skip)]
    stack: Box<StackBacking>,

    gas: Gas,
    result: Result,
    spec: SpecId,
    features: EvmFeatures,
    is_static: bool,
}

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
            output: &[],
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
    pub fn new(
        bytecode: Bytecode,
        tx_env: &'frame TxEnv<T>,
        message: &'frame Message<T>,
        caller_is_static: bool,
    ) -> Self {
        let mut interpreter = Self::default();
        interpreter.init(bytecode, tx_env, message, caller_is_static);
        interpreter
    }

    /// Initializes this interpreter for a new frame, retaining reusable allocations.
    pub(crate) fn init(
        &mut self,
        bytecode: Bytecode,
        tx_env: &'frame TxEnv<T>,
        message: &'frame Message<T>,
        caller_is_static: bool,
    ) {
        let gas_limit = message.gas_limit;
        let is_static = caller_is_static || matches!(message.kind, MessageKind::StaticCall);
        self.pc = bytecode.original_byte_slice().as_ptr();
        self.bytecode = bytecode;
        self.stack_len = 0;
        self.gas = Gas::new(gas_limit);
        self.memory.clear();
        self.result = Ok(());
        self.output = &[];
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
    pub(crate) fn into_parts(self) -> (Box<StackBacking>, usize, Gas, Memory, *const [u8]) {
        (self.stack, self.stack_len, self.gas, self.memory, self.output)
    }

    /// Returns output produced by `RETURN` or `REVERT`.
    #[inline]
    pub const fn output(&self) -> &[u8] {
        unsafe { &*self.output }
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

    /// Returns a reference to the current gas state.
    #[inline]
    pub const fn gas_mut(&mut self) -> &mut Gas {
        &mut self.gas
    }

    /// Returns the active frame-local call/create message.
    #[inline]
    pub const fn message(&self) -> &Message<T> {
        // SAFETY: `message` is initialized before inspected execution starts.
        unsafe { self.message.unwrap_unchecked() }
    }

    /// Returns return data from the last call-like operation.
    #[inline]
    pub const fn return_data(&self) -> &Bytes {
        &self.return_data
    }

    /// Returns the active base specification ID.
    #[inline]
    pub const fn spec(&self) -> SpecId {
        self.spec
    }

    /// Runs the interpreter until it stops.
    #[inline]
    pub fn run(&mut self, config: &ExecutionConfig<T>, host: &mut T::Host) -> InstrStop {
        self.run_inner(config.version(), host, None, config.instructions)
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
            config.version(),
            host,
            Some(NonNull::from(inspector)),
            config.inspect_instructions,
        )
    }

    #[inline(never)]
    fn run_inner(
        &mut self,
        version: &Version,
        host: &mut T::Host,
        inspector: Option<NonNull<dyn Inspector<T>>>,
        instructions: &InstrTable<T>,
    ) -> InstrStop {
        self.memory.set_memory_limit(version.memory_limit);

        self.host = Some(NonNull::from(host));
        self.inspector = inspector;
        self.version = version;
        self.spec = version.spec_id;
        self.features = version.features;

        #[cfg(tco)]
        let r = self.step_tail(instructions);
        #[cfg(not(tco))]
        let r = self.run_table_loop(instructions);

        r
    }

    #[cfg(not(tco))]
    fn run_table_loop(&mut self, instructions: &InstrTable<T>) -> InstrStop {
        // SAFETY: Only the active interpreter lifetime is erased; this stays as a raw pointer so
        // the dispatch loop does not create an extra `&mut` alias for `self`.
        let raw = unsafe { trustme::decouple_lt_mut_ptr(self as *mut Self) };
        // SAFETY: Instruction methods must not access the stack through `InterpreterState` while
        // the separate stack view is live.
        let state = InterpreterState::wrap_mut(unsafe { &mut *raw });
        let mut pc = Pc::new(self.pc);
        let mut stack = Stack::new(&mut self.stack, self.stack_len);
        loop {
            let op = pc.op();
            let instr = instructions[op as usize];
            let (next_pc, next_stack_len) = instr(pc, stack.reborrow(), state);
            pc = Pc::new(next_pc);
            stack.len = next_stack_len;
            if next_pc.is_null() {
                cold_path();
                self.pc = next_pc;
                self.stack_len = stack.len;
                return self.result.unwrap_err();
            }
        }
    }

    #[inline(always)]
    #[cfg(tco)]
    fn step_tail(&mut self, instructions: &InstrTable<T>) -> InstrStop {
        // SAFETY: Only the active interpreter lifetime is erased; this stays as a raw pointer so
        // the dispatch step does not create an extra `&mut` alias for `self`.
        let raw = unsafe { trustme::decouple_lt_mut_ptr(self as *mut Self) };
        // SAFETY: Instruction methods must not access the stack through `InterpreterState` while
        // the separate stack view is live.
        let state = InterpreterState::wrap_mut(unsafe { &mut *raw });
        let pc = Pc::new(self.pc);
        let op = pc.op();
        let stack = Stack::new(&mut self.stack, self.stack_len);
        let remaining_gas = RemainingGas::new(self.gas.remaining());
        let instr = instructions[op as usize];
        instr(pc, stack, remaining_gas, state, (instructions as *const InstrTable<T>).cast());
        self.result.unwrap_err()
    }
}

/// Interpreter state exposed to instruction implementations.
#[repr(transparent)]
pub struct InterpreterState<'frame, T: EvmTypes>(Interpreter<'frame, T>);

impl<T: EvmTypes> fmt::Debug for InterpreterState<'_, T> {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl<'frame, T: EvmTypes> InterpreterState<'frame, T> {
    #[inline]
    pub(crate) const fn wrap_mut<'a>(interpreter: &'a mut Interpreter<'frame, T>) -> &'a mut Self {
        // SAFETY: `InterpreterState` is a transparent wrapper over `Interpreter`.
        unsafe { core::mem::transmute::<&mut Interpreter<'frame, T>, &mut Self>(interpreter) }
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
    #[cfg(tco)]
    pub(crate) const fn result(&self) -> Result {
        self.0.result
    }

    #[inline]
    #[cfg(tco)]
    pub(crate) const fn set_pc_stack_len(&mut self, pc: *const u8, stack_len: usize) {
        self.0.pc = pc;
        self.0.stack_len = stack_len;
    }

    /// Returns the cached transaction-global environment.
    #[inline]
    pub const fn tx(&self) -> &TxEnv<T> {
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
    pub const fn message(&self) -> &Message<T> {
        // SAFETY: `message` is initialized at the beginning of `run` and remains set for
        // instruction execution.
        unsafe { self.0.message.unwrap_unchecked() }
    }

    /// Returns whether the active frame forbids state-changing operations.
    #[inline]
    pub const fn is_static(&self) -> bool {
        self.0.is_static
    }

    /// Returns the active spec identifier.
    #[inline]
    pub const fn spec(&self) -> SpecId {
        self.0.spec
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

    /// Sets the current frame output.
    #[inline]
    pub const fn set_output(&mut self, output: *const [u8]) {
        self.0.output = output;
    }

    #[inline]
    fn inspector(&mut self) -> Option<&mut dyn Inspector<T>> {
        self.0.inspector.map(|mut x| unsafe { x.as_mut() })
    }

    #[inline]
    pub(crate) fn inspect_step(&mut self, pc: Pc, stack_len: usize) {
        self.0.pc = pc.as_ptr();
        self.0.stack_len = stack_len;
        let mut inspector = unsafe { self.0.inspector.unwrap_unchecked() };
        unsafe { inspector.as_mut() }.step(&mut self.0);
    }

    #[inline]
    pub(crate) fn inspect_step_end(&mut self, pc: Pc, stack_len: usize) {
        self.0.pc = pc.as_ptr();
        self.0.stack_len = stack_len;
        let mut inspector = unsafe { self.0.inspector.unwrap_unchecked() };
        unsafe { inspector.as_mut() }.step_end(&mut self.0);
    }

    #[inline]
    pub(crate) fn inspect_call(&mut self, message: &mut Message<T>) -> Option<MessageResult<T>> {
        self.inspector().and_then(|inspector| inspector.call(message))
    }

    #[inline]
    pub(crate) fn inspect_call_end(&mut self, message: &Message<T>, result: &mut MessageResult<T>) {
        if let Some(inspector) = self.inspector() {
            inspector.call_end(message, result);
        }
    }

    #[inline]
    pub(crate) fn inspect_create(&mut self, message: &mut Message<T>) -> Option<MessageResult<T>> {
        self.inspector().and_then(|inspector| inspector.create(message))
    }

    #[inline]
    pub(crate) fn inspect_create_end(
        &mut self,
        message: &Message<T>,
        result: &mut MessageResult<T>,
    ) {
        if let Some(inspector) = self.inspector() {
            inspector.create_end(message, result);
        }
    }

    #[inline]
    pub(crate) fn inspect_log(&mut self, log: &Log) {
        if let Some(inspector) = self.inspector() {
            inspector.log(log);
        }
    }

    #[inline]
    pub(crate) fn inspect_selfdestruct(&mut self, contract: Address, target: Address, value: Word) {
        if let Some(inspector) = self.inspector() {
            inspector.selfdestruct(contract, target, value);
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
}
