use super::super::Word;

#[inline]
pub(in crate::interpreter) fn as_usize(value: Word) -> Option<usize> {
    let limbs = value.as_limbs();
    if limbs[1..].iter().any(|&limb| limb != 0) || limbs[0] > usize::MAX as u64 {
        None
    } else {
        Some(limbs[0] as usize)
    }
}

#[inline]
pub(in crate::interpreter) fn i256_cmp(a: &Word, b: &Word) -> core::cmp::Ordering {
    match (a.bit(255), b.bit(255)) {
        (false, true) => core::cmp::Ordering::Greater,
        (true, false) => core::cmp::Ordering::Less,
        _ => a.cmp(b),
    }
}

#[inline]
pub(in crate::interpreter) fn i256_div(a: Word, b: Word) -> Word {
    if b.is_zero() {
        return Word::ZERO;
    }
    let a_neg = a.bit(255);
    let b_neg = b.bit(255);
    let q = i256_abs(a) / i256_abs(b);
    if a_neg ^ b_neg { i256_neg(q) } else { q }
}

#[inline]
pub(in crate::interpreter) fn i256_mod(a: Word, b: Word) -> Word {
    if b.is_zero() {
        return Word::ZERO;
    }
    let r = i256_abs(a) % i256_abs(b);
    if a.bit(255) { i256_neg(r) } else { r }
}

#[inline]
pub(in crate::interpreter) fn i256_abs(value: Word) -> Word {
    if value.bit(255) { i256_neg(value) } else { value }
}

#[inline]
pub(in crate::interpreter) fn i256_neg(value: Word) -> Word {
    (!value).wrapping_add(Word::from(1))
}
