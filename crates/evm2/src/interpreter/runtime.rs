use super::{
    BytecodeRef, Gas, InstrStop, Memory, Message, MessageKind, Pc, Result, Stack, State, Word,
    table::InstructionTables,
};
use crate::{EvmConfig, EvmTypes, bytecode::Bytecode, env::TxEnv};
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
    tx_env: TxEnv,
    pub(crate) message: Message,
    pub(crate) is_static: bool,
    pub(crate) return_data: Bytes,
    _marker: PhantomData<fn() -> T>,
}

impl<T: EvmTypes> Interpreter<T> {
    /// Creates an interpreter from analyzed bytecode, a transaction-global environment, and a
    /// frame-local message.
    pub fn new(
        bytecode: Bytecode,
        tx_env: TxEnv,
        message: Message,
        caller_is_static: bool,
    ) -> Self {
        let gas_limit = message.gas_limit;
        let is_static = caller_is_static || matches!(message.kind, MessageKind::StaticCall);
        Self {
            pc: bytecode.original_byte_slice().as_ptr(),
            bytecode,
            // SAFETY: `Word` is valid at any bitpattern. It's not read before init anyway.
            stack: unsafe { Box::new_uninit().assume_init() },
            stack_len: 0,
            gas: Gas::new(gas_limit),
            memory: Memory::new(),
            result: Ok(()),
            output: &[],
            tx_env,
            message,
            is_static,
            return_data: Bytes::new(),
            _marker: PhantomData,
        }
    }

    #[cfg(test)]
    pub(crate) const fn stack_len(&self) -> usize {
        self.stack_len
    }

    #[inline]
    pub(crate) const fn tx_env(&self) -> &TxEnv {
        &self.tx_env
    }

    #[inline]
    pub(crate) const fn message(&self) -> &Message {
        &self.message
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

    /// Runs the interpreter until it stops.
    pub fn run<C: EvmConfig>(&mut self, host: &mut T::Host) -> InstrStop {
        let _gas_start = self.gas.remaining();

        #[cfg(feature = "nightly")]
        let r = self.step_tail::<C>(host);
        #[cfg(not(feature = "nightly"))]
        let r = self.run_table_loop::<C>(host);

        r
    }

    #[cfg(not(feature = "nightly"))]
    fn run_table_loop<C: EvmConfig>(&mut self, host: &mut T::Host) -> InstrStop {
        let mut pc = self.pc;
        let mut stack_len = self.stack_len;
        loop {
            let (next_pc, next_stack_len, flow) = self.raw_step::<C>(host, pc, stack_len);
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
    #[inline(always)]
    #[cfg(not(feature = "nightly"))]
    pub fn step<C: EvmConfig>(&mut self, host: &mut T::Host) -> ControlFlow<(), ()> {
        let (pc, stack_len, flow) = self.raw_step::<C>(host, self.pc, self.stack_len);
        self.pc = pc;
        self.stack_len = stack_len;
        flow
    }

    #[inline(always)]
    #[cfg(not(feature = "nightly"))]
    fn raw_step<C: EvmConfig>(
        &mut self,
        host: &mut T::Host,
        pc: *const u8,
        stack_len: usize,
    ) -> (*const u8, usize, ControlFlow<(), ()>) {
        let raw = self as *mut Self;
        let bytecode = BytecodeRef::new(&self.bytecode);
        let pc = Pc::from_ptr(pc);
        let op = pc.op();
        let instr = <T as InstructionTables<C>>::INSTRUCTIONS[op as usize];
        let (pc, stack_len) = instr(
            pc,
            Stack::new(&mut self.stack, stack_len),
            &mut self.gas,
            &mut State { bytecode, host, spec: C::spec_id(), raw_interp: raw },
        );
        let flow = if pc.is_null() { Break(()) } else { Continue(()) };
        (pc, stack_len, flow)
    }

    #[inline(always)]
    #[cfg(feature = "nightly")]
    fn step_tail<C: EvmConfig>(&mut self, host: &mut T::Host) -> InstrStop {
        let raw = self as *mut Self;
        let bytecode = BytecodeRef::new(&self.bytecode);
        let pc = Pc::from_ptr(self.pc);
        let op = pc.op();
        let instr = <T as InstructionTables<C>>::INSTRUCTIONS[op as usize];
        instr(
            pc,
            Stack::new(&mut self.stack, self.stack_len),
            &mut self.gas,
            &mut State { bytecode, host, spec: C::spec_id(), raw_interp: raw },
            0,
        );
        self.result.unwrap_err()
    }
}
