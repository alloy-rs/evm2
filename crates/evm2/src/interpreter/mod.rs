use core::{fmt, hint::cold_path};

#[cfg(feature = "nightly")]
macro_rules! tail_return {
    ($e:expr) => {
        become $e;
    };
}
#[cfg(not(feature = "nightly"))]
macro_rules! tail_return {
    ($e:expr) => {
        return $e;
    };
}

#[cfg(feature = "nightly")]
macro_rules! extern_table {
    ($(#[$attr:meta])* fn $($f:tt)*) => {
        $(#[$attr])* extern "rust-preserve-none" fn $($f)*
    };
}
#[cfg(not(feature = "nightly"))]
macro_rules! extern_table {
    ($(#[$attr:meta])* fn $($f:tt)*) => {
        $(#[$attr])* fn $($f)*
    };
}

macro_rules! opcodes {
    ($d:tt $($val:literal => $name:ident => $f:expr;)*) => {
        mod op {
            $(
                pub const $name: u8 = $val;
            )*
        }

        #[cfg(test)]
        const _: () = {
            $(
                let _ = $f;
            )*
        };

        /// Higher-order macro to iterate over all opcodes.
        macro_rules! for_each_opcode {
            ([$d ($d extra:tt)*] $d m:path) => {{
                $m!{[$d($d extra)*]
                    $(
                        ($name, $f),
                    )*
                }
            }};
        }
    };
}

pub(crate) type Word = u64;
pub(crate) type Result<T = (), E = InstrErr> = core::result::Result<T, E>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SpecId {
    Frontier,
    Homestead,
}

#[derive(Clone, Copy, Debug)]
pub enum InstrErr {
    Stop = 1,
    OutOfGas,
    StackOverflow,
    StackUnderflow,
    Invalid,
}

#[derive(Clone, Copy)]
pub struct Pc<'a> {
    base: *const u8,
    pc: usize,
    _marker: core::marker::PhantomData<&'a [u8]>,
}

pub struct PcRef<'a> {
    base: *const u8,
    pc: &'a mut usize,
    _marker: core::marker::PhantomData<&'a [u8]>,
}

impl<'a> Pc<'a> {
    pub(crate) fn new(bytecode: &'a [u8], pc: usize) -> Self {
        Self { base: bytecode.as_ptr(), pc, _marker: core::marker::PhantomData }
    }

    #[inline]
    pub fn as_mut(&mut self) -> PcRef<'_> {
        PcRef { base: self.base, pc: &mut self.pc, _marker: core::marker::PhantomData }
    }

    #[inline]
    pub unsafe fn advance_unchecked(&mut self, n: usize) {
        self.pc += n;
    }

    #[inline]
    pub fn op(&self) -> u8 {
        unsafe { *self.base.add(self.pc) }
    }

    #[inline]
    pub fn pc(&self) -> usize {
        self.pc
    }

    #[inline]
    pub unsafe fn read_bytes_unchecked(&self, n: usize) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.base.add(self.pc), n) }
    }
}

impl<'a> PcRef<'a> {
    pub(crate) fn new(bytecode: &'a [u8], pc: &'a mut usize) -> Self {
        Self { base: bytecode.as_ptr(), pc, _marker: core::marker::PhantomData }
    }

    #[inline]
    pub fn reborrow(&mut self) -> PcRef<'_> {
        unsafe { core::ptr::read(self) }
    }

    #[inline]
    pub unsafe fn advance_unchecked(&mut self, n: usize) {
        *self.pc += n;
    }

    #[inline]
    pub fn op(&self) -> u8 {
        unsafe { *self.base.add(*self.pc) }
    }

    #[inline]
    pub fn pc(&self) -> usize {
        *self.pc
    }

    #[inline]
    pub unsafe fn read_bytes_unchecked(&self, n: usize) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.base.add(*self.pc), n) }
    }
}

pub struct Stack<'a> {
    stack: &'a mut [Word; 1024],
    pub(crate) len: usize,
}

impl fmt::Debug for Stack<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_slice().fmt(f)
    }
}

impl<'a> Stack<'a> {
    #[inline]
    pub(crate) fn new(stack: &'a mut [Word; 1024], len: usize) -> Self {
        Self { stack, len }
    }

    #[inline]
    fn as_slice(&self) -> &[Word] {
        unsafe { core::slice::from_raw_parts(self.stack.as_ptr(), self.len) }
    }

    #[inline]
    pub fn push(&mut self, value: Word) -> Result {
        if self.len == 1024 {
            cold_path();
            return Err(InstrErr::StackOverflow);
        }
        unsafe { *self.stack.get_unchecked_mut(self.len) = value };
        self.len += 1;
        Ok(())
    }

    #[inline]
    pub fn pop(&mut self) -> Result<Word> {
        self.popn().map(|[x]| x)
    }

    #[inline]
    pub fn popn<const N: usize>(&mut self) -> Result<[Word; N]> {
        if self.len < N {
            cold_path();
            return Err(InstrErr::StackUnderflow);
        }
        Ok(unsafe { self.popn_unchecked() })
    }

    #[inline]
    pub unsafe fn popn_unchecked<const N: usize>(&mut self) -> [Word; N] {
        core::array::from_fn(|_| unsafe { self.pop_unchecked() })
    }

    #[inline(always)]
    pub fn popn_top<const N: usize>(&mut self) -> Result<([Word; N], &mut Word)> {
        if self.len < (N + 1) {
            cold_path();
            return Err(InstrErr::StackUnderflow);
        }
        let popped = unsafe { self.popn_unchecked() };
        let top = unsafe { self.top_unchecked() };
        Ok((popped, top))
    }

    #[inline]
    pub unsafe fn top_unchecked(&mut self) -> &mut Word {
        unsafe { self.stack.get_unchecked_mut(self.len - 1) }
    }

    #[inline]
    pub unsafe fn pop_unchecked(&mut self) -> Word {
        self.len -= 1;
        unsafe { *self.stack.get_unchecked(self.len) }
    }
}

#[derive(Clone, Copy)]
pub struct Gas {
    pub(crate) remaining: u64,
}

pub type GasRef<'a> = &'a mut Gas;

impl Gas {
    pub(crate) fn new(remaining: u64) -> Self {
        Self { remaining }
    }

    #[inline(always)]
    pub fn spend(&mut self, amount: u64) -> Result {
        let overflow;
        (self.remaining, overflow) = self.remaining.overflowing_sub(amount);
        if overflow {
            cold_path();
            Err(InstrErr::OutOfGas)
        } else {
            Ok(())
        }
    }
}

/// Catch all. Rest of stuff, cold.
#[allow(unused)]
pub struct State<'a> {
    pub host: &'a mut (dyn Host + 'a),
    pub spec: SpecId,
    raw_interp: *mut Interpreter,
}

pub trait Host {
    fn balance(&self, address: Word) -> Word;
}

mod instruction;
mod instructions;

#[cfg(test)]
use self::instructions::{add, balance, push, stop};

opcodes! {$
    0x00 => STOP => stop;
    0x01 => ADD  => add;

    0x31 => BALANCE => balance;

    0x5f => PUSH0 => push::<0>;
    0x60 => PUSH1 => push::<1>;
}

mod runtime;
mod table;

pub use instruction::*;
pub use runtime::{Interpreter, Table};
pub use table::{
    DEFAULT_GAS_TABLE, DEFAULT_TABLE, DEFAULT_TAIL_TABLE, make_table, make_tail_table, mk_dispatch,
    mk_tail_dispatch, new_gas_table,
};

#[inline(always)]
pub(crate) fn likely(b: bool) -> bool {
    if b {
        true
    } else {
        cold_path();
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyHost;

    impl Host for DummyHost {
        fn balance(&self, address: Word) -> Word {
            address
        }
    }

    #[test]
    fn main_smoke() {
        #[rustfmt::skip]
        let bytecode = core::hint::black_box(&[
            op::PUSH1, 0x01,
            op::PUSH1, 0x02,
            op::ADD,
            op::STOP,
        ][..]);
        let spec_id = core::hint::black_box(SpecId::Homestead);
        let instruction_table = core::hint::black_box(Table::Tail(&DEFAULT_TAIL_TABLE));

        let gas_table = new_gas_table(spec_id);
        let mut interpreter = Interpreter::new(bytecode.into(), spec_id);
        interpreter.run(instruction_table, &gas_table, &mut DummyHost);
    }

    #[test]
    fn basic() {
        const BASIC: &[u8] = &[op::PUSH1, 0x01, op::PUSH1, 0x02, op::ADD, op::STOP];

        for spec in [SpecId::Frontier, SpecId::Homestead] {
            let gas_table = new_gas_table(spec);
            for (_name, table) in [
                ("normal", Table::Normal(&DEFAULT_TABLE)),
                ("tail", Table::Tail(&DEFAULT_TAIL_TABLE)),
            ] {
                let mut interpreter = Interpreter::new(BASIC.into(), spec);
                interpreter.run(table, &gas_table, &mut DummyHost);
                assert!(interpreter.gas.remaining > 0);
                assert_eq!(interpreter.pc, 6);
                assert_eq!(interpreter.stack_len, 1);
                assert_eq!(interpreter.stack[0], 3);
            }
        }
    }
}
