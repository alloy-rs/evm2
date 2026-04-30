use super::{
    Gas, Host, InstrErr, PcRef, Result, SpecId, Stack, State, Word,
    instruction::{GasTable, InstrTable, TailInstrTable},
};
use alloc::{boxed::Box, vec::Vec};
use core::hint::cold_path;

#[derive(Clone, Copy)]
pub enum Table<'a> {
    Normal(&'a InstrTable),
    Tail(&'a TailInstrTable),
}

pub struct Interpreter {
    bytecode: Vec<u8>,
    pub(crate) pc: usize,
    pub(crate) stack: Box<[Word; 1024]>,
    pub(crate) stack_len: usize,
    pub(crate) gas: Gas,
    spec_id: SpecId,
}

impl Interpreter {
    pub fn new(bytecode: Vec<u8>, spec_id: SpecId) -> Self {
        Self {
            bytecode,
            pc: 0,
            // SAFETY: `Word` is valid at any bitpattern. It's not read before init anyway.
            stack: unsafe { Box::new_uninit().assume_init() },
            stack_len: 0,
            gas: Gas::new(10_000),
            spec_id,
        }
    }

    pub fn run(&mut self, table: Table<'_>, gas_table: &GasTable, host: &mut dyn Host) {
        let _gas_start = self.gas.remaining;

        let _r = match table {
            Table::Tail(table) => self.run_table_loop(table, gas_table, host),
            Table::Normal(table) => self.run_table_loop(table, gas_table, host),
        };

        #[cfg(feature = "std")]
        {
            eprintln!("execution stopped: {_r:?}");
            eprintln!("consumed gas: {}", _gas_start - self.gas.remaining)
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
    pub(crate) fn pre_step(mut pc: PcRef<'_>, gas: &mut Gas, gas_table: &GasTable) -> Result<u8> {
        let op = pc.op();
        unsafe { pc.advance_unchecked(1) };
        gas.spend(gas_table[op as usize] as _)?;
        Ok(op)
    }

    #[inline(always)]
    fn step(&mut self, table: &InstrTable, gas_table: &GasTable, host: &mut dyn Host) -> Result {
        let mut pc = PcRef::new(&self.bytecode, &mut self.pc);
        let op = Self::pre_step(pc.reborrow(), &mut self.gas, gas_table)?;
        let r;
        (self.stack_len, r) = table[op as usize](
            pc,
            Stack::new(&mut self.stack, self.stack_len),
            &mut self.gas,
            &mut State { host, spec: self.spec_id, raw_interp: core::ptr::null_mut() },
        );
        r
    }
}
