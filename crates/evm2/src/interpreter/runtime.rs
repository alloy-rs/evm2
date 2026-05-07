#[cfg(feature = "nightly")]
use super::gas::RemainingGas;
use super::{
    BytecodeRef, Gas, InstrStop, Memory, Message, MessageKind, Pc, Result, Stack, State, Word,
};
use crate::{EvmConfig, EvmTypes, ExecutionConfig, bytecode::Bytecode, env::TxEnv};
use alloc::{boxed::Box, vec::Vec};
use alloy_primitives::Bytes;
#[cfg(not(feature = "nightly"))]
use core::hint::cold_path;
use core::marker::PhantomData;

/// EVM interpreter.
#[derive(Debug)]
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
            _marker: PhantomData,
        }
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
        self.return_data.clear();
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

    #[inline]
    pub(crate) const fn message(&self) -> &Message {
        self.message.expect("interpreter message is initialized")
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

        #[cfg(feature = "nightly")]
        let r = self.step_tail(config, host);
        #[cfg(not(feature = "nightly"))]
        let r = self.run_table_loop(config, host);

        r
    }

    #[cfg(not(feature = "nightly"))]
    fn run_table_loop(&mut self, config: &ExecutionConfig<T>, host: &mut T::Host) -> InstrStop {
        #[expect(clippy::unnecessary_cast, reason = "cast erases the active interpreter lifetime")]
        let raw = self as *mut Self as *mut Interpreter<'_, T>;
        let mut pc = Pc::new(self.pc);
        let mut stack_len = self.stack_len;
        let stack = &mut *self.stack;
        let bytecode = BytecodeRef::new(&self.bytecode);
        let mut state = State {
            bytecode,
            host,
            spec: config.version.spec_id,
            gas: self.gas,
            result: Ok(()),
            version: &config.version,
            raw_interp: raw,
        };
        loop {
            let op = pc.op();
            let instr = config.instructions[op as usize];
            let (next_pc, next_stack_len) =
                instr(pc, Stack::new(&mut *stack, stack_len), &mut state);
            pc = Pc::new(next_pc);
            stack_len = next_stack_len;
            if next_pc.is_null() {
                cold_path();
                self.pc = next_pc;
                self.stack_len = stack_len;
                self.gas = state.gas;
                return self.result.unwrap_err();
            }
        }
    }

    #[inline(always)]
    #[cfg(feature = "nightly")]
    fn step_tail(&mut self, config: &ExecutionConfig<T>, host: &mut T::Host) -> InstrStop {
        #[expect(clippy::unnecessary_cast, reason = "cast erases the active interpreter lifetime")]
        let raw = self as *mut Self as *mut Interpreter<'_, T>;
        let bytecode = BytecodeRef::new(&self.bytecode);
        let pc = Pc::new(self.pc);
        let op = pc.op();
        let instr = config.instructions[op as usize];
        let stack = &mut *self.stack;
        let remaining_gas = RemainingGas::new(self.gas.remaining());
        let mut state = State {
            bytecode,
            host,
            spec: config.version.spec_id,
            gas: self.gas,
            result: Ok(()),
            version: &config.version,
            raw_interp: raw,
        };
        instr(pc, Stack::new(&mut *stack, self.stack_len), remaining_gas, &mut state);
        self.result.unwrap_err()
    }
}

#[derive(Debug, Default)]
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
