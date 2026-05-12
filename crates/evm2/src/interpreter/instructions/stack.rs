use crate::interpreter::{InstrStop, Word};
use evm2_macros::instruction;

#[instruction]
pub(crate) fn pop([_value]: [Word]) -> Result {}

#[instruction(no_stack_preamble)]
pub(crate) fn push<const N: usize>(cx: _) -> Result {
    if N == 0 {
        return stack.push(Word::ZERO);
    }
    let slice = unsafe { cx.pc.read_bytes_offset_unchecked(1, N) };
    stack.push_slice(slice)
}

#[instruction(no_stack_preamble)]
pub(crate) fn dup<const N: usize>() -> Result {
    stack.dup(N)
}

#[instruction(no_stack_preamble)]
pub(crate) fn swap<const N: usize>() -> Result {
    stack.swap(N)
}

#[instruction(no_stack_preamble)]
pub(crate) fn dupn(cx: _) -> Result {
    let n = decode_single(unsafe { cx.pc.read_bytes_offset_unchecked(1, 1)[0] })
        .ok_or(InstrStop::InvalidImmediateEncoding)?;
    stack.dup(n)
}

#[instruction(no_stack_preamble)]
pub(crate) fn swapn(cx: _) -> Result {
    let n = decode_single(unsafe { cx.pc.read_bytes_offset_unchecked(1, 1)[0] })
        .ok_or(InstrStop::InvalidImmediateEncoding)?;
    stack.exchange(0, n)
}

#[instruction(no_stack_preamble)]
pub(crate) fn exchange(cx: _) -> Result {
    let (n, m) = decode_pair(unsafe { cx.pc.read_bytes_offset_unchecked(1, 1)[0] })
        .ok_or(InstrStop::InvalidImmediateEncoding)?;
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
    use crate::{
        SpecId,
        interpreter::{
            InstrStop, StackMut, Word,
            instructions::tests::{RunConfig, push, run, run_stack},
            opcode,
        },
    };
    use alloc::{vec, vec::Vec};

    #[test]
    fn pop_opcode() {
        let interpreter = run_stack([1], opcode::POP);
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert!(interpreter.stack().is_empty());

        let interpreter = run(RunConfig::new([
            opcode::PUSH1,
            0x01,
            opcode::PUSH1,
            0x02,
            opcode::POP,
            opcode::STOP,
        ]));
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack(), [Word::from(1)]);
    }

    #[test]
    fn stack_underflow() {
        let interpreter = run(RunConfig::new([opcode::POP]));
        assert!(matches!(interpreter.err, InstrStop::StackUnderflow));

        let interpreter = run(RunConfig::new([opcode::DUP1]));
        assert!(matches!(interpreter.err, InstrStop::StackUnderflow));

        let interpreter = run(RunConfig::new([opcode::PUSH0, opcode::SWAP1]));
        assert!(matches!(interpreter.err, InstrStop::StackUnderflow));

        let interpreter = run(RunConfig::new([opcode::DUPN, 0x80]).spec(SpecId::AMSTERDAM));
        assert!(matches!(interpreter.err, InstrStop::StackUnderflow));

        let interpreter =
            run(RunConfig::new([opcode::PUSH0, opcode::SWAPN, 0x80]).spec(SpecId::AMSTERDAM));
        assert!(matches!(interpreter.err, InstrStop::StackUnderflow));

        let interpreter =
            run(RunConfig::new([opcode::PUSH0, opcode::EXCHANGE, 0x8e]).spec(SpecId::AMSTERDAM));
        assert!(matches!(interpreter.err, InstrStop::StackUnderflow));
    }

    #[test]
    fn stack_overflow() {
        let mut code = vec![opcode::PUSH0; StackMut::CAPACITY];
        let interpreter = run(RunConfig::new(code.clone()));
        assert!(matches!(interpreter.err, InstrStop::Stop));
        code.extend([opcode::PUSH0]);
        let interpreter = run(RunConfig::new(code));
        assert!(matches!(interpreter.err, InstrStop::StackOverflow));

        let mut code = vec![opcode::PUSH0; StackMut::CAPACITY];
        code.extend([opcode::PUSH1, 0x00, opcode::STOP]);
        let interpreter = run(RunConfig::new(code));
        assert!(matches!(interpreter.err, InstrStop::StackOverflow));

        let mut code = vec![opcode::PUSH0; StackMut::CAPACITY];
        code.extend([opcode::DUP1, opcode::STOP]);
        let interpreter = run(RunConfig::new(code));
        assert!(matches!(interpreter.err, InstrStop::StackOverflow));
    }

    #[test]
    fn push0_opcode() {
        let interpreter = run(RunConfig::new([opcode::PUSH0, opcode::STOP]));
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack(), [0]);

        let interpreter = run(RunConfig::new([opcode::PUSH0, opcode::PUSH0, opcode::STOP]));
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack(), [0, 0]);
    }

    fn assert_push_opcode(opcode: u8, n: usize) {
        for bytes in [vec![0; n], vec![n as u8; n], (1..=n as u8).collect::<Vec<_>>()] {
            let mut code = Vec::new();
            code.push(opcode);
            code.extend_from_slice(&bytes);
            code.push(opcode::STOP);

            let interpreter = run(RunConfig::new(code));
            assert!(matches!(interpreter.err, InstrStop::Stop));
            assert_eq!(interpreter.stack(), [Word::from_be_slice(&bytes)]);
        }
    }

    macro_rules! push_tests {
        ($($name:ident, $opcode:expr, $n:expr;)*) => {
            $(
                #[test]
                fn $name() {
                    assert_push_opcode($opcode, $n);
                }
            )*
        }
    }

    push_tests! {
        push1_opcode, opcode::PUSH1, 1;
        push2_opcode, opcode::PUSH2, 2;
        push3_opcode, opcode::PUSH3, 3;
        push4_opcode, opcode::PUSH4, 4;
        push5_opcode, opcode::PUSH5, 5;
        push6_opcode, opcode::PUSH6, 6;
        push7_opcode, opcode::PUSH7, 7;
        push8_opcode, opcode::PUSH8, 8;
        push9_opcode, opcode::PUSH9, 9;
        push10_opcode, opcode::PUSH10, 10;
        push11_opcode, opcode::PUSH11, 11;
        push12_opcode, opcode::PUSH12, 12;
        push13_opcode, opcode::PUSH13, 13;
        push14_opcode, opcode::PUSH14, 14;
        push15_opcode, opcode::PUSH15, 15;
        push16_opcode, opcode::PUSH16, 16;
        push17_opcode, opcode::PUSH17, 17;
        push18_opcode, opcode::PUSH18, 18;
        push19_opcode, opcode::PUSH19, 19;
        push20_opcode, opcode::PUSH20, 20;
        push21_opcode, opcode::PUSH21, 21;
        push22_opcode, opcode::PUSH22, 22;
        push23_opcode, opcode::PUSH23, 23;
        push24_opcode, opcode::PUSH24, 24;
        push25_opcode, opcode::PUSH25, 25;
        push26_opcode, opcode::PUSH26, 26;
        push27_opcode, opcode::PUSH27, 27;
        push28_opcode, opcode::PUSH28, 28;
        push29_opcode, opcode::PUSH29, 29;
        push30_opcode, opcode::PUSH30, 30;
        push31_opcode, opcode::PUSH31, 31;
        push32_opcode, opcode::PUSH32, 32;
    }

    fn assert_dup_opcode(opcode: u8, n: usize) {
        for offset in [0, 100, 200] {
            let mut code = Vec::new();
            for value in 1..=16 {
                push(&mut code, Word::from(value + offset));
            }
            code.push(opcode);
            code.push(opcode::STOP);

            let interpreter = run(RunConfig::new(code));
            assert!(matches!(interpreter.err, InstrStop::Stop));
            assert_eq!(interpreter.stack().len(), 17);
            assert_eq!(interpreter.stack()[16], Word::from(17 - n + offset));
        }
    }

    macro_rules! dup_tests {
        ($($name:ident, $opcode:expr, $n:expr;)*) => {
            $(
                #[test]
                fn $name() {
                    assert_dup_opcode($opcode, $n);
                }
            )*
        }
    }

    dup_tests! {
        dup1_opcode, opcode::DUP1, 1;
        dup2_opcode, opcode::DUP2, 2;
        dup3_opcode, opcode::DUP3, 3;
        dup4_opcode, opcode::DUP4, 4;
        dup5_opcode, opcode::DUP5, 5;
        dup6_opcode, opcode::DUP6, 6;
        dup7_opcode, opcode::DUP7, 7;
        dup8_opcode, opcode::DUP8, 8;
        dup9_opcode, opcode::DUP9, 9;
        dup10_opcode, opcode::DUP10, 10;
        dup11_opcode, opcode::DUP11, 11;
        dup12_opcode, opcode::DUP12, 12;
        dup13_opcode, opcode::DUP13, 13;
        dup14_opcode, opcode::DUP14, 14;
        dup15_opcode, opcode::DUP15, 15;
        dup16_opcode, opcode::DUP16, 16;
    }

    fn assert_swap_opcode(opcode: u8, n: usize) {
        for offset in [0, 100, 200] {
            let mut code = Vec::new();
            for value in 1..=17 {
                push(&mut code, Word::from(value + offset));
            }
            code.push(opcode);
            code.push(opcode::STOP);

            let interpreter = run(RunConfig::new(code));
            assert!(matches!(interpreter.err, InstrStop::Stop));
            assert_eq!(interpreter.stack().len(), 17);
            assert_eq!(interpreter.stack()[16], Word::from(17 - n + offset));
            assert_eq!(interpreter.stack()[16 - n], Word::from(17 + offset));
        }
    }

    macro_rules! swap_tests {
        ($($name:ident, $opcode:expr, $n:expr;)*) => {
            $(
                #[test]
                fn $name() {
                    assert_swap_opcode($opcode, $n);
                }
            )*
        }
    }

    swap_tests! {
        swap1_opcode, opcode::SWAP1, 1;
        swap2_opcode, opcode::SWAP2, 2;
        swap3_opcode, opcode::SWAP3, 3;
        swap4_opcode, opcode::SWAP4, 4;
        swap5_opcode, opcode::SWAP5, 5;
        swap6_opcode, opcode::SWAP6, 6;
        swap7_opcode, opcode::SWAP7, 7;
        swap8_opcode, opcode::SWAP8, 8;
        swap9_opcode, opcode::SWAP9, 9;
        swap10_opcode, opcode::SWAP10, 10;
        swap11_opcode, opcode::SWAP11, 11;
        swap12_opcode, opcode::SWAP12, 12;
        swap13_opcode, opcode::SWAP13, 13;
        swap14_opcode, opcode::SWAP14, 14;
        swap15_opcode, opcode::SWAP15, 15;
        swap16_opcode, opcode::SWAP16, 16;
    }

    #[test]
    fn dupn_opcode() {
        let mut code = vec![opcode::PUSH1, 0x01, opcode::PUSH1, 0x00];
        code.extend(core::iter::repeat_n(opcode::DUP1, 15));
        code.extend([opcode::DUPN, 0x80, opcode::STOP]);
        let interpreter = run(RunConfig::new(code).spec(SpecId::AMSTERDAM));
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack().len(), 18);
        assert_eq!(interpreter.stack()[17], Word::from(1));
        assert_eq!(interpreter.stack()[0], Word::from(1));
        for i in 1..17 {
            assert_eq!(interpreter.stack()[i], 0);
        }

        let mut code = Vec::new();
        for value in 0..145 {
            push(&mut code, Word::from(value));
        }
        code.extend([opcode::DUPN, 0xff, opcode::STOP]);
        let interpreter = run(RunConfig::new(code).spec(SpecId::AMSTERDAM));
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack().len(), 146);
        assert_eq!(interpreter.stack()[145], Word::from(1));
    }

    #[test]
    fn swapn_opcode() {
        let mut code = vec![opcode::PUSH1, 0x01, opcode::PUSH1, 0x00];
        code.extend(core::iter::repeat_n(opcode::DUP1, 15));
        code.extend([opcode::PUSH1, 0x02, opcode::SWAPN, 0x80, opcode::STOP]);
        let interpreter = run(RunConfig::new(code).spec(SpecId::AMSTERDAM));
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack().len(), 18);
        assert_eq!(interpreter.stack()[17], Word::from(1));
        assert_eq!(interpreter.stack()[0], Word::from(2));
        for i in 1..17 {
            assert_eq!(interpreter.stack()[i], 0);
        }

        let mut code = Vec::new();
        for value in 0..145 {
            push(&mut code, Word::from(value));
        }
        code.extend([opcode::SWAPN, 0xff, opcode::STOP]);
        let interpreter = run(RunConfig::new(code).spec(SpecId::AMSTERDAM));
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack()[0], Word::from(144));
        assert_eq!(interpreter.stack()[144], 0);
    }

    #[test]
    fn exchange_opcode() {
        let interpreter = run(RunConfig::new([
            opcode::PUSH1,
            0x00,
            opcode::PUSH1,
            0x01,
            opcode::PUSH1,
            0x02,
            opcode::EXCHANGE,
            0x8e,
            opcode::STOP,
        ])
        .spec(SpecId::AMSTERDAM));
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack(), [Word::from(1), Word::from(0), Word::from(2)]);

        let mut code = Vec::new();
        for value in 0..23 {
            push(&mut code, Word::from(value));
        }
        code.extend([opcode::EXCHANGE, 0xff, opcode::STOP]);
        let interpreter = run(RunConfig::new(code).spec(SpecId::AMSTERDAM));
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack()[0], Word::from(21));
        assert_eq!(interpreter.stack()[21], 0);
        assert_eq!(interpreter.stack()[22], Word::from(22));
    }

    #[test]
    fn relative_stack_opcodes_are_not_enabled_before_amsterdam() {
        let interpreter = run(RunConfig::new([opcode::DUPN, 0x80]).spec(SpecId::OSAKA));
        assert!(matches!(interpreter.err, InstrStop::OpcodeNotFound));

        let interpreter = run(RunConfig::new([opcode::SWAPN, 0x80]).spec(SpecId::OSAKA));
        assert!(matches!(interpreter.err, InstrStop::OpcodeNotFound));

        let interpreter = run(RunConfig::new([opcode::EXCHANGE, 0x8e]).spec(SpecId::OSAKA));
        assert!(matches!(interpreter.err, InstrStop::OpcodeNotFound));
    }
}
