use super::{
    BytecodeRef, Gas, GasParams, Host, InstrStop, Memory, Message, Pc, PcMut, Result, Stack, State,
    Word,
    instructions::table::{GasTable, InstrTable, Instruction, TailInstrTable, make_tail_table},
};
use crate::{EvmConfig, bytecode::Bytecode, env::TxEnv};
use alloc::boxed::Box;
use alloy_primitives::Bytes;
use core::{hint::cold_path, marker::PhantomData};

/// Interpreter dispatch table mode.
#[derive(Clone, Copy)]
#[non_exhaustive]
pub(crate) enum Table<'a, C: EvmConfig> {
    /// Normal dispatch loop.
    Normal(&'a InstrTable<C>),
    /// Tail-call dispatch loop.
    Tail(&'a TailInstrTable<C>),
}

/// EVM interpreter.
#[derive(Debug)]
pub struct Interpreter<C: EvmConfig = crate::EvmVersion<()>> {
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
    _marker: PhantomData<fn() -> C>,
}

impl<C: EvmConfig> Interpreter<C> {
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
            gas_params: GasParams::new(C::GAS_PARAMS),
            memory: Memory::new(),
            tx_env,
            message,
            return_data: Bytes::new(),
            _marker: PhantomData,
        }
    }

    /// Runs the interpreter until it stops.
    pub fn run(&mut self, host: &mut dyn Host) -> InstrStop {
        let table = C::INSTRUCTION_IMPLS;
        self.run_with_table(Table::Normal(&table), &C::GAS_TABLE, host)
    }

    /// Runs the interpreter with tail-call dispatch until it stops.
    pub fn run_tail(&mut self, host: &mut dyn Host) -> InstrStop {
        let table = make_tail_table(C::INSTRUCTION_IMPLS);
        self.run_with_table(Table::Tail(&table), &C::GAS_TABLE, host)
    }

    pub(crate) fn run_with_table(
        &mut self,
        table: Table<'_, C>,
        gas_table: &GasTable,
        host: &mut dyn Host,
    ) -> InstrStop {
        let _gas_start = self.gas.remaining();

        let _r = match table {
            Table::Tail(table) => self.step_tail(table, gas_table, host).unwrap_err(),
            Table::Normal(table) => self.run_table_loop(table, gas_table, host),
        };

        #[cfg(feature = "std")]
        {
            eprintln!("execution stopped: {_r:?}");
            eprintln!("consumed gas: {}", _gas_start - self.gas.remaining())
        }

        _r
    }

    fn run_table_loop(
        &mut self,
        table: &InstrTable<C>,
        gas_table: &GasTable,
        host: &mut dyn Host,
    ) -> InstrStop {
        loop {
            if let Err(e) = self.step(table, gas_table, host) {
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
    fn step(&mut self, table: &InstrTable<C>, gas_table: &GasTable, host: &mut dyn Host) -> Result {
        let bytecode = BytecodeRef::new(&self.bytecode);
        let mut pc = PcMut::new(bytecode, &mut self.pc);
        let op = Self::pre_step(pc.reborrow(), &mut self.gas, gas_table)?;
        let mut stack = Stack::new(&mut self.stack, self.stack_len);
        let r = table[op as usize].unwrap_or_else(<dyn Instruction<C>>::default_unknown).execute(
            &mut stack,
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
                raw_interp: core::ptr::null_mut(),
            },
        );
        self.stack_len = stack.len;
        r
    }

    #[inline(always)]
    fn step_tail(
        &mut self,
        table: &TailInstrTable<C>,
        gas_table: &GasTable,
        host: &mut dyn Host,
    ) -> Result {
        let raw = self as *mut Self;
        let bytecode = BytecodeRef::new(&self.bytecode);
        let (op, pc) = {
            let mut pc_mut = PcMut::new(bytecode, &mut self.pc);
            let op = Self::pre_step(pc_mut.reborrow(), &mut self.gas, gas_table)?;
            (op, Pc::new(bytecode, pc_mut.get()))
        };
        let instr = table[op as usize];
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
                raw_interp: raw.cast(),
            },
            gas_table,
            instr.instr,
            table.as_ptr().cast(),
        );
        Err(e)
    }
}
