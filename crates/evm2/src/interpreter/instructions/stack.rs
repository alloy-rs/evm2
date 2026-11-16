use crate::interpreter::InstrStop;
use evm2_macros::instruction;

#[instruction]
pub fn pop([_value]: [Word]) -> Result {}

#[instruction(no_stack_preamble)]
pub fn push<const N: usize>(cx: _) -> Result {
    if N == 0 {
        return stack.push_u64(0);
    }
    let slice = unsafe { cx.pc.read_bytes_offset_unchecked(1, N) };
    stack.push_slice(slice)
}

#[instruction(no_stack_preamble)]
pub fn dup<const N: usize>() -> Result {
    stack.dup(N)
}

#[instruction(no_stack_preamble)]
pub fn swap<const N: usize>() -> Result {
    stack.swap(N)
}

#[instruction(no_stack_preamble)]
pub fn dupn(cx: _) -> Result {
    let n = decode_single(unsafe { cx.pc.read_bytes_offset_unchecked(1, 1)[0] })
        .ok_or(InstrStop::InvalidImmediateEncoding)?;
    stack.dup(n as usize)
}

#[instruction(no_stack_preamble)]
pub fn swapn(cx: _) -> Result {
    let n = decode_single(unsafe { cx.pc.read_bytes_offset_unchecked(1, 1)[0] })
        .ok_or(InstrStop::InvalidImmediateEncoding)?;
    stack.swap(n as usize)
}

#[instruction(no_stack_preamble)]
pub fn exchange(cx: _) -> Result {
    let (n, m) = decode_pair(unsafe { cx.pc.read_bytes_offset_unchecked(1, 1)[0] })
        .ok_or(InstrStop::InvalidImmediateEncoding)?;
    stack.exchange(n as usize, m as usize)
}

/// Decodes a `DUPN`/`SWAPN` immediate byte into a stack index.
///
/// Returns `None` if the immediate is in the invalid range `[91, 127]`.
pub const fn decode_single(x: u8) -> Option<u8> {
    if x <= 90 || x >= 128 { Some(x.wrapping_add(145)) } else { None }
}

/// Decodes an `EXCHANGE` immediate byte into a pair of stack indices `(n, m)`.
///
/// Returns `None` if the immediate is in the invalid range `[82, 127]`.
pub const fn decode_pair(x: u8) -> Option<(u8, u8)> {
    if x > 81 && x < 128 {
        return None;
    }
    let k = (x ^ 143) as u16;
    let q = (k / 16) as u8;
    let r = (k % 16) as u8;
    if q < r { Some((q + 1, r + 1)) } else { Some((r + 1, 29 - q)) }
}

/// Encodes a stack index into a DUPN/SWAPN immediate byte.
///
/// Returns `None` if the value is outside the valid range.
pub fn encode_single(n: u8) -> Option<u8> {
    let x = n.wrapping_sub(145);
    if decode_single(x) == Some(n) { Some(x) } else { None }
}

/// Encodes a pair of stack indices into an EXCHANGE immediate byte.
///
/// Returns `None` if the pair cannot be encoded.
pub fn encode_pair(n: u8, m: u8) -> Option<u8> {
    if n == 0 || m == 0 {
        return None;
    }
    // Try both (q,r) orderings and check round-trip.
    let try_k = |q: u8, r: u8| -> Option<u8> {
        if q >= 16 || r >= 16 {
            return None;
        }
        let k = q as u16 * 16 + r as u16;
        let x = (k as u8) ^ 143;
        if decode_pair(x) == Some((n, m)) { Some(x) } else { None }
    };

    // Case 1: q < r → n = q+1, m = r+1.
    if n < m {
        try_k(n - 1, m - 1)
    } else {
        // Case 2: q >= r → n = r+1, m = 29-q.
        29u8.checked_sub(m).and_then(|q| try_k(q, n - 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        SpecId,
        interpreter::{InstrStop, StackMut, Word, op},
        test_utils::{RunConfig, push, run, run_stack},
    };
    use alloc::{vec, vec::Vec};
    use core::assert_matches;
    use test_case::test_case;

    #[test]
    fn pop_opcode() {
        let interp = run_stack([1], op::POP);
        assert_matches!(interp.err, InstrStop::Stop);
        assert!(interp.stack().is_empty());

        let interp = run(RunConfig::new([op::PUSH1, 0x01, op::PUSH1, 0x02, op::POP, op::STOP]));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [Word::ONE]);
    }

    #[test]
    fn stack_underflow() {
        let interp = run(RunConfig::new([op::POP]));
        assert_matches!(interp.err, InstrStop::StackUnderflow);

        let interp = run(RunConfig::new([op::DUP1]));
        assert_matches!(interp.err, InstrStop::StackUnderflow);

        let interp = run(RunConfig::new([op::PUSH0, op::SWAP1]));
        assert_matches!(interp.err, InstrStop::StackUnderflow);

        let interp = run(RunConfig::new([op::DUPN, 0x80]).spec(SpecId::AMSTERDAM));
        assert_matches!(interp.err, InstrStop::StackUnderflow);

        let interp = run(RunConfig::new([op::PUSH0, op::SWAPN, 0x80]).spec(SpecId::AMSTERDAM));
        assert_matches!(interp.err, InstrStop::StackUnderflow);

        let interp = run(RunConfig::new([op::PUSH0, op::EXCHANGE, 0x8e]).spec(SpecId::AMSTERDAM));
        assert_matches!(interp.err, InstrStop::StackUnderflow);
    }

    #[test]
    fn stack_overflow() {
        let mut code = vec![op::PUSH0; StackMut::CAPACITY];
        let interp = run(RunConfig::new(code.clone()));
        assert_matches!(interp.err, InstrStop::Stop);
        code.extend([op::PUSH0]);
        let interp = run(RunConfig::new(code));
        assert_matches!(interp.err, InstrStop::StackOverflow);

        let mut code = vec![op::PUSH0; StackMut::CAPACITY];
        code.extend([op::PUSH1, 0x00, op::STOP]);
        let interp = run(RunConfig::new(code));
        assert_matches!(interp.err, InstrStop::StackOverflow);

        let mut code = vec![op::PUSH0; StackMut::CAPACITY];
        code.extend([op::DUP1, op::STOP]);
        let interp = run(RunConfig::new(code));
        assert_matches!(interp.err, InstrStop::StackOverflow);
    }

    #[test]
    fn push0_opcode() {
        let interp = run(RunConfig::new([op::PUSH0, op::STOP]));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [0]);

        let interp = run(RunConfig::new([op::PUSH0, op::PUSH0, op::STOP]));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [0, 0]);
    }

    #[test_case(op::PUSH1, 1; "push1_opcode")]
    #[test_case(op::PUSH2, 2; "push2_opcode")]
    #[test_case(op::PUSH3, 3; "push3_opcode")]
    #[test_case(op::PUSH4, 4; "push4_opcode")]
    #[test_case(op::PUSH5, 5; "push5_opcode")]
    #[test_case(op::PUSH6, 6; "push6_opcode")]
    #[test_case(op::PUSH7, 7; "push7_opcode")]
    #[test_case(op::PUSH8, 8; "push8_opcode")]
    #[test_case(op::PUSH9, 9; "push9_opcode")]
    #[test_case(op::PUSH10, 10; "push10_opcode")]
    #[test_case(op::PUSH11, 11; "push11_opcode")]
    #[test_case(op::PUSH12, 12; "push12_opcode")]
    #[test_case(op::PUSH13, 13; "push13_opcode")]
    #[test_case(op::PUSH14, 14; "push14_opcode")]
    #[test_case(op::PUSH15, 15; "push15_opcode")]
    #[test_case(op::PUSH16, 16; "push16_opcode")]
    #[test_case(op::PUSH17, 17; "push17_opcode")]
    #[test_case(op::PUSH18, 18; "push18_opcode")]
    #[test_case(op::PUSH19, 19; "push19_opcode")]
    #[test_case(op::PUSH20, 20; "push20_opcode")]
    #[test_case(op::PUSH21, 21; "push21_opcode")]
    #[test_case(op::PUSH22, 22; "push22_opcode")]
    #[test_case(op::PUSH23, 23; "push23_opcode")]
    #[test_case(op::PUSH24, 24; "push24_opcode")]
    #[test_case(op::PUSH25, 25; "push25_opcode")]
    #[test_case(op::PUSH26, 26; "push26_opcode")]
    #[test_case(op::PUSH27, 27; "push27_opcode")]
    #[test_case(op::PUSH28, 28; "push28_opcode")]
    #[test_case(op::PUSH29, 29; "push29_opcode")]
    #[test_case(op::PUSH30, 30; "push30_opcode")]
    #[test_case(op::PUSH31, 31; "push31_opcode")]
    #[test_case(op::PUSH32, 32; "push32_opcode")]
    fn push_opcode(opcode: u8, n: usize) {
        for bytes in [vec![0; n], vec![n as u8; n], (1..=n as u8).collect::<Vec<_>>()] {
            let code = [&[opcode][..], &bytes[..], &[op::STOP][..]].concat();

            let interp = run(RunConfig::new(code));
            assert_matches!(interp.err, InstrStop::Stop);
            assert_eq!(interp.stack(), [Word::from_be_slice(&bytes)]);
        }
    }

    #[test_case(op::DUP1, 1; "dup1_opcode")]
    #[test_case(op::DUP2, 2; "dup2_opcode")]
    #[test_case(op::DUP3, 3; "dup3_opcode")]
    #[test_case(op::DUP4, 4; "dup4_opcode")]
    #[test_case(op::DUP5, 5; "dup5_opcode")]
    #[test_case(op::DUP6, 6; "dup6_opcode")]
    #[test_case(op::DUP7, 7; "dup7_opcode")]
    #[test_case(op::DUP8, 8; "dup8_opcode")]
    #[test_case(op::DUP9, 9; "dup9_opcode")]
    #[test_case(op::DUP10, 10; "dup10_opcode")]
    #[test_case(op::DUP11, 11; "dup11_opcode")]
    #[test_case(op::DUP12, 12; "dup12_opcode")]
    #[test_case(op::DUP13, 13; "dup13_opcode")]
    #[test_case(op::DUP14, 14; "dup14_opcode")]
    #[test_case(op::DUP15, 15; "dup15_opcode")]
    #[test_case(op::DUP16, 16; "dup16_opcode")]
    fn dup_opcode(opcode: u8, n: usize) {
        for offset in [0, 100, 200] {
            let mut code = Vec::new();
            for value in 1..=16 {
                push(&mut code, Word::from(value + offset));
            }
            code.push(opcode);
            code.push(op::STOP);

            let interp = run(RunConfig::new(code));
            assert_matches!(interp.err, InstrStop::Stop);
            assert_eq!(interp.stack().len(), 17);
            assert_eq!(interp.stack()[16], Word::from(17 - n + offset));
        }
    }

    #[test_case(op::SWAP1, 1; "swap1_opcode")]
    #[test_case(op::SWAP2, 2; "swap2_opcode")]
    #[test_case(op::SWAP3, 3; "swap3_opcode")]
    #[test_case(op::SWAP4, 4; "swap4_opcode")]
    #[test_case(op::SWAP5, 5; "swap5_opcode")]
    #[test_case(op::SWAP6, 6; "swap6_opcode")]
    #[test_case(op::SWAP7, 7; "swap7_opcode")]
    #[test_case(op::SWAP8, 8; "swap8_opcode")]
    #[test_case(op::SWAP9, 9; "swap9_opcode")]
    #[test_case(op::SWAP10, 10; "swap10_opcode")]
    #[test_case(op::SWAP11, 11; "swap11_opcode")]
    #[test_case(op::SWAP12, 12; "swap12_opcode")]
    #[test_case(op::SWAP13, 13; "swap13_opcode")]
    #[test_case(op::SWAP14, 14; "swap14_opcode")]
    #[test_case(op::SWAP15, 15; "swap15_opcode")]
    #[test_case(op::SWAP16, 16; "swap16_opcode")]
    fn swap_opcode(opcode: u8, n: usize) {
        for offset in [0, 100, 200] {
            let mut code = Vec::new();
            for value in 1..=17 {
                push(&mut code, Word::from(value + offset));
            }
            code.push(opcode);
            code.push(op::STOP);

            let interp = run(RunConfig::new(code));
            assert_matches!(interp.err, InstrStop::Stop);
            assert_eq!(interp.stack().len(), 17);
            assert_eq!(interp.stack()[16], Word::from(17 - n + offset));
            assert_eq!(interp.stack()[16 - n], Word::from(17 + offset));
        }
    }

    #[test]
    fn dupn_opcode() {
        let mut code = vec![op::PUSH1, 0x01, op::PUSH1, 0x00];
        code.extend(core::iter::repeat_n(op::DUP1, 15));
        code.extend([op::DUPN, 0x80, op::STOP]);
        let interp = run(RunConfig::new(code).spec(SpecId::AMSTERDAM));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack().len(), 18);
        assert_eq!(interp.stack()[17], Word::ONE);
        assert_eq!(interp.stack()[0], Word::ONE);
        for i in 1..17 {
            assert_eq!(interp.stack()[i], 0);
        }

        let mut code = Vec::new();
        for value in 0..145 {
            push(&mut code, Word::from(value));
        }
        code.extend([op::DUPN, 0xff, op::STOP]);
        let interp = run(RunConfig::new(code).spec(SpecId::AMSTERDAM));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack().len(), 146);
        assert_eq!(interp.stack()[145], Word::ONE);
    }

    #[test]
    fn swapn_opcode() {
        let mut code = vec![op::PUSH1, 0x01, op::PUSH1, 0x00];
        code.extend(core::iter::repeat_n(op::DUP1, 15));
        code.extend([op::PUSH1, 0x02, op::SWAPN, 0x80, op::STOP]);
        let interp = run(RunConfig::new(code).spec(SpecId::AMSTERDAM));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack().len(), 18);
        assert_eq!(interp.stack()[17], Word::ONE);
        assert_eq!(interp.stack()[0], Word::from(2));
        for i in 1..17 {
            assert_eq!(interp.stack()[i], 0);
        }

        let mut code = Vec::new();
        for value in 0..145 {
            push(&mut code, Word::from(value));
        }
        code.extend([op::SWAPN, 0xff, op::STOP]);
        let interp = run(RunConfig::new(code).spec(SpecId::AMSTERDAM));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack()[0], Word::from(144));
        assert_eq!(interp.stack()[144], 0);
    }

    #[test]
    fn exchange_opcode() {
        let interp = run(RunConfig::new([
            op::PUSH1,
            0x00,
            op::PUSH1,
            0x01,
            op::PUSH1,
            0x02,
            op::EXCHANGE,
            0x8e,
            op::STOP,
        ])
        .spec(SpecId::AMSTERDAM));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [Word::ONE, Word::ZERO, Word::from(2)]);

        let mut code = Vec::new();
        for value in 0..23 {
            push(&mut code, Word::from(value));
        }
        code.extend([op::EXCHANGE, 0xff, op::STOP]);
        let interp = run(RunConfig::new(code).spec(SpecId::AMSTERDAM));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack()[0], Word::from(21));
        assert_eq!(interp.stack()[21], 0);
        assert_eq!(interp.stack()[22], Word::from(22));
    }

    #[test]
    fn relative_stack_opcodes_are_not_enabled_before_amsterdam() {
        let interp = run(RunConfig::new([op::DUPN, 0x80]).spec(SpecId::OSAKA));
        assert_matches!(interp.err, InstrStop::NotActivated);

        let interp = run(RunConfig::new([op::SWAPN, 0x80]).spec(SpecId::OSAKA));
        assert_matches!(interp.err, InstrStop::NotActivated);

        let interp = run(RunConfig::new([op::EXCHANGE, 0x8e]).spec(SpecId::OSAKA));
        assert_matches!(interp.err, InstrStop::NotActivated);
    }

    #[test]
    fn encode_decode_single_roundtrip() {
        for x in 0..=255u8 {
            if let Some(n) = decode_single(x) {
                assert_eq!(encode_single(n), Some(x), "roundtrip failed for x={x}, n={n}");
            }
        }
        for n in 0..=255u8 {
            if let Some(x) = encode_single(n) {
                assert_eq!(decode_single(x), Some(n), "roundtrip failed for n={n}, x={x}");
            }
        }
        // Out-of-range: values in [236,255] ∪ [0,16] have no valid encoding.
        assert_eq!(encode_single(0), None);
        assert_eq!(encode_single(16), None);
        assert_eq!(encode_single(236), None);
        // Invalid raw bytes.
        assert_eq!(decode_single(91), None);
        assert_eq!(decode_single(127), None);
    }

    #[test]
    fn encode_decode_pair_roundtrip() {
        // encode → decode is always exact.
        for n in 1..=30u8 {
            for m in 1..=30u8 {
                if let Some(x) = encode_pair(n, m) {
                    assert_eq!(
                        decode_pair(x),
                        Some((n, m)),
                        "roundtrip failed for (n,m)=({n},{m}), x={x}"
                    );
                }
            }
        }
        // decode → encode: some pairs have multiple encodings, so the raw byte may differ,
        // but decoding the result must produce the same pair.
        // Some decoded pairs may have values too large to re-encode.
        for x in 0..=255u8 {
            if let Some((n, m)) = decode_pair(x)
                && let Some(re) = encode_pair(n, m)
            {
                assert_eq!(
                    decode_pair(re),
                    Some((n, m)),
                    "decode roundtrip failed for x={x}, re={re}, (n,m)=({n},{m})"
                );
            }
        }
        // Zero indices are invalid.
        assert_eq!(encode_pair(0, 1), None);
        assert_eq!(encode_pair(1, 0), None);
        // Invalid raw bytes: [82, 127].
        assert_eq!(decode_pair(82), None);
        assert_eq!(decode_pair(127), None);
        // Edge: 80 and 81 are now valid.
        assert!(decode_pair(80).is_some());
        assert!(decode_pair(81).is_some());
    }
}
