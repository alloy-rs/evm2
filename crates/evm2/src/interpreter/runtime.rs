#[cfg(not(feature = "nightly"))]
use super::table::{InstructionTable, make_normal_instruction_table};
use super::{
    BytecodeRef, Gas, GasParams, Host, InstrStop, Memory, Message, PcMut, Result, Stack, State,
    Word,
};
#[cfg(feature = "nightly")]
use super::{
    Pc,
    table::{TailInstructionTable, make_tail_instruction_table},
};
use crate::{EvmConfig, bytecode::Bytecode, env::TxEnv};
use alloc::boxed::Box;
use alloy_primitives::Bytes;
#[cfg(not(feature = "nightly"))]
use core::hint::cold_path;

pub(crate) trait InterpreterConfig: EvmConfig {
    #[cfg(not(feature = "nightly"))]
    const INSTRUCTIONS: InstructionTable<Self> = make_normal_instruction_table::<Self>();

    #[cfg(feature = "nightly")]
    const TAIL_INSTRUCTIONS: TailInstructionTable<Self> = make_tail_instruction_table::<Self>();
}

impl<C: EvmConfig> InterpreterConfig for C {}

/// EVM interpreter.
#[derive(Debug)]
pub struct Interpreter {
    bytecode: Bytecode,
    pub(crate) pc: usize,
    pub(crate) stack: Box<[Word; Stack::CAPACITY]>,
    pub(crate) stack_len: usize,
    pub(crate) gas: Gas,
    pub(crate) gas_params: GasParams,
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
            gas_params: GasParams::new([0; 256]),
            memory: Memory::new(),
            tx_env,
            message,
            return_data: Bytes::new(),
        }
    }

    /// Runs the interpreter until it stops.
    pub fn run<C: EvmConfig>(&mut self, host: &mut dyn Host) -> InstrStop {
        self.gas_params = GasParams::new(C::GAS_PARAMS);
        let _gas_start = self.gas.remaining();

        #[cfg(feature = "nightly")]
        let _r = self.step_tail::<C>(host).unwrap_err();
        #[cfg(not(feature = "nightly"))]
        let _r = self.run_table_loop::<C>(host);

        #[cfg(feature = "std")]
        {
            eprintln!("execution stopped: {_r:?}");
            eprintln!("consumed gas: {}", _gas_start - self.gas.remaining())
        }

        _r
    }

    #[cfg(not(feature = "nightly"))]
    fn run_table_loop<C: EvmConfig>(&mut self, host: &mut dyn Host) -> InstrStop {
        loop {
            if let Err(e) = self.step::<C>(host) {
                cold_path();
                return e;
            }
        }
    }

    #[inline(always)]
    pub(crate) fn pre_step<C: EvmConfig>(mut pc: PcMut<'_>, gas: &mut Gas) -> Result<u8> {
        let op = pc.op();
        unsafe { pc.advance_unchecked(1) };
        gas.spend(C::GAS_TABLE[op as usize] as _)?;
        Ok(op)
    }

    #[inline(always)]
    #[cfg(not(feature = "nightly"))]
    fn step<C: EvmConfig>(&mut self, host: &mut dyn Host) -> Result {
        let raw = self as *mut Self;
        let bytecode = BytecodeRef::new(&self.bytecode);
        let mut pc = PcMut::new(bytecode, &mut self.pc);
        let op = Self::pre_step::<C>(pc.reborrow(), &mut self.gas)?;
        let instr = <C as InterpreterConfig>::INSTRUCTIONS[op as usize];
        let (len, r) = (instr.f)(
            Stack::new(&mut self.stack, self.stack_len),
            pc,
            &mut self.gas,
            &mut State {
                bytecode,
                host,
                tx: &self.tx_env,
                message: &self.message,
                memory: &mut self.memory,
                return_data: &self.return_data,
                spec: C::SPEC_ID,
                gas_params: &self.gas_params,
                raw_interp: raw,
            },
        );
        self.stack_len = len;
        r
    }

    #[inline(always)]
    #[cfg(feature = "nightly")]
    fn step_tail<C: EvmConfig>(&mut self, host: &mut dyn Host) -> Result {
        let raw = self as *mut Self;
        let bytecode = BytecodeRef::new(&self.bytecode);
        let (op, pc) = {
            let mut pc_mut = PcMut::new(bytecode, &mut self.pc);
            let op = Self::pre_step::<C>(pc_mut.reborrow(), &mut self.gas)?;
            (op, Pc::new(bytecode, pc_mut.get()))
        };
        let instr = <C as InterpreterConfig>::TAIL_INSTRUCTIONS[op as usize];
        let e = (instr.f)(
            Stack::new(&mut self.stack, self.stack_len),
            pc,
            &mut self.gas,
            &mut State {
                bytecode,
                host,
                tx: &self.tx_env,
                message: &self.message,
                memory: &mut self.memory,
                return_data: &self.return_data,
                spec: C::SPEC_ID,
                gas_params: &self.gas_params,
                raw_interp: raw,
            },
            <C as InterpreterConfig>::TAIL_INSTRUCTIONS.as_ptr().cast(),
            core::ptr::null(),
        );
        Err(e)
    }
}
