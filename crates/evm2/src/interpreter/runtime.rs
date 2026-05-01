use super::{
    BytecodeRef, Gas, GasParams, Host, InstrStop, Memory, Message, PcMut, Result, Stack, State,
    Word, instructions::table::GasTable,
};
use crate::{EvmConfig, bytecode::Bytecode, env::TxEnv};
use alloc::boxed::Box;
use alloy_primitives::Bytes;
use core::hint::cold_path;

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

        let _r = self.run_table_loop::<C>(&C::GAS_TABLE, host);

        #[cfg(feature = "std")]
        {
            eprintln!("execution stopped: {_r:?}");
            eprintln!("consumed gas: {}", _gas_start - self.gas.remaining())
        }

        _r
    }

    fn run_table_loop<C: EvmConfig>(
        &mut self,
        gas_table: &GasTable,
        host: &mut dyn Host,
    ) -> InstrStop {
        loop {
            if let Err(e) = self.step::<C>(gas_table, host) {
                cold_path();
                return e;
            }
        }
    }

    #[inline(always)]
    pub(crate) fn pre_step(mut pc: PcMut<'_>, gas: &mut Gas, gas_table: &GasTable) -> Result<u8> {
        let op = pc.op();
        unsafe { pc.advance_unchecked(1) };
        gas.spend(gas_table[op as usize] as _)?;
        Ok(op)
    }

    #[inline(always)]
    fn step<C: EvmConfig>(&mut self, gas_table: &GasTable, host: &mut dyn Host) -> Result {
        let raw = self as *mut Self;
        let bytecode = BytecodeRef::new(&self.bytecode);
        let mut pc = PcMut::new(bytecode, &mut self.pc);
        let op = Self::pre_step(pc.reborrow(), &mut self.gas, gas_table)?;
        let instr = C::INSTRUCTIONS[op as usize];
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
            instr.instr,
        );
        self.stack_len = len;
        r
    }
}
