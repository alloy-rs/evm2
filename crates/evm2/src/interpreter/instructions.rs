use evm2_macros::instruction;

use super::{GasRef, Host, InstrErr, InstrFnRet, PcRef, Result, Stack, State, Word};
use core::{hint::cold_path, mem};

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

#[instruction(raw)]
pub(super) fn stop() -> Result {
    cold_path();
    return Err(InstrErr::Stop);
}

#[instruction(raw)]
pub(super) fn invalid() -> Result {
    cold_path();
    return Err(InstrErr::Invalid);
}

#[instruction]
pub(super) fn add(a: &Word) -> Result<b> {
    *b = a.wrapping_add(*b);
}

#[instruction]
pub(super) fn balance(cx: _) -> Result<addr> {
    let address = *addr;
    *addr = cx.host.balance(address);
}

#[instruction(raw)]
pub(super) fn push<const N: usize>(cx: _) -> Result {
    // SAFETY: `PUSH<N>` is always followed by N bytes of data.
    let mut buf = [0u8; 32];
    buf[mem::size_of::<Word>() - N..].copy_from_slice(unsafe { cx.pc.read_bytes_unchecked(N) });
    unsafe { cx.pc.advance_unchecked(N) };
    cx.stack.push(Word::from_be_bytes(buf))?;
}
