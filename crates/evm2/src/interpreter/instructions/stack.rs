use super::super::{CtrlRef, Gas, InstrErr, InstructionCx, Result, Stack, State, Word};
use core::{hint::cold_path, mem};
use evm2_macros::instruction;

#[instruction(raw)]
pub(in crate::interpreter) fn pop() -> Result {
    return stack.pop().map(drop);
}

#[instruction(raw)]
pub(in crate::interpreter) fn push<const N: usize>(cx: _) -> Result {
    let out = stack.push_slot()?;
    let mut buf = [0u8; 32];
    buf[mem::size_of::<Word>() - N..].copy_from_slice(unsafe { cx.ctrl.read_bytes_unchecked(N) });
    unsafe { cx.ctrl.advance_unchecked(N) };
    *out = Word::from_be_bytes(buf);
    return Ok(());
}

#[instruction(raw)]
pub(in crate::interpreter) fn dup<const N: usize>() -> Result {
    if stack.len < N || stack.len == 1024 {
        cold_path();
        return Err(if stack.len == 1024 {
            InstrErr::StackOverflow
        } else {
            InstrErr::StackUnderflow
        });
    }
    let value = unsafe { *stack.stack.get_unchecked(stack.len - N) };
    return stack.push(value);
}

#[instruction(raw)]
pub(in crate::interpreter) fn swap<const N: usize>() -> Result {
    if stack.len <= N {
        cold_path();
        return Err(InstrErr::StackUnderflow);
    }
    stack.stack.swap(stack.len - 1, stack.len - 1 - N);
    return Ok(());
}

#[instruction(raw)]
pub(in crate::interpreter) fn dupn() -> Result {
    let n = decode_single(unsafe { ctrl.read_bytes_unchecked(1)[0] } as usize)
        .ok_or(InstrErr::Invalid)?;
    unsafe { ctrl.advance_unchecked(1) };
    if stack.len <= n || stack.len == 1024 {
        cold_path();
        return Err(if stack.len == 1024 {
            InstrErr::StackOverflow
        } else {
            InstrErr::StackUnderflow
        });
    }
    let value = unsafe { *stack.stack.get_unchecked(stack.len - 1 - n) };
    return stack.push(value);
}

#[instruction(raw)]
pub(in crate::interpreter) fn swapn() -> Result {
    let n = decode_single(unsafe { ctrl.read_bytes_unchecked(1)[0] } as usize)
        .ok_or(InstrErr::Invalid)?;
    unsafe { ctrl.advance_unchecked(1) };
    return swap_n(stack, n + 1);
}

#[instruction(raw)]
pub(in crate::interpreter) fn exchange() -> Result {
    let (n, m) = decode_pair(unsafe { ctrl.read_bytes_unchecked(1)[0] } as usize)
        .ok_or(InstrErr::Invalid)?;
    unsafe { ctrl.advance_unchecked(1) };
    if stack.len <= m {
        cold_path();
        return Err(InstrErr::StackUnderflow);
    }
    stack.stack.swap(stack.len - 1 - n, stack.len - 1 - m);
    return Ok(());
}

#[inline]
fn swap_n(stack: &mut Stack<'_>, n: usize) -> Result {
    if stack.len <= n {
        cold_path();
        return Err(InstrErr::StackUnderflow);
    }
    stack.stack.swap(stack.len - 1, stack.len - 1 - n);
    Ok(())
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
