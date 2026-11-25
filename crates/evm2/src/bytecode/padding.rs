//! Legacy bytecode padding.

use crate::interpreter::op;
use alloy_primitives::Bytes;

/// Maximum PUSH immediate length plus a terminating STOP.
const PADDING: usize = 33;

/// Appends zero padding unless the bytecode already ends with 33 zeros.
pub(super) fn pad_legacy(bytecode: Bytes) -> Bytes {
    if bytecode.is_empty() {
        return Bytes::from_static(&[op::STOP]);
    }
    if bytecode.ends_with(&[0; PADDING]) {
        return bytecode;
    }

    match bytecode.0.try_into_mut() {
        Ok(mut bytecode) => {
            bytecode.extend_from_slice(&[0; PADDING]);
            bytecode.freeze().into()
        }
        Err(bytecode) => [&bytecode[..], &[0; PADDING]].concat().into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytecode::analyze_legacy;
    use alloc::{vec, vec::Vec};

    #[test]
    fn test_bytecode_ends_with_stop_still_padded() {
        let bytecode = vec![op::PUSH1, 0x01, op::PUSH1, 0x02, op::ADD, op::STOP];
        let padded_bytecode = pad_legacy(bytecode.clone().into());
        assert_eq!(padded_bytecode.len(), bytecode.len() + 33);
    }

    #[test]
    fn test_bytecode_ends_without_stop_requires_padding() {
        let bytecode = vec![op::PUSH1, 0x01, op::PUSH1, 0x02, op::ADD];
        let padded_bytecode = pad_legacy(bytecode.clone().into());
        assert_eq!(padded_bytecode.len(), bytecode.len() + 33);
    }

    #[test]
    fn test_bytecode_ends_with_push16() {
        let bytecode = vec![op::PUSH1, 0x01, op::PUSH16];
        let padded_bytecode = pad_legacy(bytecode.clone().into());
        assert_eq!(padded_bytecode.len(), bytecode.len() + 33);
    }

    #[test]
    fn test_bytecode_ends_with_push2() {
        let bytecode = vec![op::PUSH1, 0x01, op::PUSH2, 0x02];
        let padded_bytecode = pad_legacy(bytecode.clone().into());
        assert_eq!(padded_bytecode.len(), bytecode.len() + 33);
    }

    #[test]
    fn test_bytecode_with_max_push32() {
        let bytecode = vec![op::PUSH32];
        let padded_bytecode = pad_legacy(bytecode.clone().into());
        assert_eq!(padded_bytecode.len(), bytecode.len() + 33); // PUSH32 + 32 bytes + STOP
    }

    #[test]
    fn test_truncated_pushes_are_padded_without_inbounds_pointer_advance() {
        for push in op::PUSH1..=op::PUSH32 {
            let bytecode = vec![push];
            let jump_table = analyze_legacy(&bytecode);
            assert_eq!(jump_table.len(), bytecode.len());
            assert!(!jump_table.is_valid(0));
            let padded_bytecode = pad_legacy(bytecode.clone().into());
            let push_immediate_len = (push - op::PUSH1 + 1) as usize;
            assert_eq!(padded_bytecode.len(), bytecode.len() + 33);
            assert!(padded_bytecode.len() > bytecode.len() + push_immediate_len);
        }
    }

    #[test]
    fn test_bytecode_with_sequential_pushes() {
        let bytecode = vec![
            op::PUSH1,
            0x01,
            op::PUSH2,
            0x02,
            0x03,
            op::PUSH4,
            0x04,
            0x05,
            0x06,
            0x07,
            op::STOP,
        ];
        let jump_table = analyze_legacy(&bytecode);
        let padded_bytecode = pad_legacy(bytecode.clone().into());
        assert_eq!(padded_bytecode.len(), bytecode.len() + 33);
        assert!(!jump_table.is_valid(0)); // PUSH1
        assert!(!jump_table.is_valid(2)); // PUSH2
        assert!(!jump_table.is_valid(5)); // PUSH4
    }

    #[test]
    fn test_bytecode_ends_with_immediate_opcode_and_stop_requires_padding() {
        // For SWAPN/DUPN/EXCHANGE, the STOP (0x00) is consumed as the immediate operand,
        // not as an actual STOP instruction, so padding is needed.
        // The fixed padding supplies both the immediate and a terminating STOP.
        for op in [op::SWAPN, op::DUPN, op::EXCHANGE] {
            for bytecode in [vec![op], vec![op, op::STOP]] {
                let original_len = bytecode.len();
                let padded_bytecode = pad_legacy(bytecode.into());
                assert_eq!(padded_bytecode.len(), original_len + 33);
                assert_eq!(padded_bytecode[0], op);
                assert_eq!(padded_bytecode[1], op::STOP);
                assert_eq!(padded_bytecode[2], op::STOP);
            }
        }
    }

    #[test]
    fn padding_zero_suffix_boundary() {
        for len in [1, 32, 33, 34, 65] {
            let raw = Bytes::from(vec![0; len]);
            let padded = pad_legacy(raw.clone());
            if len >= 33 {
                assert_eq!(padded.len(), len);
                assert_eq!(padded.as_ptr(), raw.as_ptr());
            } else {
                assert_eq!(padded.len(), len + 33);
            }
            assert!(padded.iter().all(|&byte| byte == 0));
        }

        // Every byte of the suffix must be zero to skip padding.
        for nonzero in 0..33 {
            let mut raw = vec![0; 33];
            raw[nonzero] = op::JUMPDEST;
            let padded = pad_legacy(raw.clone().into());
            assert_eq!(padded.len(), 66);
            assert_eq!(&padded[..33], &raw);
            assert_eq!(&padded[33..], &[0; 33]);
        }
    }

    #[test]
    fn padding_reuses_owned_capacity() {
        let mut raw = Vec::with_capacity(34);
        raw.push(op::PUSH32);
        let raw = Bytes::from(raw);
        let ptr = raw.as_ptr();
        let padded = pad_legacy(raw);
        assert_eq!(padded.as_ptr(), ptr);
        assert_eq!(padded.len(), 34);
        assert_eq!(&padded[1..], &[0; 33]);
    }

    #[test]
    fn padding_releases_shared_input() {
        let raw = Bytes::from([op::PUSH32]);
        let padded = pad_legacy(raw.clone());
        assert!(raw.is_unique());
        assert_ne!(padded.as_ptr(), raw.as_ptr());
        assert_eq!(&raw[..], &[op::PUSH32]);
        assert_eq!(padded.len(), 34);
        assert_eq!(&padded[1..], &[0; 33]);
    }
}
