//! Shared utility helpers.

use crate::interpreter::{InstrStop, Result, Word};
use alloc::{borrow::Cow, vec};
use alloy_primitives::{Address, B256, Bytes, b256};
use core::hint::cold_path;

/// Returns the number of EVM words needed for `len` bytes.
#[inline]
pub const fn num_words(len: usize) -> usize {
    len.div_ceil(32)
}

/// Converts an address to an EVM word.
#[inline]
pub fn address_to_word(address: &Address) -> Word {
    Word::from_be_slice(address.as_slice())
}

/// Converts a 256-bit hash to an EVM word.
#[inline]
pub const fn b256_to_word(value: B256) -> Word {
    Word::from_be_bytes(value.0)
}

/// Converts an EVM word to an address.
#[inline]
pub fn word_to_address(value: Word) -> Address {
    Address::from_word(B256::from(value.to_be_bytes::<32>()))
}

/// Converts an EVM word to `usize`, returning an invalid-operand OOG stop on overflow.
#[inline]
pub fn word_to_usize(value: &Word) -> Result<usize> {
    word_to_usize_opt(value).ok_or_else(|| {
        cold_path();
        InstrStop::InvalidOperandOOG
    })
}

/// Converts an EVM word to `usize`, saturating on overflow.
#[inline]
pub fn word_to_usize_saturated(value: &Word) -> usize {
    word_to_usize_opt(value).unwrap_or(usize::MAX)
}

#[inline(always)]
fn word_to_usize_opt(value: &Word) -> Option<usize> {
    core::cfg_select! {
        // Stack words are written with one 32-byte store, which can't forward to separate loads
        // of the high limbs, so test them with one full-width load instead. Zeroing the low limb
        // with a blend keeps this a `vptest`; a masked test becomes a lane compare on AVX2.
        all(target_arch = "x86_64", target_feature = "avx2", not(miri)) => unsafe {
            use core::arch::x86_64::*;
            let v = _mm256_loadu_si256(core::ptr::from_ref(value).cast());
            let high = _mm256_blend_epi32::<0b11>(v, _mm256_setzero_si256());
            (_mm256_testz_si256(high, high) != 0)
                .then(|| _mm_cvtsi128_si64(_mm256_castsi256_si128(v)) as usize)
        },
        _ => (*value).try_into().ok(),
    }
}

/// Calculate the linear cost of a precompile.
#[inline]
pub const fn calc_linear_cost(len: usize, base: u64, word: u64) -> u64 {
    (len as u64).div_ceil(32) * word + base
}

/// Const function for making an address by concatenating the bytes from two given numbers.
///
/// Note that 32 + 128 = 160 = 20 bytes (the length of an address).
///
/// This function is used as a convenience for specifying the addresses of the various precompiles.
#[inline]
pub const fn u64_to_address(x: u64) -> Address {
    let x = x.to_be_bytes();
    Address::new([
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, x[0], x[1], x[2], x[3], x[4], x[5], x[6], x[7],
    ])
}

/// Right-pads the given slice with zeroes until `LEN`.
///
/// Returns the first `LEN` bytes if it does not need padding.
#[inline]
pub fn right_pad<const LEN: usize>(data: &[u8]) -> Cow<'_, [u8; LEN]> {
    if let Some(data) = data.first_chunk() {
        Cow::Borrowed(data)
    } else {
        let mut padded = [0; LEN];
        padded[..data.len()].copy_from_slice(data);
        Cow::Owned(padded)
    }
}

/// Right-pads the given slice with zeroes until `len`.
///
/// Returns the first `len` bytes if it does not need padding.
#[inline]
pub fn right_pad_vec(data: &[u8], len: usize) -> Cow<'_, [u8]> {
    if let Some(data) = data.get(..len) {
        Cow::Borrowed(data)
    } else {
        let mut padded = vec![0; len];
        padded[..data.len()].copy_from_slice(data);
        Cow::Owned(padded)
    }
}

/// Left-pads the given slice with zeroes until `LEN`.
///
/// Returns the first `LEN` bytes if it does not need padding.
#[inline]
pub fn left_pad<const LEN: usize>(data: &[u8]) -> Cow<'_, [u8; LEN]> {
    if let Some(data) = data.first_chunk() {
        Cow::Borrowed(data)
    } else {
        let mut padded = [0; LEN];
        padded[LEN - data.len()..].copy_from_slice(data);
        Cow::Owned(padded)
    }
}

/// Left-pads the given big-endian slice with zeroes until `len`.
///
/// When `data` is longer than `len`, this truncates the leading (most-significant) bytes.
#[inline]
pub fn left_pad_vec_be(data: &[u8], len: usize) -> Cow<'_, [u8]> {
    if data.len() < len {
        let mut padded = vec![0; len];
        padded[len - data.len()..].copy_from_slice(data);
        Cow::Owned(padded)
    } else {
        Cow::Borrowed(&data[data.len() - len..])
    }
}

/// Converts a boolean to a left-padded 32-byte [`Bytes`] value.
///
/// This is optimized to not allocate at runtime by using 2 static arrays.
#[inline]
pub const fn bool_to_bytes32(value: bool) -> Bytes {
    Bytes::from_static(&bool_to_b256(value).0)
}

/// Converts a boolean to a left-padded [`B256`] value.
///
/// This is optimized to not allocate at runtime by using 2 static arrays.
#[inline]
const fn bool_to_b256(value: bool) -> &'static B256 {
    const TRUE: &B256 =
        &b256!("0x0000000000000000000000000000000000000000000000000000000000000001");
    const FALSE: &B256 =
        &b256!("0x0000000000000000000000000000000000000000000000000000000000000000");
    if value { TRUE } else { FALSE }
}

/// Reads the 32 bytes of `data` starting at `offset`, zero-padding past the end.
#[inline]
pub fn read_word_padded(data: &[u8], offset: usize) -> B256 {
    match data.get(offset..) {
        // A fixed-size read for the common in-bounds case avoids a `memcpy` call.
        Some(rest) => match rest.first_chunk() {
            Some(chunk) => B256::new(*chunk),
            None => {
                let mut word = B256::ZERO;
                word[..rest.len()].copy_from_slice(rest);
                word
            }
        },
        None => B256::ZERO,
    }
}

/// Fixed-size bit array stored inline in `WORDS` 64-bit words.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct BitArray<const WORDS: usize>([u64; WORDS]);

impl<const WORDS: usize> BitArray<WORDS> {
    /// Creates a bit array with every bit unset.
    #[inline]
    pub(crate) const fn new() -> Self {
        Self([0; WORDS])
    }

    /// Returns whether bit `index` is set.
    #[inline]
    pub(crate) const fn get(&self, index: usize) -> bool {
        self.0[index / 64] & (1 << (index % 64)) != 0
    }

    /// Sets bit `index`.
    #[inline]
    pub(crate) const fn set(&mut self, index: usize) {
        self.0[index / 64] |= 1 << (index % 64);
    }

    /// Unsets every bit.
    #[inline]
    pub(crate) fn clear(&mut self) {
        self.0.fill(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::assert_matches;

    #[test]
    fn read_word_with_padding() {
        let data: [u8; 40] = core::array::from_fn(|i| i as u8 + 1);
        assert_eq!(read_word_padded(&data, 8).0, core::array::from_fn(|i| i as u8 + 9));
        let mut tail = [0; 32];
        tail[..2].copy_from_slice(&[39, 40]);
        assert_eq!(read_word_padded(&data, 38).0, tail);
        assert_eq!(read_word_padded(&data, 40), B256::ZERO);
        assert_eq!(read_word_padded(&data, usize::MAX), B256::ZERO);
    }

    #[test]
    fn right_padding() {
        let data = [1, 2, 3, 4];
        let padded = right_pad::<8>(&data);
        assert_matches!(padded, Cow::Owned(_));
        assert_eq!(padded[..], [1, 2, 3, 4, 0, 0, 0, 0]);
        let padded = right_pad_vec(&data, 8);
        assert_matches!(padded, Cow::Owned(_));
        assert_eq!(padded[..], [1, 2, 3, 4, 0, 0, 0, 0]);

        let data = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        let padded = right_pad::<8>(&data);
        assert_matches!(padded, Cow::Borrowed(_));
        assert_eq!(padded[..], [1, 2, 3, 4, 5, 6, 7, 8]);
        let padded = right_pad_vec(&data, 8);
        assert_matches!(padded, Cow::Borrowed(_));
        assert_eq!(padded[..], [1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn left_padding() {
        let data = [1, 2, 3, 4];
        let padded = left_pad::<8>(&data);
        assert_matches!(padded, Cow::Owned(_));
        assert_eq!(padded[..], [0, 0, 0, 0, 1, 2, 3, 4]);

        let data = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        let padded = left_pad::<8>(&data);
        assert_matches!(padded, Cow::Borrowed(_));
        assert_eq!(padded[..], [1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn left_padding_be() {
        let data = [1, 2, 3, 4];
        let padded = left_pad_vec_be(&data, 8);
        assert_matches!(padded, Cow::Owned(_));
        assert_eq!(padded[..], [0, 0, 0, 0, 1, 2, 3, 4]);

        let data = [0, 0, 1, 2, 3, 4];
        let padded = left_pad_vec_be(&data, 4);
        assert_matches!(padded, Cow::Borrowed(_));
        assert_eq!(padded[..], [1, 2, 3, 4]);

        let data = [1, 2, 3, 4];
        let padded = left_pad_vec_be(&data, 4);
        assert_matches!(padded, Cow::Borrowed(_));
        assert_eq!(padded[..], [1, 2, 3, 4]);
    }

    #[test]
    fn bool2bytes() {
        let f = bool_to_bytes32(false);
        assert_eq!(f[..], [0; 32]);
        let t = bool_to_bytes32(true);
        assert_eq!(t.len(), 32);
        assert_eq!(t[..31], [0; 31]);
        assert_eq!(t[31], 1);
    }
}
