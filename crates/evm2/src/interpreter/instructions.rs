use super::{InstrErr, InstructionCx, Result, Word};
use core::{hint::cold_path, mem};
use evm2_macros::instruction;

#[instruction]
pub(super) fn stop() -> Result {
    cold_path();
    return Err(InstrErr::Stop);
}

#[instruction]
pub(super) fn invalid() -> Result {
    cold_path();
    return Err(InstrErr::Invalid);
}

#[instruction]
pub(super) fn add(a: &Word, b: &Word) -> Result<out> {
    *out = a.wrapping_add(*b);
}

#[instruction]
pub(super) fn balance(cx: _, addr: &Word) -> Result<out> {
    *out = cx.host.balance(*addr);
}

#[instruction]
pub(super) fn push<const N: usize>(cx: _) -> Result<out> {
    // SAFETY: `PUSH<N>` is always followed by N bytes of data.
    let mut buf = [0u8; 32];
    buf[mem::size_of::<Word>() - N..].copy_from_slice(unsafe { cx.ctrl.read_bytes_unchecked(N) });
    unsafe { cx.ctrl.advance_unchecked(N) };
    *out = Word::from_be_bytes(buf);
}
