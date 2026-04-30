use crate::interpreter::Word;

#[inline]
pub(in crate::interpreter) fn as_usize(value: Word) -> Option<usize> {
    value.try_into().ok()
}
