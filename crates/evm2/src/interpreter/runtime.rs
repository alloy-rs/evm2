use alloc::{boxed::Box, vec::Vec};
use core::hint::cold_path;

use super::{
    DEFAULT_TABLE, Gas, Host, InstrErr, Pc, PcRef, Result, SpecId, Stack, State,
    instruction::{GasTable, InstrTable, Instruction, TailInstrTable},
    instructions::{add, balance, push, stop},
    likely, op,
};

#[derive(Clone, Copy)]
pub enum Table<'a> {
    Normal(&'a InstrTable),
    Tail(&'a TailInstrTable),
}

pub struct Interpreter {
    bytecode: Vec<u8>,
    pub(crate) pc: usize,
    pub(crate) stack: Box<[u64; 1024]>,
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
            Table::Tail(table) => self.step_tail(table, gas_table, host).unwrap_err(),
            Table::Normal(table) => {
                if likely(core::ptr::eq(table, &DEFAULT_TABLE)) {
                    self.run_match_loop(gas_table, host)
                } else {
                    self.run_table_loop(table, gas_table, host)
                }
            }
        };

        #[cfg(feature = "std")]
        {
            eprintln!("execution stopped: {_r:?}");
            eprintln!("consumed gas: {}", _gas_start - self.gas.remaining)
        }
    }

    #[inline(never)]
    fn run_match_loop(&mut self, gas_table: &GasTable, host: &mut dyn Host) -> InstrErr {
        // TODO: do these local copies do anything?
        let mut pc_real = self.pc;
        let mut pc = PcRef::new(&self.bytecode, &mut pc_real);

        let stack = &mut Stack::new(&mut self.stack, self.stack_len);

        let mut gas_real = self.gas;
        let gas = &mut gas_real;

        let state = &mut State { host, spec: self.spec_id, raw_interp: core::ptr::null_mut() };

        let e = loop {
            let op = match Self::pre_step(pc.reborrow(), gas, gas_table) {
                Ok(op) => op,
                Err(e) => {
                    cold_path();
                    break e;
                }
            };
            let pc = pc.reborrow();

            macro_rules! make_match {
                ([] $(
                    ($op:ident, $fn:expr),
                )*) => {
                    match op {
                        $(op::$op => $fn.execute(pc, stack, gas, state),)*
                        _ => {
                            cold_path();
                            Err(InstrErr::Invalid)
                        }
                    }
                };
            }
            if let Err(e) = for_each_opcode!([] make_match) {
                cold_path();
                break e;
            }
        };

        self.pc = pc_real;
        self.gas = gas_real;
        self.stack_len = stack.len;

        e
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

    #[inline(always)]
    fn step_tail(
        &mut self,
        table: &TailInstrTable,
        gas_table: &GasTable,
        host: &mut dyn Host,
    ) -> Result {
        let raw = self as *mut _;
        let mut pc = Pc::new(&self.bytecode, self.pc);
        let op = Self::pre_step(pc.as_mut(), &mut self.gas, gas_table)?;
        let e = table[op as usize](
            pc,
            Stack::new(&mut self.stack, self.stack_len),
            self.gas,
            &mut State { host, spec: self.spec_id, raw_interp: raw },
            gas_table,
            table.as_ptr().cast(),
        );
        Err(e)
    }
}
