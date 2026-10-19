use super::i256::i256_lt;
use crate::{interpreter::Word, utils::WordExt};
use core::hint::select_unpredictable;
use evm2_macros::instruction;

#[instruction]
pub fn lt([a, b]: [Word]) -> out {
    out.write_u64((a < b) as u64);
}

#[instruction]
pub fn gt([a, b]: [Word]) -> out {
    out.write_u64((a > b) as u64);
}

#[instruction]
pub fn slt([a, b]: [Word]) -> out {
    out.write_u64(i256_lt(a, b) as u64);
}

#[instruction]
pub fn sgt([a, b]: [Word]) -> out {
    out.write_u64(i256_lt(b, a) as u64);
}

#[instruction]
pub fn eq([a, b]: [Word]) -> out {
    out.write_u64((a == b) as u64);
}

#[instruction]
pub fn iszero([value]: [Word]) -> out {
    out.write_u64(value.is_zero() as u64);
}

#[instruction]
pub fn bitand([a, b]: [Word]) -> out {
    *out = *a & *b;
}

#[instruction]
pub fn bitor([a, b]: [Word]) -> out {
    *out = *a | *b;
}

#[instruction]
pub fn bitxor([a, b]: [Word]) -> out {
    *out = *a ^ *b;
}

#[instruction]
pub fn not([value]: [Word]) -> out {
    *out = !*value;
}

#[instruction]
pub fn byte([index, value]: [Word]) -> out {
    let index = index.to_usize_saturated();
    out.write_u64(if index < 32 { value.byte(31 - index).into() } else { 0 });
}

#[instruction]
pub fn shl([shift, value]: [Word]) -> out {
    out.write(shl_word(value, shift.to_usize_saturated()));
}

#[instruction]
pub fn shr([shift, value]: [Word]) -> out {
    out.write(shr_word(value, shift.to_usize_saturated(), 0));
}

#[instruction]
pub fn sar([shift, value]: [Word]) -> out {
    let fill = ((value.as_limbs()[3] as i64) >> 63) as u64;
    out.write(shr_word(value, shift.to_usize_saturated(), fill));
}

#[instruction]
pub fn clz([value]: [Word]) -> out {
    out.write_u64(value.leading_zeros() as u64);
}

/// Shifts `value` left by `shift` bits, returning zero for shifts of 256 or more.
#[inline(always)]
fn shl_word(value: &Word, shift: usize) -> Word {
    if shift >= 256 {
        return Word::ZERO;
    }
    let [l0, l1, l2, l3] = *value.as_limbs();
    let limbs = shift / 64;
    let bits = (shift % 64) as u32;
    // Move whole limbs with two selects per limb instead of indexing memory.
    let [l0, l1, l2, l3] = select_unpredictable(limbs & 1 != 0, [0, l0, l1, l2], [l0, l1, l2, l3]);
    let [l0, l1, l2, l3] = select_unpredictable(limbs & 2 != 0, [0, 0, l0, l1], [l0, l1, l2, l3]);
    let carry = |lo: u64| (lo >> 1) >> (63 - bits);
    Word::from_limbs([
        l0 << bits,
        (l1 << bits) | carry(l0),
        (l2 << bits) | carry(l1),
        (l3 << bits) | carry(l2),
    ])
}

/// Shifts `value` right by `shift` bits, shifting in `fill` (all zeros or all ones) from the top.
#[inline(always)]
fn shr_word(value: &Word, shift: usize, fill: u64) -> Word {
    if shift >= 256 {
        return Word::from_limbs([fill; 4]);
    }
    let [l0, l1, l2, l3] = *value.as_limbs();
    let limbs = shift / 64;
    let bits = (shift % 64) as u32;
    let [l0, l1, l2, l3] =
        select_unpredictable(limbs & 1 != 0, [l1, l2, l3, fill], [l0, l1, l2, l3]);
    let [l0, l1, l2, l3] =
        select_unpredictable(limbs & 2 != 0, [l2, l3, fill, fill], [l0, l1, l2, l3]);
    let carry = |hi: u64| (hi << 1) << (63 - bits);
    Word::from_limbs([
        (l0 >> bits) | carry(l1),
        (l1 >> bits) | carry(l2),
        (l2 >> bits) | carry(l3),
        (l3 >> bits) | carry(fill),
    ])
}

#[cfg(test)]
mod tests {
    use crate::{
        interpreter::Word,
        test_utils::{assert_stack, neg},
    };

    #[test]
    fn lt_opcode() {
        assert_stack!(LT(1, 2), 1);
        assert_stack!(LT(2, 1), 0);
        assert_stack!(LT(2, 2), 0);
    }

    #[test]
    fn gt_opcode() {
        assert_stack!(GT(2, 1), 1);
        assert_stack!(GT(1, 2), 0);
        assert_stack!(GT(2, 2), 0);
    }

    #[test]
    fn slt_opcode() {
        assert_stack!(SLT(neg(1), 0), 1);
        assert_stack!(SLT(0, neg(1)), 0);
        assert_stack!(SLT(neg(2), neg(1)), 1);
        assert_stack!(SLT(neg(1), neg(1)), 0);
        assert_stack!(SLT(!(Word::MAX >> 1usize), Word::MAX >> 1usize), 1);
    }

    #[test]
    fn sgt_opcode() {
        assert_stack!(SGT(0, neg(1)), 1);
        assert_stack!(SGT(neg(1), 0), 0);
        assert_stack!(SGT(neg(1), neg(2)), 1);
        assert_stack!(SGT(neg(1), neg(1)), 0);
        assert_stack!(SGT(Word::MAX >> 1usize, !(Word::MAX >> 1usize)), 1);
    }

    #[test]
    fn eq_opcode() {
        assert_stack!(EQ(3, 3), 1);
        assert_stack!(EQ(3, 4), 0);
        assert_stack!(EQ(neg(1), neg(1)), 1);
    }

    #[test]
    fn iszero_opcode() {
        assert_stack!(ISZERO(0), 1);
        assert_stack!(ISZERO(1), 0);
        assert_stack!(ISZERO(neg(1)), 0);
    }

    #[test]
    fn and_opcode() {
        assert_stack!(AND(0b1100, 0b1010), 0b1000);
        assert_stack!(AND(neg(1), 0x55), 0x55);
        assert_stack!(AND(0, neg(1)), 0);
    }

    #[test]
    fn or_opcode() {
        assert_stack!(OR(0b1100, 0b1010), 0b1110);
        assert_stack!(OR(0, 0x55), 0x55);
        assert_stack!(OR(neg(1), 0), neg(1));
    }

    #[test]
    fn xor_opcode() {
        assert_stack!(XOR(0b1100, 0b1010), 0b0110);
        assert_stack!(XOR(neg(1), neg(1)), 0);
        assert_stack!(XOR(0, 0x55), 0x55);
    }

    #[test]
    fn not_opcode() {
        assert_stack!(NOT(0), neg(1));
        assert_stack!(NOT(neg(1)), 0);
        assert_stack!(NOT(0xff), neg(1) - Word::from(0xff));
    }

    #[test]
    fn byte_opcode() {
        assert_stack!(BYTE(31, 0x1234), 0x34);
        assert_stack!(BYTE(30, 0x1234), 0x12);
        assert_stack!(BYTE(32, 0x1234), 0);
    }

    const POSITIVE: Word = Word::from_limbs([
        0x0123_4567_89ab_cdef,
        0xfedc_ba98_7654_3210,
        0x0f1e_2d3c_4b5a_6978,
        0x7899_aabb_ccdd_eeff,
    ]);
    const NEGATIVE: Word = Word::from_limbs([
        0x0123_4567_89ab_cdef,
        0xfedc_ba98_7654_3210,
        0x0f1e_2d3c_4b5a_6978,
        0xf899_aabb_ccdd_eeff,
    ]);
    const SHIFTS: [u64; 13] = [0, 1, 63, 64, 65, 127, 128, 129, 191, 192, 255, 256, 257];

    /// Shifts wider than a `u64`, including one whose low limb is a small valid shift.
    const WIDE_SHIFTS: [Word; 3] =
        [Word::from_limbs([1, 1, 0, 0]), Word::from_limbs([0, 0, 0, 1]), Word::MAX];

    #[test]
    fn shl_opcode() {
        assert_stack!(SHL(8, 1), 256);
        assert_stack!(SHL(0, 7), 7);
        assert_stack!(SHL(256, 1), 0);

        for value in [POSITIVE, NEGATIVE] {
            for shift in SHIFTS {
                let expected = if shift < 256 { value << shift as usize } else { Word::ZERO };
                assert_stack!(SHL(shift, value), expected);
            }
            for shift in WIDE_SHIFTS {
                assert_stack!(SHL(shift, value), 0);
            }
        }

        let [l0, l1, l2, _] = *POSITIVE.as_limbs();
        assert_stack!(SHL(64, POSITIVE), Word::from_limbs([0, l0, l1, l2]));
        assert_stack!(SHL(128, POSITIVE), Word::from_limbs([0, 0, l0, l1]));
        assert_stack!(SHL(192, POSITIVE), Word::from_limbs([0, 0, 0, l0]));
        assert_stack!(
            SHL(4, POSITIVE),
            Word::from_limbs([
                0x1234_5678_9abc_def0,
                0xedcb_a987_6543_2100,
                0xf1e2_d3c4_b5a6_978f,
                0x899a_abbc_cdde_eff0,
            ])
        );
        assert_stack!(SHL(255, 3), Word::from_limbs([0, 0, 0, 1 << 63]));
    }

    #[test]
    fn shr_opcode() {
        assert_stack!(SHR(8, 256), 1);
        assert_stack!(SHR(0, 7), 7);
        assert_stack!(SHR(256, neg(1)), 0);

        for value in [POSITIVE, NEGATIVE] {
            for shift in SHIFTS {
                let expected = if shift < 256 { value >> shift as usize } else { Word::ZERO };
                assert_stack!(SHR(shift, value), expected);
            }
            for shift in WIDE_SHIFTS {
                assert_stack!(SHR(shift, value), 0);
            }
        }

        let [_, l1, l2, l3] = *NEGATIVE.as_limbs();
        assert_stack!(SHR(64, NEGATIVE), Word::from_limbs([l1, l2, l3, 0]));
        assert_stack!(SHR(128, NEGATIVE), Word::from_limbs([l2, l3, 0, 0]));
        assert_stack!(SHR(192, NEGATIVE), Word::from_limbs([l3, 0, 0, 0]));
        assert_stack!(SHR(255, NEGATIVE), 1);
        assert_stack!(
            SHR(4, POSITIVE),
            Word::from_limbs([
                0x0012_3456_789a_bcde,
                0x8fed_cba9_8765_4321,
                0xf0f1_e2d3_c4b5_a697,
                0x0789_9aab_bccd_deef,
            ])
        );
    }

    #[test]
    fn sar_opcode() {
        assert_stack!(SAR(1, neg(1) - Word::from(1)), neg(1));
        assert_stack!(SAR(1, 4), 2);
        assert_stack!(SAR(256, neg(1)), neg(1));

        for value in [POSITIVE, NEGATIVE] {
            let fill = if value.bit(255) { Word::MAX } else { Word::ZERO };
            for shift in SHIFTS {
                let expected =
                    if shift < 256 { value.arithmetic_shr(shift as usize) } else { fill };
                assert_stack!(SAR(shift, value), expected);
            }
            for shift in WIDE_SHIFTS {
                assert_stack!(SAR(shift, value), fill);
            }
        }

        let [_, l1, l2, l3] = *NEGATIVE.as_limbs();
        assert_stack!(SAR(64, NEGATIVE), Word::from_limbs([l1, l2, l3, u64::MAX]));
        assert_stack!(SAR(128, NEGATIVE), Word::from_limbs([l2, l3, u64::MAX, u64::MAX]));
        assert_stack!(SAR(192, NEGATIVE), Word::from_limbs([l3, u64::MAX, u64::MAX, u64::MAX]));
        assert_stack!(SAR(255, NEGATIVE), Word::MAX);
        assert_stack!(SAR(255, POSITIVE), 0);
        assert_stack!(SAR(255, Word::from_limbs([0, 0, 0, 1 << 63])), Word::MAX);
        assert_stack!(SAR(254, Word::from_limbs([0, 0, 0, 1 << 63])), neg(2));
    }

    #[test]
    fn clz_opcode() {
        assert_stack!(CLZ(1), 255);
        assert_stack!(CLZ(0), 256);
        assert_stack!(CLZ(neg(1)), 0);
    }
}
