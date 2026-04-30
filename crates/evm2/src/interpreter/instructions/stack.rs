use super::super::{CtrlRef, Gas, InstrErr, InstructionCx, Result, Stack, State, Word};
use evm2_macros::instruction;

#[instruction]
pub(in crate::interpreter) fn pop(_value: &Word) -> Result {
    Ok(())
}

#[instruction(raw)]
pub(in crate::interpreter) fn push<const N: usize>(cx: _) -> Result {
    if N == 0 {
        return stack.push(Word::ZERO);
    }
    let slice = unsafe { cx.ctrl.read_bytes_unchecked(N) };
    stack.push_slice(slice)?;
    unsafe { cx.ctrl.advance_unchecked(N) };
    Ok(())
}

#[instruction(raw)]
pub(in crate::interpreter) fn dup<const N: usize>() -> Result {
    stack.dup(N)
}

#[instruction(raw)]
pub(in crate::interpreter) fn swap<const N: usize>() -> Result {
    stack.swap(N)
}

#[instruction(raw)]
pub(in crate::interpreter) fn dupn() -> Result {
    let n = decode_single(unsafe { ctrl.read_bytes_unchecked(1)[0] } as usize)
        .ok_or(InstrErr::Invalid)?;
    unsafe { ctrl.advance_unchecked(1) };
    stack.dup(n + 1)
}

#[instruction(raw)]
pub(in crate::interpreter) fn swapn() -> Result {
    let n = decode_single(unsafe { ctrl.read_bytes_unchecked(1)[0] } as usize)
        .ok_or(InstrErr::Invalid)?;
    unsafe { ctrl.advance_unchecked(1) };
    stack.exchange(0, n + 1)
}

#[instruction(raw)]
pub(in crate::interpreter) fn exchange() -> Result {
    let (n, m) = decode_pair(unsafe { ctrl.read_bytes_unchecked(1)[0] } as usize)
        .ok_or(InstrErr::Invalid)?;
    unsafe { ctrl.advance_unchecked(1) };
    stack.exchange(n, m - n)
}

const fn decode_single(x: usize) -> Option<usize> {
    if x <= 90 || x >= 128 { Some((x + 145) % 256) } else { None }
}

const fn decode_pair(x: usize) -> Option<(usize, usize)> {
    if x > 81 && x < 128 {
        return None;
    }
    let k = x ^ 143;
    let q = k / 16;
    let r = k % 16;
    if q < r { Some((q + 1, r + 1)) } else { Some((r + 1, 29 - q)) }
}
