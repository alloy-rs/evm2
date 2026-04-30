use crate::interpreter::{CtrlRef, Gas, InstrErr, InstructionCx, Result, Stack, State, Word};
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
pub(in crate::interpreter) fn dupn(cx: _) -> Result {
    let n =
        decode_single(unsafe { cx.ctrl.read_bytes_unchecked(1)[0] }).ok_or(InstrErr::Invalid)?;
    unsafe { cx.ctrl.advance_unchecked(1) };
    stack.dup(n)
}

#[instruction(raw)]
pub(in crate::interpreter) fn swapn(cx: _) -> Result {
    let n =
        decode_single(unsafe { cx.ctrl.read_bytes_unchecked(1)[0] }).ok_or(InstrErr::Invalid)?;
    unsafe { cx.ctrl.advance_unchecked(1) };
    stack.exchange(0, n)
}

#[instruction(raw)]
pub(in crate::interpreter) fn exchange(cx: _) -> Result {
    let (n, m) =
        decode_pair(unsafe { cx.ctrl.read_bytes_unchecked(1)[0] }).ok_or(InstrErr::Invalid)?;
    unsafe { cx.ctrl.advance_unchecked(1) };
    stack.exchange(n, m)
}

const fn decode_single(x: u8) -> Option<usize> {
    if x <= 90 || x >= 128 { Some(x.wrapping_add(145) as usize) } else { None }
}

const fn decode_pair(x: u8) -> Option<(usize, usize)> {
    if x > 81 && x < 128 {
        return None;
    }
    let k = (x ^ 143) as usize;
    let q = k / 16;
    let r = k % 16;
    if q < r { Some((q + 1, r + 1)) } else { Some((r + 1, 29 - q)) }
}

#[cfg(test)]
mod tests {
    use crate::interpreter::{
        InstrErr, Word,
        instructions::tests::{push, run, run_stack},
        op,
    };
    use alloc::vec::Vec;

    #[test]
    fn pop_opcode() {
        let interpreter = run_stack(&[Word::from(1)], op::POP);
        assert!(matches!(interpreter.err, InstrErr::Stop));
        assert!(interpreter.stack().is_empty());
    }

    #[test]
    fn push_opcodes() {
        let mut code = Vec::new();
        code.push(op::PUSH0);
        for n in 1..=32 {
            code.push(op::PUSH1 + n - 1);
            code.extend(core::iter::repeat_n(n, n as usize));
        }
        code.push(op::STOP);

        let interpreter = run(code);
        assert!(matches!(interpreter.err, InstrErr::Stop));
        assert_eq!(interpreter.stack().len(), 33);
        assert_eq!(interpreter.stack()[0], Word::ZERO);

        for n in 1..=32 {
            let mut bytes = [0u8; 32];
            bytes[32 - n as usize..].fill(n);
            assert_eq!(interpreter.stack()[n as usize], Word::from_be_bytes(bytes));
        }
    }

    #[test]
    fn dup_opcodes() {
        for n in 1..=16 {
            let mut code = Vec::new();
            for value in 1..=16 {
                push(&mut code, Word::from(value));
            }
            code.push(op::DUP1 + n - 1);
            code.push(op::STOP);

            let interpreter = run(code);
            assert!(matches!(interpreter.err, InstrErr::Stop));
            assert_eq!(interpreter.stack().len(), 17);
            assert_eq!(interpreter.stack()[16], Word::from(17 - n));
        }
    }

    #[test]
    fn swap_opcodes() {
        for n in 1..=16 {
            let mut code = Vec::new();
            for value in 1..=17 {
                push(&mut code, Word::from(value));
            }
            code.push(op::SWAP1 + n - 1);
            code.push(op::STOP);

            let interpreter = run(code);
            assert!(matches!(interpreter.err, InstrErr::Stop));
            assert_eq!(interpreter.stack().len(), 17);
            assert_eq!(interpreter.stack()[16], Word::from(17 - n));
            assert_eq!(interpreter.stack()[16 - n as usize], Word::from(17));
        }
    }

    #[test]
    fn eof_stack_opcodes() {
        let mut code = vec![op::PUSH1, 0x01, op::PUSH1, 0x00];
        code.extend(core::iter::repeat_n(op::DUP1, 15));
        code.extend([op::DUPN, 0x80, op::STOP]);
        let interpreter = run(code);
        assert!(matches!(interpreter.err, InstrErr::Stop));
        assert_eq!(interpreter.stack().len(), 18);
        assert_eq!(interpreter.stack()[17], Word::from(1));
        assert_eq!(interpreter.stack()[0], Word::from(1));
        for i in 1..17 {
            assert_eq!(interpreter.stack()[i], Word::ZERO);
        }

        let mut code = vec![op::PUSH1, 0x01, op::PUSH1, 0x00];
        code.extend(core::iter::repeat_n(op::DUP1, 15));
        code.extend([op::PUSH1, 0x02, op::SWAPN, 0x80, op::STOP]);
        let interpreter = run(code);
        assert!(matches!(interpreter.err, InstrErr::Stop));
        assert_eq!(interpreter.stack().len(), 18);
        assert_eq!(interpreter.stack()[17], Word::from(1));
        assert_eq!(interpreter.stack()[0], Word::from(2));
        for i in 1..17 {
            assert_eq!(interpreter.stack()[i], Word::ZERO);
        }

        let interpreter =
            run([op::PUSH1, 0x00, op::PUSH1, 0x01, op::PUSH1, 0x02, op::EXCHANGE, 0x8e, op::STOP]);
        assert!(matches!(interpreter.err, InstrErr::Stop));
        assert_eq!(interpreter.stack(), [Word::from(1), Word::ZERO, Word::from(2)]);
    }
}
