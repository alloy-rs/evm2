#![cfg_attr(
    feature = "nightly",
    feature(explicit_tail_calls, rust_preserve_none_cc),
    allow(incomplete_features)
)]
#![allow(clippy::missing_safety_doc)]
#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

use alloc::{boxed::Box, vec::Vec};
use core::{fmt, hint::cold_path, mem};

#[inline(always)]
fn likely(b: bool) -> bool {
    if b {
        true
    } else {
        cold_path();
        false
    }
}

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

opcodes! {$
    0x00 => STOP => stop;
    0x01 => ADD  => add;

    0x31 => BALANCE => balance;

    0x5f => PUSH0 => push::<0>;
    0x60 => PUSH1 => push::<1>;
}

type Word = u64;
type Result<T = (), E = InstrErr> = core::result::Result<T, E>;

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
    fn new(bytecode: &'a [u8], pc: usize) -> Self {
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
    fn new(bytecode: &'a [u8], pc: &'a mut usize) -> Self {
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
    len: usize,
}

impl fmt::Debug for Stack<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_slice().fmt(f)
    }
}

impl<'a> Stack<'a> {
    #[inline]
    fn new(stack: &'a mut [Word; 1024], len: usize) -> Self {
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
    remaining: u64,
}

pub type GasRef<'a> = &'a mut Gas;

impl Gas {
    fn new(remaining: u64) -> Self {
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

// -- instruction defs --

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

pub static DEFAULT_TABLE: InstrTable = make_table();
pub static DEFAULT_TAIL_TABLE: TailInstrTable = make_tail_table();

pub static DEFAULT_GAS_TABLE: GasTable = [3; 256];

pub fn new_gas_table(spec: SpecId) -> GasTable {
    let mut t = DEFAULT_GAS_TABLE;
    if spec >= SpecId::Homestead {
        t[op::ADD as usize] = 69;
    }
    t
}

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

// -- instruction impls --

#[doc(hidden)]
#[collapse_debuginfo(yes)]
macro_rules! _count {
    (@count) => { 0 };
    (@count $head:tt $($tail:tt)*) => { 1 + _count!(@count $($tail)*) };
    ($($arg:tt)*) => { _count!(@count $($arg)*) };
}

#[collapse_debuginfo(yes)]
macro_rules! popn_top {
    ([ $($x:ident),* ], $top:ident, $stack:expr) => {
        // this fucking stupid codegen bug
        // https://github.com/rust-lang/rust/issues/144329

        // let ($elems, $top) = $stack.popn_top()?;

        if $stack.len < (1 + _count!($($x)*)) {
            cold_path();
            return Err(InstrErr::StackUnderflow);
        }
        let ([$($x),*], $top) = unsafe { $stack.popn_top().unwrap_unchecked() };
    };
}

fn stop() -> Result {
    cold_path();
    Err(InstrErr::Stop)
}

fn invalid() -> Result {
    cold_path();
    Err(InstrErr::Invalid)
}

fn add(stack: &mut Stack<'_>) -> Result {
    popn_top!([a], b, stack);
    *b = a.wrapping_add(*b);
    Ok(())
}

fn balance(stack: &mut Stack<'_>, state: &mut State) -> Result {
    popn_top!([], addr, stack);
    *addr = state.host.balance(*addr);
    Ok(())
}

fn push<const N: usize>(mut pc: PcRef<'_>, stack: &mut Stack<'_>) -> Result {
    // SAFETY: `PUSH<N>` is always followed by N bytes of data.
    let mut buf = [0u8; _];
    buf[mem::size_of::<Word>() - N..].copy_from_slice(unsafe { pc.read_bytes_unchecked(N) });
    unsafe { pc.advance_unchecked(N) };
    stack.push(Word::from_be_bytes(buf))?;
    Ok(())
}

// -- table --

macro_rules! make_table_inner {
    ([$table:expr, $mk_dispatch:expr] $(
        ($op:ident, $fn:expr),
    )*) => {
        $(
            $table[op::$op as usize] = $mk_dispatch($fn);
        )*
    };
}
macro_rules! make_table_m {
    ($mk_dispatch:expr) => {{
        let mut table = [$mk_dispatch(invalid); 256];
        for_each_opcode!([table, $mk_dispatch] make_table_inner);
        table
    }};
}

pub const fn make_table() -> InstrTable {
    make_table_m!(mk_dispatch)
}

pub const fn mk_dispatch<I: Instruction<T>, T>(f: I) -> InstrFn {
    mem::forget(f);
    dispatch::<I, T>
}

pub const fn make_tail_table() -> TailInstrTable {
    make_table_m!(mk_tail_dispatch)
}

pub const fn mk_tail_dispatch<I: Instruction<T>, T>(f: I) -> TailInstrFn {
    mem::forget(f);
    tail_dispatch::<I, T>
}

extern_table! {
    fn dispatch<I: Instruction<T>, T>(
        pc: PcRef<'_>,
        mut stack: Stack<'_>,
        gas: GasRef<'_>,
        state: &mut State,
    ) -> InstrFnRet {
        let r = I::new().execute(pc, &mut stack, gas, state);
        (stack.len, r)
    }
}

extern_table! {
    fn tail_dispatch<I: Instruction<T>, T>(
        mut pc: Pc<'_>,
        mut stack: Stack<'_>,
        mut gas: Gas,
        state: &mut State,
        gast: &GasTable,
        instrsp: *const (),
    ) -> TailInstrFnRet {
        if let Err(e) = I::new().execute(pc.as_mut(), &mut stack, &mut gas, state) {
            tail_return!(tail_call_restore(pc, stack, gas, state, gast, e as usize as *const ()));
        }
        tail_return!(tail_call_next(pc, stack, gas, state, gast, instrsp));
    }
}

extern_table! {
    #[inline(never)] // TODO: bench inlining this vs having a single dispatcher for all
    fn tail_call_next(
        mut pc: Pc<'_>,
        stack: Stack<'_>,
        mut gas: Gas,
        state: &mut State,
        gast: &GasTable,
        instrsp: *const (),
    ) -> TailInstrFnRet {
        let op = match Interpreter::pre_step(pc.as_mut(), &mut gas, gast) {
            Ok(op) => op,
            Err(e) => {
                tail_return!(tail_call_restore(pc, stack, gas, state, gast, e as usize as *const ()));
            }
        };
        // SAFETY: Restoring type-erased table pointer. See [`TailInstrFn`].
        let instrs = unsafe { &*instrsp.cast::<TailInstrTable>() };
        tail_return!(instrs[op as usize](pc, stack, gas, state, gast, instrsp));
    }
}

extern_table! {
    #[inline(never)]
    #[cold]
    fn tail_call_restore(
        pc: Pc<'_>,
        stack: Stack<'_>,
        gas: Gas,
        state: &mut State,
        _gast: &GasTable,
        ret: *const (), // Tail calls require same function signature, this is unused so we pass the return value here.
    ) -> TailInstrFnRet {
        // SAFETY: `raw_interp` is valid for the duration of execution.
        let interp = unsafe { &mut *state.raw_interp };
        interp.pc = pc.pc;
        interp.gas = gas;
        interp.stack_len = stack.len;
        unsafe { core::mem::transmute::<u8, TailInstrFnRet>(ret as usize as u8) }
    }
}

// -- interpreter --

#[derive(Clone, Copy)]
pub enum Table<'a> {
    Normal(&'a InstrTable),
    Tail(&'a TailInstrTable),
}

pub struct Interpreter {
    bytecode: Vec<u8>,
    pc: usize,
    stack: Box<[u64; 1024]>,
    stack_len: usize,
    gas: Gas,
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
    fn pre_step(mut pc: PcRef<'_>, gas: &mut Gas, gas_table: &GasTable) -> Result<u8> {
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
            for (name, table) in [
                ("normal", Table::Normal(&DEFAULT_TABLE)),
                ("tail", Table::Tail(&DEFAULT_TAIL_TABLE)),
            ] {
                eprintln!("{spec:?}::{name}");
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
