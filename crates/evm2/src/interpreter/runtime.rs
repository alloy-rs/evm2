use super::{
    Ctrl, CtrlRef, Gas, Host, InstrErr, Memory, Result, SpecId, Stack, State, Word,
    instructions::table::{GasTable, InstrTable, TailInstrTable},
};
use alloc::{boxed::Box, vec::Vec};
use core::hint::cold_path;

/// Interpreter dispatch table mode.
#[non_exhaustive]
#[derive(Clone, Copy, Debug)]
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
    pub fn run(&mut self, table: Table<'_>, gas_table: &GasTable, host: &mut dyn Host) {
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
        mut ctrl: CtrlRef<'_>,
        gas: &mut Gas,
        gas_table: &GasTable,
    ) -> Result<u8> {
        let op = ctrl.op();
        unsafe { ctrl.advance_unchecked(1) };
        gas.spend(gas_table[op as usize] as _)?;
        Ok(op)
    }

    #[inline(always)]
    fn step(&mut self, table: &InstrTable, gas_table: &GasTable, host: &mut dyn Host) -> Result {
        let mut ctrl = CtrlRef::new(&self.bytecode, &mut self.pc);
        let op = Self::pre_step(ctrl.reborrow(), &mut self.gas, gas_table)?;
        let r;
        (self.stack_len, r) = table[op as usize](
            ctrl,
            Stack::new(&mut self.stack, self.stack_len),
            &mut self.gas,
            &mut State {
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
        let mut ctrl = Ctrl::new(&self.bytecode, self.pc);
        let op = Self::pre_step(ctrl.as_mut(), &mut self.gas, gas_table)?;
        let e = table[op as usize](
            ctrl,
            Stack::new(&mut self.stack, self.stack_len),
            self.gas,
            &mut State { host, memory: &mut self.memory, spec: self.spec_id, raw_interp: raw },
            gas_table,
            table.as_ptr().cast(),
        );
        Err(e)
    }
}
