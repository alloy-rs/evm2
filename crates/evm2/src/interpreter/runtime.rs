#[cfg(feature = "nightly")]
use super::Pc;
#[cfg(not(feature = "nightly"))]
use super::PcMut;
use super::{
    BytecodeRef, Gas, InstrStop, Memory, Message, Result, StackMut, State, Word,
    table::InstructionTables,
};
use crate::{EvmConfig, bytecode::Bytecode, env::TxEnv};
use alloc::boxed::Box;
use alloy_primitives::Bytes;
#[cfg(not(feature = "nightly"))]
use core::hint::cold_path;

/// EVM interpreter.
#[derive(Debug)]
pub struct Interpreter {
    bytecode: Bytecode,
    pub(crate) pc: usize,
    pub(crate) stack: Box<[Word; StackMut::CAPACITY]>,
    pub(crate) stack_len: usize,
    pub(crate) gas: Gas,
    pub(crate) memory: Memory,
    tx_env: TxEnv,
    pub(crate) message: Message,
    pub(crate) return_data: Bytes,
}

impl Interpreter {
    /// Creates an interpreter from analyzed bytecode, a transaction-global environment, and a
    /// frame-local message.
    pub fn new(bytecode: Bytecode, tx_env: TxEnv, message: Message) -> Self {
        let gas_limit = message.gas_limit;
        Self {
            bytecode,
            pc: 0,
            // SAFETY: `Word` is valid at any bitpattern. It's not read before init anyway.
            stack: unsafe { Box::new_uninit().assume_init() },
            stack_len: 0,
            gas: Gas::new(gas_limit),
            memory: Memory::new(),
            tx_env,
            message,
            return_data: Bytes::new(),
        }
    }

    /// Runs the interpreter until it stops.
    pub fn run<C: EvmConfig>(&mut self, host: &mut C::Host) -> InstrStop {
        let _gas_start = self.gas.remaining();

        #[cfg(feature = "nightly")]
        let r = self.step_tail::<C>(host).unwrap_err();
        #[cfg(not(feature = "nightly"))]
        let r = self.run_table_loop::<C>(host);

        #[cfg(feature = "std")]
        {
            eprintln!("execution stopped: {r:?}");
            eprintln!("consumed gas: {}", _gas_start - self.gas.remaining())
        }

        r
    }

    #[cfg(not(feature = "nightly"))]
    fn run_table_loop<C: EvmConfig>(&mut self, host: &mut C::Host) -> InstrStop {
        loop {
            if let Err(e) = self.step::<C>(host) {
                cold_path();
                return e;
            }
        }
    }

    #[inline(always)]
    #[cfg(not(feature = "nightly"))]
    fn step<C: EvmConfig>(&mut self, host: &mut C::Host) -> Result {
        let raw = self as *mut Self;
        let bytecode = BytecodeRef::new(&self.bytecode);
        let pc = PcMut::new(bytecode, &mut self.pc);
        let op = pc.op();
        let instr = <C as InstructionTables>::INSTRUCTIONS[op as usize];
        let (pc, r) = instr(
            pc,
            StackMut::new(&mut self.stack, &mut self.stack_len),
            &mut self.gas,
            &mut State {
                bytecode,
                host,
                tx: &self.tx_env,
                message: &self.message,
                memory: &mut self.memory,
                return_data: &self.return_data,
                spec: C::SPEC_ID,
                raw_interp: raw,
            },
        );
        self.pc = pc;
        r
    }

    #[inline(always)]
    #[cfg(feature = "nightly")]
    fn step_tail<C: EvmConfig>(&mut self, host: &mut C::Host) -> Result {
        let raw = self as *mut Self;
        let bytecode = BytecodeRef::new(&self.bytecode);
        let pc = Pc::new(bytecode, self.pc);
        let op = pc.op();
        let instr = <C as InstructionTables>::TAIL_INSTRUCTIONS[op as usize];
        let e = instr(
            pc,
            StackMut::new(&mut self.stack, &mut self.stack_len),
            &mut self.gas,
            &mut State {
                bytecode,
                host,
                tx: &self.tx_env,
                message: &self.message,
                memory: &mut self.memory,
                return_data: &self.return_data,
                spec: C::SPEC_ID,
                raw_interp: raw,
            },
            InstrStop::Stop,
        );
        Err(e)
    }
}
