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
