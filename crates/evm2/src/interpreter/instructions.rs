use core::{hint::cold_path, mem};

use super::{InstrErr, PcRef, Result, Stack, State, Word};

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

pub(super) fn stop() -> Result {
    cold_path();
    Err(InstrErr::Stop)
}

pub(super) fn invalid() -> Result {
    cold_path();
    Err(InstrErr::Invalid)
}

pub(super) fn add(stack: &mut Stack<'_>) -> Result {
    popn_top!([a], b, stack);
    *b = a.wrapping_add(*b);
    Ok(())
}

pub(super) fn balance(stack: &mut Stack<'_>, state: &mut State) -> Result {
    popn_top!([], addr, stack);
    *addr = state.host.balance(*addr);
    Ok(())
}

pub(super) fn push<const N: usize>(mut pc: PcRef<'_>, stack: &mut Stack<'_>) -> Result {
    // SAFETY: `PUSH<N>` is always followed by N bytes of data.
    let mut buf = [0u8; _];
    buf[mem::size_of::<Word>() - N..].copy_from_slice(unsafe { pc.read_bytes_unchecked(N) });
    unsafe { pc.advance_unchecked(N) };
    stack.push(Word::from_be_bytes(buf))?;
    Ok(())
}
