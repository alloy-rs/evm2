use super::{
    BytecodeRef, Gas, Host, InstrStop, Memory, Pc, PcMut, Result, SpecId, Stack, State, Word,
    instructions::table::{
        DEFAULT_TABLE, DEFAULT_TAIL_TABLE, GasTable, InstrTable, TailInstrTable, new_gas_table,
    },
};
use crate::bytecode::Bytecode;
use alloc::boxed::Box;
use core::hint::cold_path;

/// Interpreter dispatch table mode.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub(in crate::interpreter) enum Table<'a> {
    /// Normal dispatch loop.
    Normal(&'a InstrTable),
    /// Tail-call dispatch loop.
    Tail(&'a TailInstrTable),
}

/// EVM interpreter.
#[derive(Debug)]
pub struct Interpreter {
    bytecode: Bytecode,
    pub(crate) pc: usize,
    pub(crate) stack: Box<[Word; Stack::CAPACITY]>,
    pub(crate) stack_len: usize,
    pub(crate) gas: Gas,
    pub(crate) memory: Memory,
    spec_id: SpecId,
}

impl Interpreter {
    /// Creates an interpreter from analyzed bytecode and a spec identifier.
    pub fn new(bytecode: Bytecode, spec_id: SpecId) -> Self {
        Self {
            bytecode,
            pc: 0,
            // SAFETY: `Word` is valid at any bitpattern. It's not read before init anyway.
            stack: unsafe { Box::new_uninit().assume_init() },
            stack_len: 0,
            gas: Gas::new(10_000),
            memory: Memory::new(),
            spec_id,
        }
    }

    /// Runs the interpreter until it stops.
    pub fn run(&mut self, host: &mut dyn Host) -> InstrStop {
        let gas_table = new_gas_table(self.spec_id);
        self.run_with_table(Table::Normal(&DEFAULT_TABLE), &gas_table, host)
    }

    /// Runs the interpreter with tail-call dispatch until it stops.
    pub fn run_tail(&mut self, host: &mut dyn Host) -> InstrStop {
        let gas_table = new_gas_table(self.spec_id);
        self.run_with_table(Table::Tail(&DEFAULT_TAIL_TABLE), &gas_table, host)
    }

    pub(in crate::interpreter) fn run_with_table(
        &mut self,
        table: Table<'_>,
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
        table: &InstrTable,
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
    fn step(&mut self, table: &InstrTable, gas_table: &GasTable, host: &mut dyn Host) -> Result {
        let bytecode = BytecodeRef::new(&self.bytecode);
        let mut pc = PcMut::new(bytecode, &mut self.pc);
        let op = Self::pre_step(pc.reborrow(), &mut self.gas, gas_table)?;
        let r;
        (self.stack_len, r) = table[op as usize](
            Stack::new(&mut self.stack, self.stack_len),
            pc,
            &mut self.gas,
            &mut State {
                bytecode,
                host,
                memory: &mut self.memory,
                spec: self.spec_id,
                raw_interp: core::ptr::null_mut(),
            },
        );
        r
    }

    #[inline(always)]
    fn step_tail(
        &mut self,
        table: &TailInstrTable,
        gas_table: &GasTable,
        host: &mut dyn Host,
    ) -> Result {
        let raw = self as *mut _;
        let bytecode = BytecodeRef::new(&self.bytecode);
        let (op, pc) = {
            let mut pc_mut = PcMut::new(bytecode, &mut self.pc);
            let op = Self::pre_step(pc_mut.reborrow(), &mut self.gas, gas_table)?;
            (op, Pc::new(bytecode, pc_mut.get()))
        };
        let e = table[op as usize](
            Stack::new(&mut self.stack, self.stack_len),
            pc,
            self.gas,
            &mut State {
                bytecode,
                host,
                memory: &mut self.memory,
                spec: self.spec_id,
                raw_interp: raw,
            },
            gas_table,
            table.as_ptr().cast(),
        );
        Err(e)
    }
}
