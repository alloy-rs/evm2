use super::{GasRef, Host, InstrErr, PcRef, Result, Stack, State};

pub type InstrFnRet = (usize, Result);
pub type InstrFn = extern_table!(
    fn(pc: PcRef<'_>, stack: Stack<'_>, gas: GasRef<'_>, state: &mut State) -> InstrFnRet
);
pub type InstrTable = [InstrFn; 256];

pub type TailInstrFnRet = InstrErr;
pub type TailInstrFn = InstrFn;
pub type TailInstrTable = InstrTable;

pub type GasTable = [u16; 256];

#[allow(dead_code)]
pub struct InstructionCx<'a, 'pc, 'stack, 'host> {
    pub pc: &'a mut PcRef<'pc>,
    pub stack: &'a mut Stack<'stack>,
    pub gas: GasRef<'a>,
    pub host: &'a mut (dyn Host + 'host),
}
