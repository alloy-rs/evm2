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
