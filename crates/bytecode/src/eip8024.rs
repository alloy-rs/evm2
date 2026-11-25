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
    if decode_single(x) == Some(n) { Some(x) } else { None }
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
        if decode_pair(x) == Some((n, m)) { Some(x) } else { None }
    };

    // q < r: n = q+1, m = r+1. q >= r: n = r+1, m = 29-q.
    try_k(n - 1, m - 1).or_else(|| 29u8.checked_sub(m).and_then(|q| try_k(q, n - 1)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_single_roundtrip() {
        // Every valid byte encodes back to itself, and every encodable index decodes back.
        let mut valid = 0;
        for x in 0..=255u8 {
            if let Some(n) = decode_single(x) {
                valid += 1;
                assert_eq!(encode_single(n), Some(x), "x={x}, n={n}");
            }
        }
        assert_eq!(valid, 91 + 128);
        let mut encodable = 0;
        for n in 0..=255u8 {
            if let Some(x) = encode_single(n) {
                encodable += 1;
                assert_eq!(decode_single(x), Some(n), "n={n}, x={x}");
            }
        }
        assert_eq!(encodable, valid);
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
        let mut valid = 0;
        for x in 0..=255u8 {
            if let Some((n, m)) = decode_pair(x) {
                valid += 1;
                assert_eq!(encode_pair(n, m), Some(x), "x={x}, (n,m)=({n},{m})");
            }
        }
        assert_eq!(valid, 82 + 128);
        // Every encodable pair decodes back, and exactly the decoded pairs are encodable.
        let mut encodable = 0;
        for n in 0..=255u8 {
            for m in 0..=255u8 {
                if let Some(x) = encode_pair(n, m) {
                    encodable += 1;
                    assert_eq!(decode_pair(x), Some((n, m)), "(n,m)=({n},{m}), x={x}");
                }
            }
        }
        assert_eq!(encodable, valid);
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
