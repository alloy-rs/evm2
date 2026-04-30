use super::{CtrlRef, GasRef, Host, Result, Stack, State};

pub type InstrFnRet = (usize, Result);
pub type InstrFn = extern_table!(
    fn(ctrl: CtrlRef<'_>, stack: Stack<'_>, gas: GasRef<'_>, state: &mut State) -> InstrFnRet
);
pub type InstrTable = [InstrFn; 256];

pub type TailInstrFn = InstrFn;
pub type TailInstrTable = InstrTable;

pub type GasTable = [u16; 256];

#[allow(dead_code)]
pub struct InstructionCx<'a, 'ctrl, 'host> {
    pub ctrl: &'a mut CtrlRef<'ctrl>,
    pub gas: GasRef<'a>,
    pub host: &'a mut (dyn Host + 'host),
}
