//! EIP-8024 immediate encoding of `DUPN`, `SWAPN` and `EXCHANGE`.

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
    (decode_single(x) == Some(n)).then_some(x)
}

/// Encodes a pair of stack indices into an EXCHANGE immediate byte.
///
/// Returns `None` if the pair cannot be encoded.
pub fn encode_pair(n: u8, m: u8) -> Option<u8> {
    if n == 0 || m == 0 {
        return None;
    }
    // Try both (q,r) layouts and check round-trip.
    let try_k = |q: u8, r: u8| -> Option<u8> {
        if q >= 16 || r >= 16 {
            return None;
        }
        let k = q as u16 * 16 + r as u16;
        let x = (k as u8) ^ 143;
        (decode_pair(x) == Some((n, m))).then_some(x)
    };

    // q < r: n = q+1, m = r+1. q >= r: n = r+1, m = 29-q.
    try_k(n - 1, m - 1).or_else(|| 29u8.checked_sub(m).and_then(|q| try_k(q, n - 1)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    #[test]
    fn encode_decode_single_roundtrip() {
        // Every valid byte encodes back to itself, and every encodable index decodes back.
        let decoded = (0..=255u8).filter_map(|x| Some((x, decode_single(x)?))).collect::<Vec<_>>();
        assert_eq!(decoded.len(), 91 + 128);
        for &(x, n) in &decoded {
            assert_eq!(encode_single(n), Some(x), "x={x}, n={n}");
        }
        let encoded = (0..=255u8).filter_map(|n| Some((encode_single(n)?, n))).collect::<Vec<_>>();
        for &(x, n) in &encoded {
            assert_eq!(decode_single(x), Some(n), "n={n}, x={x}");
        }
        assert_eq!(encoded.len(), decoded.len());
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
        // Every valid byte encodes back to itself, so no two bytes decode to the same pair.
        let decoded = (0..=255u8).filter_map(|x| Some((x, decode_pair(x)?))).collect::<Vec<_>>();
        assert_eq!(decoded.len(), 82 + 128);
        for &(x, (n, m)) in &decoded {
            assert_eq!(encode_pair(n, m), Some(x), "x={x}, (n,m)=({n},{m})");
        }
        // Every encodable pair decodes back, and exactly the decoded pairs are encodable.
        let pairs = (0..=255u8).flat_map(|n| (0..=255u8).map(move |m| (n, m)));
        let encoded =
            pairs.filter_map(|(n, m)| Some((encode_pair(n, m)?, (n, m)))).collect::<Vec<_>>();
        for &(x, pair) in &encoded {
            assert_eq!(decode_pair(x), Some(pair), "pair={pair:?}, x={x}");
        }
        assert_eq!(encoded.len(), decoded.len());
        // Pairs that the layouts split on.
        assert_eq!(encode_pair(1, 17), Some(79));
        // Zero indices are invalid.
        assert_eq!(encode_pair(0, 1), None);
        assert_eq!(encode_pair(1, 0), None);
        // Invalid raw bytes: [82, 127].
        assert_eq!(decode_pair(82), None);
        assert_eq!(decode_pair(127), None);
        // Edge: 80 and 81 are valid.
        assert!(decode_pair(80).is_some());
        assert!(decode_pair(81).is_some());
    }
}
