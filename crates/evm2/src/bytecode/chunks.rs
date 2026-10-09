//! Independently executable 12 KiB code chunks.

use super::Bytecode;
use crate::interpreter::op;
use alloc::vec::Vec;
use alloy_primitives::{B256, Bytes};
use thiserror::Error;

/// Number of original code bytes in a full chunk.
pub const CODE_CHUNK_SIZE: usize = 12 * 1024;

/// A chunk of deployed bytecode that passed whole-bytecode deployment validation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeChunk {
    code: Bytecode,
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
        let code = Bytecode::new_eip7702_raw(payload.clone())
            .unwrap_or_else(|_| Bytecode::new_legacy(payload));
        Self { code }
    }

    /// Original validated payload bytes, excluding legacy analysis padding.
    pub fn bytes(&self) -> &[u8] {
        self.code.original_byte_slice()
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
    pub fn hash(&self) -> B256 {
        self.code.hash_slow()
    }

    /// Checks a local jump destination.
    pub fn is_jumpdest(&self, offset: usize) -> bool {
        offset < self.code.len()
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
    /// A 12 KiB chunk does not end with a decoded STOP opcode.
    #[error("code chunk {index} does not end with STOP")]
    ChunkWithoutStop {
        /// Zero-based chunk index.
        index: usize,
    },
}

/// Validates deployed code without constructing or caching its chunks.
pub fn validate_code(code: &[u8]) -> Result<(), CodeChunkError> {
    for (index, payload) in code.chunks(CODE_CHUNK_SIZE).enumerate() {
        validate_payload(payload, index)?;
    }
    Ok(())
}

/// Constructs one independently executable chunk from resident, previously validated code.
///
/// Validation belongs to bytecode creation or state transition, not database access. Only the
/// requested payload is copied, and [`Bytecode`](super::Bytecode) retains no complete chunk vector.
pub fn code_chunk(code: &[u8], index: u32) -> Option<CodeChunk> {
    let index = index as usize;
    let start = index.checked_mul(CODE_CHUNK_SIZE)?;
    if start >= code.len() {
        return None;
    }
    let end = (start + CODE_CHUNK_SIZE).min(code.len());
    let payload = &code[start..end];
    Some(CodeChunk::from_validated_payload(Bytes::copy_from_slice(payload)))
}

/// Validates and splits deployed code. Empty code has no chunks.
pub fn chunkify_code(code: &[u8]) -> Result<Vec<CodeChunk>, CodeChunkError> {
    validate_code(code)?;
    Ok(code
        .chunks(CODE_CHUNK_SIZE)
        .map(|payload| CodeChunk::from_validated_payload(Bytes::copy_from_slice(payload)))
        .collect())
}

fn validate_payload(payload: &[u8], index: usize) -> Result<(), CodeChunkError> {
    let mut pc = 0;
    let mut final_opcode = None;
    while pc < payload.len() {
        final_opcode = Some(payload[pc]);
        let instruction_len = 1 + push_size(payload[pc]);
        if pc + instruction_len > payload.len() {
            return Err(CodeChunkError::PushCrossesBoundary { pc: index * CODE_CHUNK_SIZE + pc });
        }
        pc += instruction_len;
    }
    if final_opcode != Some(op::STOP) {
        return Err(CodeChunkError::ChunkWithoutStop { index });
    }
    Ok(())
}

const fn push_size(opcode: u8) -> usize {
    if opcode >= op::PUSH1 && opcode <= op::PUSH32 { (opcode - op::PUSH1 + 1) as usize } else { 0 }
}

/// Draft cold-access price calibrated at one billion gas per second.
///
/// This preserves the original 28.68 microsecond operation budget at the requested 1 Ggas/s
/// calibration target. Production activation still requires measurements from the persistent
/// Tempo provider.
pub const COLD_CODE_CHUNK_GAS: u64 = 28_680;

/// Draft warm-access price calibrated to a one microsecond account-map lookup and chunk switch.
pub const WARM_CODE_CHUNK_GAS: u64 = 1_000;

/// Incremental gas for cold and warm chunk accesses.
pub const fn code_chunk_gas(cold_count: u64, warm_count: u64) -> Option<u64> {
    let cold = cold_count.checked_mul(COLD_CODE_CHUNK_GAS);
    let warm = warm_count.checked_mul(WARM_CODE_CHUNK_GAS);
    match (cold, warm) {
        (Some(cold), Some(warm)) => cold.checked_add(warm),
        _ => None,
    }
}

/// Gas for one chunk access after its warmth has been determined.
pub const fn code_chunk_access_gas(is_cold: bool) -> u64 {
    if is_cold { COLD_CODE_CHUNK_GAS } else { WARM_CODE_CHUNK_GAS }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloy_primitives::keccak256;

    #[test]
    fn chunks_are_independently_executable_and_hashed() {
        let mut code = vec![op::JUMPDEST; CODE_CHUNK_SIZE];
        *code.last_mut().unwrap() = op::STOP;
        code.extend([op::PUSH1, 0x01, op::JUMPDEST, op::STOP]);
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
    fn rejects_chunks_without_terminal_stop() {
        let code = vec![op::JUMPDEST; CODE_CHUNK_SIZE + 1];
        assert_eq!(chunkify_code(&code), Err(CodeChunkError::ChunkWithoutStop { index: 0 }));

        assert_eq!(
            chunkify_code(&[op::JUMPDEST]),
            Err(CodeChunkError::ChunkWithoutStop { index: 0 })
        );
    }

    #[test]
    fn rejects_stop_byte_that_is_push_data() {
        let mut code = vec![op::JUMPDEST; CODE_CHUNK_SIZE - 2];
        code.extend([op::PUSH1, op::STOP, op::STOP]);
        assert_eq!(validate_code(&code), Err(CodeChunkError::ChunkWithoutStop { index: 0 }));
    }

    #[test]
    fn preserves_eip7702_chunk_kind() {
        let delegated = alloy_primitives::Address::repeat_byte(0x42);
        let code = Bytecode::new_eip7702(delegated);
        let chunk = code_chunk(code.original_byte_slice(), 0).unwrap();
        assert_eq!(chunk.bytecode().eip7702_address(), Some(delegated));
    }

    #[test]
    fn tariff_is_fixed_checked_and_incremental() {
        assert_eq!(code_chunk_gas(0, 0), Some(0));
        assert_eq!(code_chunk_gas(1, 0), Some(COLD_CODE_CHUNK_GAS));
        assert_eq!(code_chunk_gas(0, 1), Some(WARM_CODE_CHUNK_GAS));
        assert_eq!(code_chunk_gas(2, 3), Some(60_360));
        assert_eq!(code_chunk_gas(u64::MAX, 0), None);
        assert_eq!(code_chunk_gas(0, u64::MAX), None);
    }
}
