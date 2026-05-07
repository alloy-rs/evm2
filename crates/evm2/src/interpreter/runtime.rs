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
use core::{fmt, marker::PhantomData, ptr::NonNull};

/// EVM interpreter.
pub struct Interpreter<'frame, T: EvmTypes> {
    bytecode: Bytecode,
    pub(crate) pc: *const u8,
    pub(crate) stack: Box<[Word; Stack::CAPACITY]>,
    pub(crate) stack_len: usize,
    pub(crate) gas: Gas,
    pub(crate) memory: Memory,
    pub(crate) result: Result,
    pub(crate) output: *const [u8],
    tx_env: Option<&'frame TxEnv>,
    pub(crate) message: Option<&'frame Message>,
    pub(crate) is_static: bool,
    pub(crate) return_data: Bytes,
    pub(crate) host: Option<NonNull<T::Host>>,
    pub(crate) version: *const Version,
    /// Active spec identifier.
    pub spec: SpecId,
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
            host: None,
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
            .field("host", &self.host.map(|host| host.as_ptr()))
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
    pub(crate) const fn stack_len(&self) -> usize {
        self.stack_len
    }

    #[inline]
    pub(crate) const fn tx_env(&self) -> &TxEnv {
        self.tx_env.expect("interpreter tx env is initialized")
    }

    /// Returns the active runtime version data.
    #[inline]
    pub(crate) const fn version(&self) -> &Version {
        // SAFETY: `version` is initialized at the beginning of `run_with` and points into the
        // `ExecutionConfig` borrowed by the current run.
        unsafe { &*self.version }
    }

    #[inline]
    pub(crate) const fn message(&self) -> &Message {
        self.message.expect("interpreter message is initialized")
    }

    #[inline]
    pub(crate) const fn is_static(&self) -> bool {
        self.is_static
    }

    /// Returns the active dynamic gas parameters.
    #[inline]
    pub const fn gas_params(&self) -> &GasParams {
        &self.version().gas_params
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
        self.host = Some(NonNull::from(&mut *host));
        self.version = &config.version;
        self.spec = config.version.spec_id;

        #[cfg(feature = "nightly")]
        let r = self.step_tail(config);
        #[cfg(not(feature = "nightly"))]
        let r = self.run_table_loop(config);

        self.host = None;
        self.version = core::ptr::null();
        r
    }

    #[cfg(not(feature = "nightly"))]
    fn run_table_loop(&mut self, config: &ExecutionConfig<T>) -> InstrStop {
        #[expect(clippy::unnecessary_cast, reason = "cast erases the active interpreter lifetime")]
        let raw = self as *mut Self as *mut Interpreter<'_, T>;
        let mut pc = Pc::new(self.pc);
        let mut stack = Stack::new(&mut self.stack, self.stack_len);
        loop {
            let op = pc.op();
            let instr = config.instructions[op as usize];
            // SAFETY: Instruction methods must not access the stack through `Interpreter` while
            // the separate stack view is live.
            let (next_pc, next_stack_len) = instr(pc, stack.reborrow(), unsafe { &mut *raw });
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
        let pc = Pc::new(self.pc);
        let op = pc.op();
        let instr = config.instructions[op as usize];
        let stack = Stack::new(&mut self.stack, self.stack_len);
        let remaining_gas = RemainingGas::new(self.gas.remaining());
        // SAFETY: Instruction methods must not access the stack through `Interpreter` while the
        // separate stack view is live.
        instr(pc, stack, remaining_gas, unsafe { &mut *raw });
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

    /// Returns the cached transaction-global environment.
    #[inline]
    pub(crate) const fn tx(&self) -> &TxEnv {
        self.0.tx_env()
    }

    /// Returns the active bytecode.
    #[inline]
    pub(crate) fn bytecode(&self) -> BytecodeRef<'_> {
        BytecodeRef::new(&self.0.bytecode)
    }

    /// Returns the host implementation.
    #[inline]
    pub(crate) const fn host(&mut self) -> &mut T::Host {
        // SAFETY: `host` is initialized at the beginning of `run_with` and cleared before the
        // method returns. Instruction execution is synchronous, so the pointer cannot outlive the
        // `run_with` host borrow.
        unsafe { self.0.host.expect("interpreter host is initialized").as_mut() }
    }

    /// Returns the active runtime version data.
    #[inline]
    pub(crate) const fn version(&self) -> &Version {
        // SAFETY: `version` is initialized at the beginning of `run_with` and points into the
        // `ExecutionConfig` borrowed by the current run.
        unsafe { &*self.0.version }
    }

    /// Returns the active frame-local call/create message.
    #[inline]
    pub(crate) const fn message(&self) -> &Message {
        self.0.message()
    }

    /// Returns whether the active frame forbids state-changing operations.
    #[inline]
    pub(crate) const fn is_static(&self) -> bool {
        self.0.is_static()
    }

    /// Returns the active spec identifier.
    #[inline]
    pub(crate) const fn spec(&self) -> SpecId {
        self.0.spec
    }

    /// Returns the active dynamic gas parameters.
    #[inline]
    pub const fn gas_params(&self) -> &GasParams {
        &self.version().gas_params
    }

    /// Returns linear memory.
    #[inline]
    pub(crate) const fn memory(&mut self) -> &mut Memory {
        &mut self.0.memory
    }

    /// Returns return data from the last call-like operation.
    #[inline]
    pub(crate) const fn return_data(&self) -> &Bytes {
        &self.0.return_data
    }

    /// Sets return data from the last call-like operation.
    #[inline]
    pub(crate) fn set_return_data(&mut self, return_data: Bytes) {
        self.0.return_data = return_data;
    }

    /// Sets the current frame output.
    #[inline]
    pub(crate) const fn set_output(&mut self, output: *const [u8]) {
        self.0.output = output;
    }
}

/// Splits mutable instruction state into separate gas and state references.
///
/// # Safety
///
/// The returned `gas` reference must not be accessed through the returned `state` reference while
/// both references are live.
#[inline]
pub unsafe fn split_gas_state<'a, 'state, T: EvmTypes>(
    state: *mut InterpreterState<'state, T>,
) -> (&'a mut Gas, &'a mut InterpreterState<'state, T>) {
    // SAFETY: The caller must ensure the returned `gas` reference is not used through `state`.
    unsafe { (&mut (*state).0.gas, &mut *state) }
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
