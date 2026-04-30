use super::{GasRef, InstrErr, PcRef, Result, Stack, State};

pub type InstrFnRet = (usize, Result);
pub type InstrFn = extern_table!(
    fn(pc: PcRef<'_>, stack: Stack<'_>, gas: GasRef<'_>, state: &mut State) -> InstrFnRet
);
pub type InstrTable = [InstrFn; 256];

pub type TailInstrFnRet = InstrErr;
pub type TailInstrFn = InstrFn;
pub type TailInstrTable = InstrTable;

pub type GasTable = [u16; 256];
