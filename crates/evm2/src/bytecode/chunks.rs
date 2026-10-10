//! Prepared TIP-1143 bytecode chunks and deployment commitments.

use super::{Bytecode, JumpTable};
use crate::interpreter::{instructions::encode_rjump_offset, op};
use alloc::vec::Vec;
use alloy_primitives::{B256, Bytes};
use thiserror::Error;

/// Original bytes in a full runtime chunk.
pub const CODE_CHUNK_SIZE: usize = 24 * 1024 - 36;
/// Maximum unchanged legacy record and prepared execution buffer size.
pub const LEGACY_CODE_CHUNK_SIZE: usize = 24 * 1024;
/// Maximum number of runtime chunks.
pub const MAX_CODE_CHUNKS: usize = 40;
/// Draft runtime byte limit.
pub const MAX_CODE_SIZE: usize = CODE_CHUNK_SIZE * MAX_CODE_CHUNKS;
/// Draft resident initcode byte limit.
pub const MAX_INITCODE_SIZE: usize = 1_966_080;
/// Draft cold chunk tariff, pending production calibration.
pub const COLD_CODE_CHUNK_GAS: u64 = 28_680;
/// Draft warm chunk tariff.
pub const WARM_CODE_CHUNK_GAS: u64 = 1_000;

/// One bytecode allocation containing the stored, executable chunk and its analysis.
#[derive(Clone, Debug)]
pub struct CodeChunk {
    bytecode: Bytecode,
    prepared: Option<PreparedCodeChunk>,
}

impl PartialEq for CodeChunk {
    fn eq(&self, other: &Self) -> bool {
        self.bytecode == other.bytecode
            && self.bytecode.kind() == other.bytecode.kind()
            && self.prepared == other.prepared
    }
}

impl Eq for CodeChunk {}

impl Default for CodeChunk {
    fn default() -> Self {
        Self::from_bytecode(&Bytecode::default())
    }
}

impl From<Bytecode> for CodeChunk {
    fn from(code: Bytecode) -> Self {
        Self { bytecode: code, prepared: None }
    }
}

impl CodeChunk {
    /// Wraps ordinary code without interpreting a delegation-shaped prefix.
    pub fn new(bytes: Bytes) -> Self {
        Bytecode::new_legacy(bytes).into()
    }

    /// Retains an existing bytecode allocation and its shared analysis.
    pub fn from_bytecode(code: &Bytecode) -> Self {
        code.clone().into()
    }

    /// Returns the single stored execution buffer and its lazy jump analysis.
    pub fn bytecode(&self) -> Bytecode {
        self.bytecode.clone()
    }

    /// Stored bytes, including continued immediates, the generated tail and start-offset byte.
    /// Ordinary interpreter safety padding is excluded.
    pub fn bytes(&self) -> &[u8] {
        self.bytecode.original_byte_slice()
    }

    /// Number of logical code positions represented by this chunk, excluding its tail.
    pub fn payload_len(&self) -> usize {
        self.prepared.as_ref().map_or(self.bytecode.len(), |p| p.payload_len)
    }

    /// Number of leading immediate bytes belonging to the previous chunk's instruction.
    /// The count is the last stored byte; ordinary legacy records have no trailer.
    pub fn start_offset(&self) -> usize {
        if self.prepared.is_some() { usize::from(*self.bytes().last().unwrap()) } else { 0 }
    }

    /// Restores an already modified chunk authenticated by the provider at ingestion.
    /// The payload keeps its original bytes; the final byte identifies leading immediate data.
    pub fn from_prepared(bytes: Bytes, code_size: u32, index: u32) -> Result<Self, CodeChunkError> {
        let start = (index as usize)
            .checked_mul(CODE_CHUNK_SIZE)
            .ok_or(CodeChunkError::InvalidPreparation)?;
        let size = code_size as usize;
        if !(CODE_CHUNK_SIZE + 1..=MAX_CODE_SIZE).contains(&size) || start >= size {
            return Err(CodeChunkError::InvalidPreparation);
        }
        let payload_len = (size - start).min(CODE_CHUNK_SIZE);
        if bytes.len() <= payload_len || bytes.len() > LEGACY_CODE_CHUNK_SIZE {
            return Err(CodeChunkError::InvalidPreparation);
        }
        let start_offset = usize::from(*bytes.last().unwrap());
        if start_offset > payload_len.min(32) || (index == 0 && start_offset != 0) {
            return Err(CodeChunkError::InvalidPreparation);
        }
        let mut instruction_end = start_offset;
        while instruction_end < payload_len {
            instruction_end += instruction_len(bytes[instruction_end]);
        }
        let tail_offset = payload_len + 32;
        let continuation_len = instruction_end - payload_len;
        if bytes.get(instruction_end..tail_offset)
            != Some(padding_suffix(continuation_len).as_slice())
        {
            return Err(CodeChunkError::InvalidPreparation);
        }
        let next_chunk = (start + instruction_end < size).then_some(index + 1);
        let tail: &[u8] = if next_chunk.is_some() { &[op::RJUMP, 0x5a, 0x38] } else { &[op::STOP] };
        if bytes.get(tail_offset..bytes.len() - 1) != Some(tail) {
            return Err(CodeChunkError::InvalidPreparation);
        }
        // The prefix belongs to the preceding instruction, even if it contains JUMPDEST.
        // Preserve the existing PUSH-only jump analysis after the first real opcode.
        let mut jumps = JumpTable::new(bytes.len());
        let mut pc = start_offset;
        while pc < payload_len {
            let opcode = bytes[pc];
            if opcode == op::JUMPDEST {
                jumps.as_mut_slice()[pc / 8] |= 1 << (pc % 8);
            }
            pc += if (op::PUSH1..=op::PUSH32).contains(&opcode) {
                usize::from(opcode - op::PUSH1) + 2
            } else {
                1
            };
        }
        // Only generated padding JUMPDESTs are valid physical destinations beyond the payload.
        for pc in instruction_end..tail_offset {
            if bytes[pc] == op::JUMPDEST {
                jumps.as_mut_slice()[pc / 8] |= 1 << (pc % 8);
            }
        }
        let padded = Bytecode::new_legacy(bytes);
        // SAFETY: new_legacy supplies ordinary safety padding for all immediate reads.
        // The map has one bit per stored byte and excludes prefix data and generated tails.
        let bytecode =
            unsafe { Bytecode::new_analyzed(padded.bytes().clone(), padded.len(), jumps) };
        Ok(Self { bytecode, prepared: Some(PreparedCodeChunk { code_size, index, payload_len }) })
    }

    /// Logical context of a prepared chunk; ordinary legacy records have none.
    pub const fn prepared(&self) -> Option<&PreparedCodeChunk> {
        self.prepared.as_ref()
    }

    /// Checks the loaded buffer's logical size and index against the account.
    pub fn validate_context(&self, code_size: u32, index: u32) -> Result<(), CodeChunkError> {
        if self.prepared.as_ref().is_some_and(|p| p.code_size == code_size && p.index == index) {
            Ok(())
        } else {
            Err(CodeChunkError::InvalidPreparation)
        }
    }

    /// Dynamic jumps may enter only JUMPDESTs in the logical payload.
    pub fn is_valid_jumpdest(&self, local_pc: usize) -> bool {
        local_pc < self.payload_len()
            && self.bytecode.legacy_jump_table().is_some_and(|map| map.is_valid(local_pc))
    }
}

/// Validated commitments present only for multi-chunk code.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(try_from = "MetadataFields"))]
pub struct CodeMetadata {
    code_size: u32,
    chunk_hashes: Vec<B256>,
}

impl CodeMetadata {
    /// Validates the size and ordered hash count. Payload authentication belongs to ingestion.
    pub fn new(code_size: u32, chunk_hashes: Vec<B256>) -> Result<Self, CodeChunkError> {
        let size = usize::try_from(code_size).map_err(|_| CodeChunkError::InvalidMetadata)?;
        if !(CODE_CHUNK_SIZE + 1..=MAX_CODE_SIZE).contains(&size)
            || chunk_hashes.len() != size.div_ceil(CODE_CHUNK_SIZE)
        {
            return Err(CodeChunkError::InvalidMetadata);
        }
        Ok(Self { code_size, chunk_hashes })
    }

    /// Committed original code length.
    pub const fn code_size(&self) -> u32 {
        self.code_size
    }

    /// Ordered commitments to complete stored chunk buffers, including the start-offset byte.
    pub fn chunk_hashes(&self) -> &[B256] {
        &self.chunk_hashes
    }

    /// Exact payload length for an in-range index, without provider access.
    pub fn chunk_len(&self, index: u32) -> Option<usize> {
        let start = usize::try_from(index).ok()?.checked_mul(CODE_CHUNK_SIZE)?;
        let remaining = usize::try_from(self.code_size).ok()?.checked_sub(start)?;
        (remaining != 0).then_some(remaining.min(CODE_CHUNK_SIZE))
    }
}

/// Deployment or metadata validation failure.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum CodeChunkError {
    /// Runtime exceeds the draft capacity.
    #[error("runtime exceeds the TIP-1143 code size limit")]
    CodeTooLarge,
    /// Size and commitments do not describe multi-chunk code.
    #[error("invalid multi-chunk code metadata")]
    InvalidMetadata,
    /// Provider preparation violates the bounded instruction layout.
    #[error("invalid chunk preparation")]
    InvalidPreparation,
    /// A PUSH immediate crosses a payload boundary or the original code end.
    #[error("PUSH immediate at byte {pc} crosses a code chunk boundary")]
    PushCrossesBoundary {
        /// Global PUSH opcode offset.
        pc: usize,
    },
    /// The last original byte of a non-final chunk is not a decoded STOP.
    #[error("code chunk {index} does not end with STOP")]
    ChunkWithoutStop {
        /// Original payload index.
        index: usize,
    },
}

/// Checks the capacity of submitted original bytes; PUSH continuations are prepared separately.
pub const fn validate_code(code: &[u8]) -> Result<(), CodeChunkError> {
    if code.len() > MAX_CODE_SIZE {
        return Err(CodeChunkError::CodeTooLarge);
    }

    Ok(())
}

/// Validates creation output and commits to the stored, modified chunk buffers.
pub fn code_metadata(code: &[u8]) -> Result<Option<CodeMetadata>, CodeChunkError> {
    validate_code(code)?;
    if code.len() <= CODE_CHUNK_SIZE {
        return Ok(None);
    }
    let size = u32::try_from(code.len()).map_err(|_| CodeChunkError::CodeTooLarge)?;
    let code = Bytes::copy_from_slice(code);
    let hashes = (0..code.len().div_ceil(CODE_CHUNK_SIZE))
        .map(|index| {
            code_chunk(&code, index as u32).expect("validated chunk index").bytecode.hash_slow()
        })
        .collect();
    CodeMetadata::new(size, hashes).map(Some)
}

/// Prepares one chunk at creation or ingestion, without retaining its original slice.
pub fn code_chunk(code: &Bytes, index: u32) -> Option<CodeChunk> {
    let start = usize::try_from(index).ok()?.checked_mul(CODE_CHUNK_SIZE)?;
    if start >= code.len() {
        return None;
    }
    if code.len() <= CODE_CHUNK_SIZE {
        return Some(CodeChunk::new(code.clone()));
    }
    let end = start.checked_add(CODE_CHUNK_SIZE)?.min(code.len());
    let mut pc = 0;
    while pc < start {
        pc += instruction_len(code[pc]);
    }
    let leading = pc.min(end).saturating_sub(start);
    pc = pc.min(end);
    while pc < end {
        pc += instruction_len(code[pc]);
    }
    let mut bytes = code[start..pc.min(code.len())].to_vec();
    bytes.resize(pc - start, 0);
    bytes.extend_from_slice(&padding_suffix(pc - end));
    if pc < code.len() {
        // The fixed padding and RJUMP width place the successor's logical start 35 bytes back.
        // The transfer handler then advances over that chunk's leading immediate data.
        bytes.push(op::RJUMP);
        bytes.extend_from_slice(&encode_rjump_offset(-35).unwrap());
    } else {
        bytes.push(op::STOP);
    }
    bytes.push(leading as u8);
    CodeChunk::from_prepared(bytes.into(), code.len().try_into().ok()?, index).ok()
}

/// Checked aggregate incremental tariff for one operation.
pub const fn code_chunk_gas(cold: u64, warm: u64) -> Option<u64> {
    match (cold.checked_mul(COLD_CODE_CHUNK_GAS), warm.checked_mul(WARM_CODE_CHUNK_GAS)) {
        (Some(cold), Some(warm)) => cold.checked_add(warm),
        _ => None,
    }
}

#[cfg(feature = "serde")]
#[derive(serde::Deserialize)]
struct MetadataFields {
    code_size: u32,
    chunk_hashes: Vec<B256>,
}

#[cfg(feature = "serde")]
impl TryFrom<MetadataFields> for CodeMetadata {
    type Error = CodeChunkError;

    fn try_from(fields: MetadataFields) -> Result<Self, Self::Error> {
        Self::new(fields.code_size, fields.chunk_hashes)
    }
}

/// Logical context for an immutable prepared bytecode buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreparedCodeChunk {
    code_size: u32,
    index: u32,
    payload_len: usize,
}

impl PreparedCodeChunk {
    /// Complete logical code size.
    pub const fn code_size(&self) -> u32 {
        self.code_size
    }

    /// Position in the account's ordered chunk list.
    pub const fn index(&self) -> u32 {
        self.index
    }

    /// Local position of the generated RJUMP or STOP.
    pub const fn tail_offset(&self) -> usize {
        self.payload_len + 32
    }
}

fn instruction_len(opcode: u8) -> usize {
    if (0x60..=0x7f).contains(&opcode) {
        usize::from(opcode - 0x5f) + 1
    } else if matches!(opcode, op::RJUMP | op::RJUMPI) {
        3
    } else if (0xe6..=0xe8).contains(&opcode) {
        2
    } else {
        1
    }
}

/// Fill unused padding with a checked local jump, or up to three JUMPDESTs.
fn padding_suffix(continuation_len: usize) -> Vec<u8> {
    let remaining = 32 - continuation_len;
    let mut padding = alloc::vec![op::JUMPDEST; remaining];
    if remaining >= 4 {
        padding.fill(op::STOP);
        padding[0] = op::RJUMP;
        padding[1..3].copy_from_slice(&encode_rjump_offset((remaining - 4) as isize).unwrap());
        padding[remaining - 1] = op::JUMPDEST;
    }
    padding
}
