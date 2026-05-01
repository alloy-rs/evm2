use super::{
    BytecodeRef, Gas, Host, InstrErr, Memory, Pc, Result, SpecId, Stack, State, Word,
    instructions::table::{GasTable, InstrTable, TailInstrTable},
};
use alloc::{boxed::Box, vec::Vec};
use core::hint::cold_path;

/// Interpreter dispatch table mode.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub enum Table<'a> {
    /// Normal dispatch loop.
    Normal(&'a InstrTable),
    /// Tail-call dispatch loop.
    Tail(&'a TailInstrTable),
}

/// EVM interpreter.
#[derive(Debug)]
pub struct Interpreter {
    bytecode: Vec<u8>,
    pub(crate) pc: usize,
    pub(crate) stack: Box<[Word; 1024]>,
    pub(crate) stack_len: usize,
    pub(crate) gas: Gas,
    pub(crate) memory: Memory,
    spec_id: SpecId,
}

impl Interpreter {
    /// Creates an interpreter from bytecode and a spec identifier.
    pub fn new(bytecode: Vec<u8>, spec_id: SpecId) -> Self {
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
    pub fn run(&mut self, table: Table<'_>, gas_table: &GasTable, host: &mut dyn Host) -> InstrErr {
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
    ) -> InstrErr {
        loop {
            if let Err(e) = self.step(table, gas_table, host) {
                cold_path();
                return e;
            }
        }
    }

    #[inline(always)]
    pub(crate) fn pre_step(
        pc: &mut Pc,
        gas: &mut Gas,
        bytecode: BytecodeRef<'_>,
        gas_table: &GasTable,
    ) -> Result<u8> {
        let op = bytecode.op(pc);
        unsafe { pc.advance_unchecked(1) };
        gas.spend(gas_table[op as usize] as _)?;
        Ok(op)
    }

    #[inline(always)]
    fn step(&mut self, table: &InstrTable, gas_table: &GasTable, host: &mut dyn Host) -> Result {
        let bytecode = BytecodeRef::new(&self.bytecode);
        let mut pc = Pc::new(self.pc);
        let op = Self::pre_step(&mut pc, &mut self.gas, bytecode, gas_table)?;
        let r;
        (self.stack_len, r) = table[op as usize](
            &mut pc,
            Stack::new(&mut self.stack, self.stack_len),
            &mut self.gas,
            bytecode,
            &mut State {
                host,
                memory: &mut self.memory,
                spec: self.spec_id,
                raw_interp: core::ptr::null_mut(),
            },
        );
        self.pc = pc.get();
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
        let mut pc = Pc::new(self.pc);
        let op = Self::pre_step(&mut pc, &mut self.gas, bytecode, gas_table)?;
        let e = table[op as usize](
            pc,
            Stack::new(&mut self.stack, self.stack_len),
            self.gas,
            bytecode,
            &mut State { host, memory: &mut self.memory, spec: self.spec_id, raw_interp: raw },
            gas_table,
            table.as_ptr().cast(),
        );
        Err(e)
    }
}
