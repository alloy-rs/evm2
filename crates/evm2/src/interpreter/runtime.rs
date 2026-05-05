use super::{
    BytecodeRef, Gas, InstrStop, Memory, Message, MessageKind, Pc, Result, Stack, State, Word,
};
use crate::{EvmConfig, EvmTypes, ExecutionConfig, bytecode::Bytecode, env::TxEnv};
use alloc::boxed::Box;
use alloy_primitives::Bytes;
use core::marker::PhantomData;
#[cfg(not(feature = "nightly"))]
use core::{
    hint::cold_path,
    ops::ControlFlow::{self, Break, Continue},
};

/// EVM interpreter.
#[derive(Debug)]
pub struct Interpreter<T: EvmTypes> {
    bytecode: Bytecode,
    pub(crate) pc: *const u8,
    pub(crate) stack: Box<[Word; Stack::CAPACITY]>,
    pub(crate) stack_len: usize,
    pub(crate) gas: Gas,
    pub(crate) memory: Memory,
    pub(crate) result: Result,
    pub(crate) output: *const [u8],
    tx_env: *const TxEnv,
    pub(crate) message: *const Message,
    pub(crate) is_static: bool,
    pub(crate) return_data: Bytes,
    _marker: PhantomData<fn() -> T>,
}

impl<T: EvmTypes> Interpreter<T> {
    /// Creates an interpreter from analyzed bytecode, a transaction-global environment, and a
    /// frame-local message.
    pub fn new(
        bytecode: Bytecode,
        tx_env: &TxEnv,
        message: &Message,
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
        tx_env: &TxEnv,
        message: &Message,
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
        self.tx_env = tx_env;
        self.message = message;
        self.is_static = is_static;
        self.return_data = Bytes::new();
    }

    #[cfg(test)]
    pub(crate) const fn stack_len(&self) -> usize {
        self.stack_len
    }

    #[inline]
    pub(crate) fn tx_env(&self) -> &TxEnv {
        debug_assert!(!self.tx_env.is_null());
        unsafe { &*self.tx_env }
    }

    #[inline]
    pub(crate) fn message(&self) -> &Message {
        debug_assert!(!self.message.is_null());
        unsafe { &*self.message }
    }

    #[inline]
    pub(crate) const fn is_static(&self) -> bool {
        self.is_static
    }

    #[inline]
    pub(crate) const fn memory(&mut self) -> &mut Memory {
        &mut self.memory
    }

    #[inline]
    pub(crate) const fn return_data(&self) -> &Bytes {
        &self.return_data
    }

    #[inline]
    pub(crate) const fn set_output(&mut self, output: *const [u8]) {
        self.output = output;
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

    /// Runs the interpreter until it stops, using `C` as the EVM configuration.
    #[inline]
    pub fn run<C: EvmConfig<T>>(&mut self, host: &mut T::Host) -> InstrStop {
        self.run_with(ExecutionConfig::for_config::<C>(), host)
    }

    /// Runs the interpreter until it stops.
    pub fn run_with(&mut self, config: ExecutionConfig<T>, host: &mut T::Host) -> InstrStop {
        let _gas_start = self.gas.remaining();

        #[cfg(feature = "nightly")]
        let r = self.step_tail(config, host);
        #[cfg(not(feature = "nightly"))]
        let r = self.run_table_loop(config, host);

        r
    }

    #[cfg(not(feature = "nightly"))]
    fn run_table_loop(&mut self, config: ExecutionConfig<T>, host: &mut T::Host) -> InstrStop {
        let mut pc = self.pc;
        let mut stack_len = self.stack_len;
        loop {
            let (next_pc, next_stack_len, flow) = self.raw_step(config, host, pc, stack_len);
            pc = next_pc;
            stack_len = next_stack_len;
            if flow.is_break() {
                cold_path();
                self.pc = pc;
                self.stack_len = stack_len;
                return self.result.unwrap_err();
            }
        }
    }

    /// Executes one instruction.
    #[inline]
    #[cfg(not(feature = "nightly"))]
    pub fn step(&mut self, config: ExecutionConfig<T>, host: &mut T::Host) -> ControlFlow<(), ()> {
        let (pc, stack_len, flow) = self.raw_step(config, host, self.pc, self.stack_len);
        self.pc = pc;
        self.stack_len = stack_len;
        flow
    }

    #[inline(never)]
    #[cfg(not(feature = "nightly"))]
    fn raw_step(
        &mut self,
        config: ExecutionConfig<T>,
        host: &mut T::Host,
        pc: *const u8,
        stack_len: usize,
    ) -> (*const u8, usize, ControlFlow<(), ()>) {
        let raw = self as *mut Self;
        let bytecode = BytecodeRef::new(&self.bytecode);
        let pc = Pc::from_ptr(pc);
        let op = pc.op();
        let instr = config.instructions[op as usize];
        let (pc, stack_len) = instr(
            pc,
            Stack::new(&mut self.stack, stack_len),
            &mut self.gas,
            &mut State {
                bytecode,
                host,
                spec: config.version.spec_id(),
                version: config.version,
                raw_interp: raw,
            },
        );
        let flow = if pc.is_null() { Break(()) } else { Continue(()) };
        (pc, stack_len, flow)
    }

    #[inline(always)]
    #[cfg(feature = "nightly")]
    fn step_tail(&mut self, config: ExecutionConfig<T>, host: &mut T::Host) -> InstrStop {
        let raw = self as *mut Self;
        let bytecode = BytecodeRef::new(&self.bytecode);
        let pc = Pc::from_ptr(self.pc);
        let op = pc.op();
        let instr = config.instructions[op as usize];
        instr(
            pc,
            Stack::new(&mut self.stack, self.stack_len),
            &mut self.gas,
            &mut State {
                bytecode,
                host,
                spec: config.version.spec_id(),
                version: config.version,
                raw_interp: raw,
            },
            0,
        );
        self.result.unwrap_err()
    }
}

impl<T: EvmTypes> Default for Interpreter<T> {
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
            tx_env: core::ptr::null(),
            message: core::ptr::null(),
            is_static: false,
            return_data: Bytes::new(),
            _marker: PhantomData,
        }
    }
}
