//! Independently executable 12 KiB code chunks.

use super::Bytecode;
use crate::interpreter::op;
use alloc::vec::Vec;
use alloy_primitives::{B256, Bytes, keccak256};
use thiserror::Error;

/// Number of original code bytes in a full chunk.
pub const CODE_CHUNK_SIZE: usize = 12 * 1024;

/// A validated chunk of deployed bytecode.
///
/// `code` includes a synthetic trailing STOP so the interpreter can execute the chunk without
/// reading the following chunk. The payload hash and length exclude that synthetic byte.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeChunk {
    code: Bytecode,
    payload_len: usize,
    hash: B256,
}

impl CodeChunk {
    /// Constructs a chunk from a payload that has already passed deployment validation.
    pub fn new_validated(payload: Bytes) -> Option<Self> {
        if payload.is_empty() || payload.len() > CODE_CHUNK_SIZE {
            return None;
        }
        Some(Self::from_validated_payload(payload))
    }

    fn from_validated_payload(payload: Bytes) -> Self {
        let payload_len = payload.len();
        let hash = keccak256(&payload);
        let mut execution = Vec::with_capacity(payload_len + 1);
        execution.extend_from_slice(&payload);
        execution.push(op::STOP);
        Self { code: Bytecode::new_legacy(execution.into()), payload_len, hash }
    }

    /// Original payload bytes, excluding the synthetic STOP and legacy analysis padding.
    pub fn bytes(&self) -> &[u8] {
        &self.code.original_byte_slice()[..self.payload_len]
    }

    /// Bytecode suitable for independent execution by the interpreter.
    pub const fn bytecode(&self) -> &Bytecode {
        &self.code
    }

    /// Consumes this chunk and returns independently executable bytecode.
    pub fn into_bytecode(self) -> Bytecode {
        self.code
    }

    /// Hash of the original payload bytes.
    pub const fn hash(&self) -> B256 {
        self.hash
    }

    /// Checks a local jump destination.
    pub fn is_jumpdest(&self, offset: usize) -> bool {
        offset < self.payload_len
            && self.code.legacy_jump_table().is_some_and(|t| t.is_valid(offset))
    }
}

/// Deployment-time chunk validation error.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum CodeChunkError {
    /// A PUSH immediate crosses a chunk boundary or the end of deployed code.
    #[error("PUSH immediate at byte {pc} crosses a code chunk boundary")]
    PushCrossesBoundary {
        /// Global byte offset of the PUSH opcode.
        pc: usize,
    },
    /// A non-final 12 KiB chunk does not end with STOP.
    #[error("non-final code chunk {index} does not end with STOP")]
    NonFinalChunkWithoutStop {
        /// Zero-based chunk index.
        index: usize,
    },
}

/// Validates deployed code without constructing or caching its chunks.
pub fn validate_code(code: &[u8]) -> Result<(), CodeChunkError> {
    for (index, payload) in code.chunks(CODE_CHUNK_SIZE).enumerate() {
        validate_payload(payload, index, code.len())?;
    }
    Ok(())
}

/// Constructs one independently executable chunk from resident validated code.
///
/// Only the requested payload is copied. No vector containing the complete deployed code is
/// retained by [`Bytecode`](super::Bytecode).
pub fn code_chunk(code: &[u8], index: u32) -> Result<Option<CodeChunk>, CodeChunkError> {
    let index = index as usize;
    let Some(start) = index.checked_mul(CODE_CHUNK_SIZE) else {
        return Ok(None);
    };
    if start >= code.len() {
        return Ok(None);
    }
    let end = (start + CODE_CHUNK_SIZE).min(code.len());
    let payload = &code[start..end];
    validate_payload(payload, index, code.len())?;
    Ok(Some(CodeChunk::from_validated_payload(Bytes::copy_from_slice(payload))))
}

/// Validates and splits deployed code. Empty code has no chunks.
pub fn chunkify_code(code: &[u8]) -> Result<Vec<CodeChunk>, CodeChunkError> {
    validate_code(code)?;
    Ok(code
        .chunks(CODE_CHUNK_SIZE)
        .map(|payload| CodeChunk::from_validated_payload(Bytes::copy_from_slice(payload)))
        .collect())
}

fn validate_payload(
    payload: &[u8],
    index: usize,
    complete_code_len: usize,
) -> Result<(), CodeChunkError> {
    let mut pc = 0;
    while pc < payload.len() {
        let instruction_len = 1 + push_size(payload[pc]);
        if pc + instruction_len > payload.len() {
            return Err(CodeChunkError::PushCrossesBoundary { pc: index * CODE_CHUNK_SIZE + pc });
        }
        pc += instruction_len;
    }
    if payload.len() == CODE_CHUNK_SIZE
        && (index + 1) * CODE_CHUNK_SIZE < complete_code_len
        && payload.last() != Some(&op::STOP)
    {
        return Err(CodeChunkError::NonFinalChunkWithoutStop { index });
    }
    Ok(())
}

const fn push_size(opcode: u8) -> usize {
    if opcode >= op::PUSH1 && opcode <= op::PUSH32 { (opcode - op::PUSH1 + 1) as usize } else { 0 }
}

/// Proposed incremental gas for cold chunks: `cold_count * (2100 + 2 * 384)`.
/// Warm reads add zero. Existing account access, opcode, copy, and memory gas remain separate.
/// Every cold chunk costs the same, including a short final chunk, allowing pre-I/O charging.
/// Rates are experimental and require storage-I/O calibration before activation.
pub const fn code_chunk_gas(cold_count: u64) -> Option<u64> {
    cold_count.checked_mul(2100 + 2 * (CODE_CHUNK_SIZE as u64 / 32))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn chunks_are_independently_executable_and_hashed() {
        let mut code = vec![op::JUMPDEST; CODE_CHUNK_SIZE];
        *code.last_mut().unwrap() = op::STOP;
        code.extend([op::PUSH1, 0x01, op::JUMPDEST]);
        let chunks = chunkify_code(&code).unwrap();
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].bytes(), &code[..CODE_CHUNK_SIZE]);
        assert_eq!(chunks[0].hash(), keccak256(&code[..CODE_CHUNK_SIZE]));
        assert_eq!(chunks[0].bytecode().original_byte_slice().last(), Some(&op::STOP));
        assert_eq!(chunks[1].bytecode().original_byte_slice().last(), Some(&op::STOP));
        assert!(chunks[1].is_jumpdest(2));
    }

    #[test]
    fn rejects_pushes_crossing_boundaries_or_code_end() {
        assert_eq!(chunkify_code(&[]).unwrap(), vec![]);
        let mut crossing = vec![op::STOP; CODE_CHUNK_SIZE - 1];
        crossing.extend([op::PUSH1, 0x01]);
        assert_eq!(
            chunkify_code(&crossing),
            Err(CodeChunkError::PushCrossesBoundary { pc: CODE_CHUNK_SIZE - 1 })
        );
        assert_eq!(
            chunkify_code(&[op::PUSH2, 0x01]),
            Err(CodeChunkError::PushCrossesBoundary { pc: 0 })
        );
    }

    #[test]
    fn rejects_non_final_fallthrough() {
        let code = vec![op::JUMPDEST; CODE_CHUNK_SIZE + 1];
        assert_eq!(
            chunkify_code(&code),
            Err(CodeChunkError::NonFinalChunkWithoutStop { index: 0 })
        );
    }

    #[test]
    fn tariff_is_fixed_checked_and_incremental() {
        assert_eq!(code_chunk_gas(0), Some(0));
        assert_eq!(code_chunk_gas(1), Some(2868));
        assert_eq!(code_chunk_gas(2), Some(5736));
        assert_eq!(code_chunk_gas(u64::MAX), None);
    }
}
