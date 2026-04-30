use super::{GasRef, Host, PcRef, Result, Stack, State};

pub type InstrFn =
    fn(pc: &mut PcRef<'_>, stack: &mut Stack<'_>, gas: GasRef<'_>, state: &mut State) -> Result;
pub type InstrTable = [InstrFn; 256];

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
