#[cfg(feature = "nightly")]
use super::gas::RemainingGas;
use super::{BytecodeRef, Gas, InstrStop, Memory, Message, MessageKind, Pc, Result, Stack, Word};
use crate::{
    EvmConfig, EvmTypes, ExecutionConfig, SpecId, Version, bytecode::Bytecode, env::TxEnv,
    version::GasParams,
};
use alloc::{boxed::Box, vec::Vec};
use alloy_primitives::Bytes;
#[cfg(not(feature = "nightly"))]
use core::hint::cold_path;
use core::{fmt, marker::PhantomData};

/// EVM interpreter.
pub struct Interpreter<'frame, T: EvmTypes> {
    bytecode: Bytecode,

    stack: Box<[Word; Stack::CAPACITY]>,

    pc: *const u8,
    stack_len: usize,
    gas: Gas,
    memory: Memory,
    result: Result,
    output: *const [u8],
    tx_env: Option<&'frame TxEnv>,
    message: Option<&'frame Message>,
    is_static: bool,
    return_data: Bytes,
    host: *mut T::Host,
    version: *const Version,
    spec: SpecId,

    _marker: PhantomData<fn() -> T>,
}

impl<T: EvmTypes> Default for Interpreter<'_, T> {
    fn default() -> Self {
        let bytecode = Bytecode::new();
        Self {
            pc: bytecode.original_byte_slice().as_ptr(),
            bytecode,
            // SAFETY: `Word` is valid at any bitpattern. It's not read before init anyway.
            stack: unsafe { Box::new_uninit().assume_init() },
            stack_len: 0,
            gas: Gas::new(0),
            memory: Memory::new(),
            result: Ok(()),
            output: &[],
            tx_env: None,
            message: None,
            is_static: false,
            return_data: Bytes::new(),
            host: core::ptr::null_mut(),
            version: core::ptr::null(),
            spec: SpecId::DEFAULT,
            _marker: PhantomData,
        }
    }
}

impl<T: EvmTypes> fmt::Debug for Interpreter<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Interpreter")
            .field("bytecode", &self.bytecode)
            .field("pc", &self.pc)
            .field("stack_len", &self.stack_len)
            .field("gas", &self.gas)
            .field("memory", &self.memory)
            .field("result", &self.result)
            .field("output", &self.output)
            .field("tx_env", &self.tx_env)
            .field("message", &self.message)
            .field("is_static", &self.is_static)
            .field("return_data", &self.return_data)
            .field("host", &self.host)
            .field("version", &self.version)
            .field("spec", &self.spec)
            .finish_non_exhaustive()
    }
}

impl<'frame, T: EvmTypes> Interpreter<'frame, T> {
    /// Creates an interpreter from analyzed bytecode, a transaction-global environment, and a
    /// frame-local message.
    pub fn new(
        bytecode: Bytecode,
        tx_env: &'frame TxEnv,
        message: &'frame Message,
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
        tx_env: &'frame TxEnv,
        message: &'frame Message,
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
    pub(crate) fn into_parts(
        self,
    ) -> (Box<[Word; Stack::CAPACITY]>, usize, Gas, Memory, *const [u8]) {
        (self.stack, self.stack_len, self.gas, self.memory, self.output)
    }

    /// Returns output produced by `RETURN` or `REVERT`.
    #[inline]
    pub const fn output(&self) -> &[u8] {
        unsafe { &*self.output }
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

    /// Runs the interpreter until it stops, using `C` as the EVM configuration.
    #[inline]
    pub fn run<C: EvmConfig<T>>(&mut self, host: &mut T::Host) -> InstrStop {
        self.run_with(&ExecutionConfig::for_config::<C>(), host)
    }

    /// Runs the interpreter until it stops.
    pub fn run_with(&mut self, config: &ExecutionConfig<T>, host: &mut T::Host) -> InstrStop {
        self.memory.set_memory_limit(config.version.memory_limit);
        self.host = host;
        self.version = &config.version;
        self.spec = config.version.spec_id;

        #[cfg(feature = "nightly")]
        let r = self.step_tail(config);
        #[cfg(not(feature = "nightly"))]
        let r = self.run_table_loop(config);

        self.host = core::ptr::null_mut();
        self.version = core::ptr::null();
        r
    }

    #[cfg(not(feature = "nightly"))]
    fn run_table_loop(&mut self, config: &ExecutionConfig<T>) -> InstrStop {
        #[expect(clippy::unnecessary_cast, reason = "cast erases the active interpreter lifetime")]
        let raw = self as *mut Self as *mut Interpreter<'_, T>;
        // SAFETY: Instruction methods must not access the stack through `InterpreterState` while
        // the separate stack view is live.
        let state = InterpreterState::wrap_mut(unsafe { &mut *raw });
        let mut pc = Pc::new(self.pc);
        let mut stack = Stack::new(&mut self.stack, self.stack_len);
        loop {
            let op = pc.op();
            let instr = config.instructions[op as usize];
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
    #[cfg(feature = "nightly")]
    fn step_tail(&mut self, config: &ExecutionConfig<T>) -> InstrStop {
        #[expect(clippy::unnecessary_cast, reason = "cast erases the active interpreter lifetime")]
        let raw = self as *mut Self as *mut Interpreter<'_, T>;
        // SAFETY: Instruction methods must not access the stack through `InterpreterState` while
        // the separate stack view is live.
        let state = InterpreterState::wrap_mut(unsafe { &mut *raw });
        let pc = Pc::new(self.pc);
        let op = pc.op();
        let instr = config.instructions[op as usize];
        let stack = Stack::new(&mut self.stack, self.stack_len);
        let remaining_gas = RemainingGas::new(self.gas.remaining());
        instr(pc, stack, remaining_gas, state);
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

    #[inline]
    pub(crate) const fn set_result(&mut self, result: Result) {
        self.0.result = result;
    }

    #[inline]
    #[cfg(feature = "nightly")]
    pub(crate) const fn result(&self) -> Result {
        self.0.result
    }

    #[inline]
    #[cfg(feature = "nightly")]
    pub(crate) const fn set_pc_stack_len(&mut self, pc: *const u8, stack_len: usize) {
        self.0.pc = pc;
        self.0.stack_len = stack_len;
    }

    /// Returns the cached transaction-global environment.
    #[inline]
    pub const fn tx(&self) -> &TxEnv {
        self.0.tx_env.expect("interpreter tx env is initialized")
    }

    /// Returns the active bytecode.
    #[inline]
    pub fn bytecode(&self) -> BytecodeRef<'_> {
        BytecodeRef::new(&self.0.bytecode)
    }

    /// Returns the host implementation.
    #[inline]
    pub const fn host(&mut self) -> &mut T::Host {
        // SAFETY: `host` is initialized at the beginning of `run_with` and cleared before the
        // method returns. Instruction execution is synchronous, so the pointer cannot outlive the
        // `run_with` host borrow.
        assert!(!self.0.host.is_null(), "interpreter host is initialized");
        unsafe { &mut *self.0.host }
    }

    /// Returns the active runtime version data.
    #[inline]
    pub const fn version(&self) -> &Version {
        // SAFETY: `version` is initialized at the beginning of `run_with` and points into the
        // `ExecutionConfig` borrowed by the current run.
        unsafe { &*self.0.version }
    }

    /// Returns the active frame-local call/create message.
    #[inline]
    pub const fn message(&self) -> &Message {
        self.0.message.expect("interpreter message is initialized")
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
}

#[derive(Default)]
#[expect(clippy::vec_box, reason = "pooled active interpreters must stay at stable addresses")]
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
        unsafe {
            core::mem::transmute::<Box<Interpreter<'static, T>>, Box<Interpreter<'frame, T>>>(frame)
        }
    }

    pub(crate) fn push<'pool, 'frame>(
        &'pool mut self,
        mut frame: Box<Interpreter<'frame, T>>,
    ) -> &'pool mut Interpreter<'frame, T> {
        frame.clear_frame_refs();
        // SAFETY: `clear_frame_refs` removes every reference carrying `'frame`, so the boxed
        // interpreter can be stored in the pool with the erased `'static` lifetime.
        let frame = unsafe {
            core::mem::transmute::<Box<Interpreter<'frame, T>>, Box<Interpreter<'static, T>>>(frame)
        };
        let frame = self.frames.push_mut(frame);
        // SAFETY: The returned borrow is tied to `&mut self`; the erased frame references are
        // empty.
        unsafe {
            core::mem::transmute::<
                &'pool mut Interpreter<'static, T>,
                &'pool mut Interpreter<'frame, T>,
            >(frame)
        }
    }
}
