use core::mem;

use super::{Gas, InstrErr, Pc, PcRef, Result, Stack, State};

pub type InstrFnRet = (usize, Result);
pub type InstrFn = extern_table!(
    fn(pc: PcRef<'_>, stack: Stack<'_>, gas: GasRef<'_>, state: &mut State) -> InstrFnRet
);
pub type InstrTable = [InstrFn; 256];

pub type TailInstrFnRet = InstrErr;
pub type TailInstrFn = extern_table!(
    fn(
        pc: Pc<'_>,
        stack: Stack<'_>,
        gas: Gas,
        state: &mut State,
        gas_table: &GasTable,
        instr_tablep: *const (), /* Type-erased pointer to `TailInstrTable`. Would otherwise
                                  * result in infinite recursion. */
    ) -> TailInstrFnRet
);
pub type TailInstrTable = [TailInstrFn; 256];

pub type GasTable = [u16; 256];

pub type GasRef<'a> = super::GasRef<'a>;

pub trait Instruction<T>: Sized {
    fn new() -> Self {
        const { assert!(size_of::<Self>() == 0) };
        // SAFETY: `Self` is a ZST.
        unsafe { mem::zeroed::<Self>() }
    }
    fn execute(self, pc: PcRef, stack: &mut Stack, gas: &mut Gas, state: &mut State) -> Result;
}

macro_rules! impl_instruction {
    (($($t:ty),* $(,)?) = |$pc:ident, $stack:ident, $gas:ident, $state:ident| ( $($e:tt)* )) => {
        impl<F: FnOnce($($t,)*) -> Result> Instruction<($($t,)*)> for F {
            #[inline(always)]
            fn execute(self, $pc: PcRef<'_>, $stack: &mut Stack<'_>, $gas: &mut Gas, $state: &mut State<'_>) -> Result {
                self($($e)*)
            }
        }
    };
}

impl_instruction!(() = |_pc, _s, _g, _st| ());
impl_instruction!((PcRef<'_>, &mut Stack<'_>) = |pc, s, _g, _st| (pc, s));
impl_instruction!((&mut Stack<'_>) = |_pc, s, _g, _st| (s));
impl_instruction!((&mut Stack<'_>, &mut Gas) = |_pc, s, g, _st| (s, g));
impl_instruction!((&mut Stack<'_>, &mut State<'_>) = |_pc, s, _g, st| (s, st));
