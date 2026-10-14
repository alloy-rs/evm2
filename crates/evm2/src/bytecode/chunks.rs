//! TIP-1143 bytecode chunks and deployment commitments.
//!
//! Each stored chunk contains up to [`CODE_CHUNK_SIZE`] logical bytes, 32 bytes for a
//! boundary-straddling immediate, and one byte recording where execution resumes in that
//! chunk. Unused continuation bytes are zero. The dispatcher switches chunks when the next
//! logical instruction position reaches the payload boundary, charging only the next chunk's
//! warm or cold tariff; no generated opcode or padding instruction executes.

use super::{Bytecode, BytecodeKind};
use crate::interpreter::op;
use alloc::vec::Vec;
use alloy_primitives::{B256, Bytes};
use thiserror::Error;

#[cfg(feature = "serde")]
use alloc::string::String;
#[cfg(feature = "serde")]
use serde::de::{Error, MapAccess, SeqAccess, Visitor};

/// Maximum unchanged legacy record and stored chunk buffer size.
pub const LEGACY_CODE_CHUNK_SIZE: usize = 24 * 1024;
/// Original bytes in a full runtime chunk, leaving 32 continuation bytes and one offset byte.
pub const CODE_CHUNK_SIZE: usize = LEGACY_CODE_CHUNK_SIZE - 33;
/// Maximum number of runtime chunks.
pub const MAX_CODE_CHUNKS: usize = 40;
/// Draft runtime byte limit.
pub const MAX_CODE_SIZE: usize = CODE_CHUNK_SIZE * MAX_CODE_CHUNKS;
/// Draft initcode byte limit (2 MiB), independent of the runtime chunk count.
pub const MAX_INITCODE_SIZE: usize = 2 * 1024 * 1024;
/// Draft cold chunk tariff, pending production calibration.
pub const COLD_CODE_CHUNK_GAS: u64 = 28_680;
/// Draft warm chunk tariff.
pub const WARM_CODE_CHUNK_GAS: u64 = 1_000;

/// Code supplied to a frame or loaded from an account.
#[derive(Clone, Debug)]
pub enum CodeChunk {
    /// Complete bytecode, including initcode, executed without account chunk loading.
    Direct(Bytecode),
    /// Complete legacy account code, without chunk context or an offset trailer.
    Legacy(Bytecode),
    /// One slice of a larger runtime, with its logical size and index.
    Chunked(ChunkedCodeChunk),
}

impl PartialEq for CodeChunk {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Direct(code), Self::Direct(other))
            | (Self::Legacy(code), Self::Legacy(other)) => {
                code == other && code.kind() == other.kind()
            }
            (Self::Chunked(chunk), Self::Chunked(other)) => chunk == other,
            _ => false,
        }
    }
}

impl Eq for CodeChunk {}

impl Default for CodeChunk {
    fn default() -> Self {
        Self::Direct(Bytecode::default())
    }
}

impl From<Bytecode> for CodeChunk {
    fn from(code: Bytecode) -> Self {
        Self::Direct(code)
    }
}

impl CodeChunk {
    /// Wraps legacy account code without interpreting a delegation-shaped prefix.
    pub fn new_legacy(bytes: Bytes) -> Self {
        Self::Legacy(Bytecode::new_legacy(bytes))
    }

    /// Retains a legacy account's bytecode allocation and its shared analysis.
    pub fn from_legacy_bytecode(code: &Bytecode) -> Self {
        Self::Legacy(code.clone())
    }

    /// Returns the single stored execution buffer and its lazy jump analysis.
    pub fn bytecode(&self) -> Bytecode {
        self.as_bytecode().clone()
    }

    /// Stored bytes, including continued immediates, zero fill, and the start-offset byte.
    /// Ordinary interpreter safety padding is excluded.
    pub fn bytes(&self) -> &[u8] {
        self.as_bytecode().original_byte_slice()
    }

    /// Number of logical code positions represented by this chunk.
    pub fn payload_len(&self) -> usize {
        match self {
            Self::Direct(code) | Self::Legacy(code) => code.len(),
            Self::Chunked(chunk) => chunk.payload_len(),
        }
    }

    /// Number of leading immediate bytes belonging to the previous chunk's instruction.
    /// The count is the last stored byte; ordinary legacy records have no trailer.
    pub fn start_offset(&self) -> usize {
        match self.as_bytecode().kind() {
            BytecodeKind::Chunk(layout) => layout.start_offset(),
            _ => 0,
        }
    }

    /// Attaches account context to a provider's validated, shared chunk bytecode.
    pub fn from_chunked(
        bytecode: Bytecode,
        code_size: u32,
        index: u32,
    ) -> Result<Self, CodeChunkError> {
        let BytecodeKind::Chunk(layout) = bytecode.kind() else {
            return Err(CodeChunkError::InvalidPreparation);
        };
        let start = (index as usize)
            .checked_mul(CODE_CHUNK_SIZE)
            .ok_or(CodeChunkError::InvalidPreparation)?;
        let size = code_size as usize;
        let bytes = bytecode.original_byte_slice();
        if !(CODE_CHUNK_SIZE + 1..=MAX_CODE_SIZE).contains(&size)
            || start >= size
            || layout.payload_len(bytes) != (size - start).min(CODE_CHUNK_SIZE)
            || (index == 0 && layout.start_offset != 0)
        {
            return Err(CodeChunkError::InvalidPreparation);
        }
        Ok(Self::Chunked(ChunkedCodeChunk { bytecode, code_size, index }))
    }

    /// Chunked runtime buffer and its logical context, absent for complete code.
    pub const fn chunked(&self) -> Option<&ChunkedCodeChunk> {
        match self {
            Self::Direct(_) | Self::Legacy(_) => None,
            Self::Chunked(chunk) => Some(chunk),
        }
    }

    /// Checks the loaded buffer's logical size and index against the account.
    pub fn validate_context(&self, code_size: u32, index: u32) -> Result<(), CodeChunkError> {
        if self.chunked().is_some_and(|p| p.code_size == code_size && p.index == index) {
            Ok(())
        } else {
            Err(CodeChunkError::InvalidPreparation)
        }
    }

    /// Dynamic jumps may enter only JUMPDESTs in the logical payload.
    pub fn is_valid_jumpdest(&self, local_pc: usize) -> bool {
        local_pc < self.payload_len()
            && self.as_bytecode().legacy_jump_table().is_some_and(|map| map.is_valid(local_pc))
    }

    /// Borrows the owned bytecode and its immutable execution analysis.
    pub const fn as_bytecode(&self) -> &Bytecode {
        match self {
            Self::Direct(code) | Self::Legacy(code) => code,
            Self::Chunked(chunk) => &chunk.bytecode,
        }
    }

    /// Logical chunk index; ordinary unchunked code starts at zero.
    pub const fn index(&self) -> u32 {
        match self {
            Self::Direct(_) | Self::Legacy(_) => 0,
            Self::Chunked(chunk) => chunk.index,
        }
    }

    /// Complete logical code length, excluding chunk padding and tails.
    pub fn code_size(&self) -> usize {
        match self {
            Self::Direct(code) | Self::Legacy(code) => code.len(),
            Self::Chunked(chunk) => chunk.code_size as usize,
        }
    }

    /// Whether this code was loaded from an account and uses account gas rules.
    pub const fn is_account(&self) -> bool {
        matches!(self, Self::Legacy(_) | Self::Chunked(_))
    }
}

/// Validated commitments present only for multi-chunk code.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct CodeMetadata {
    last_chunk_size: u32,
    chunk_hashes: Vec<B256>,
}

impl CodeMetadata {
    /// Validates the last chunk size and ordered hash count.
    /// Payload authentication belongs to ingestion.
    pub fn new(last_chunk_size: u32, chunk_hashes: Vec<B256>) -> Result<Self, CodeChunkError> {
        validate_metadata(last_chunk_size, chunk_hashes.len())?;
        Ok(Self { last_chunk_size, chunk_hashes })
    }

    /// Committed original code length.
    pub const fn code_size(&self) -> u32 {
        (self.chunk_hashes.len() as u32 - 1) * CODE_CHUNK_SIZE as u32 + self.last_chunk_size
    }

    /// Ordered commitments to complete stored chunk buffers, including the start-offset byte.
    pub fn chunk_hashes(&self) -> &[B256] {
        &self.chunk_hashes
    }

    /// Number of committed chunks.
    pub const fn chunk_count(&self) -> usize {
        self.chunk_hashes.len()
    }

    /// Hash at the given logical chunk index.
    pub fn chunk_hash(&self, index: u32) -> Option<B256> {
        self.chunk_hashes.get(index as usize).copied()
    }

    /// Exact payload length for an in-range index, without provider access.
    pub fn chunk_len(&self, index: u32) -> Option<usize> {
        let index = usize::try_from(index).ok()?;
        (index < self.chunk_hashes.len()).then(|| {
            if index == self.chunk_hashes.len() - 1 {
                self.last_chunk_size as usize
            } else {
                CODE_CHUNK_SIZE
            }
        })
    }

    /// Committed original payload length of the last chunk.
    pub const fn last_chunk_size(&self) -> u32 {
        self.last_chunk_size
    }
}

fn validate_metadata(last_chunk_size: u32, chunk_count: usize) -> Result<(), CodeChunkError> {
    if !(1..=CODE_CHUNK_SIZE as u32).contains(&last_chunk_size)
        || !(2..=MAX_CODE_CHUNKS).contains(&chunk_count)
        || (chunk_count == 2
            && last_chunk_size as usize <= LEGACY_CODE_CHUNK_SIZE - CODE_CHUNK_SIZE)
    {
        return Err(CodeChunkError::InvalidMetadata);
    }
    Ok(())
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
    prepare_code_chunks(&Bytes::copy_from_slice(code))
        .map(|prepared| prepared.map(|(metadata, _)| metadata))
}

/// Prepares all runtime chunks and their commitments in one instruction scan.
pub fn prepare_code_chunks(
    code: &Bytes,
) -> Result<Option<(CodeMetadata, Vec<CodeChunk>)>, CodeChunkError> {
    validate_code(code)?;
    if code.len() <= LEGACY_CODE_CHUNK_SIZE {
        return Ok(None);
    }
    let count = code.len().div_ceil(CODE_CHUNK_SIZE);
    let mut chunks = Vec::with_capacity(count);
    let mut hashes = Vec::with_capacity(count);
    let mut pc = 0;
    for index in 0..count {
        let chunk = prepare_code_chunk(code, index, &mut pc)?;
        hashes.push(chunk.as_bytecode().hash_slow());
        chunks.push(chunk);
    }
    let last_chunk_size = ((code.len() - 1) % CODE_CHUNK_SIZE + 1) as u32;
    Ok(Some((CodeMetadata::new(last_chunk_size, hashes)?, chunks)))
}

/// Prepares one chunk at creation or ingestion, without retaining its original slice.
pub fn code_chunk(code: &Bytes, index: u32) -> Option<CodeChunk> {
    let start = usize::try_from(index).ok()?.checked_mul(CODE_CHUNK_SIZE)?;
    if start >= code.len() {
        return None;
    }
    if code.len() <= LEGACY_CODE_CHUNK_SIZE {
        return (index == 0).then(|| CodeChunk::new_legacy(code.clone()));
    }
    let mut pc = 0;
    while pc < start {
        pc += instruction_len(code[pc]);
    }
    prepare_code_chunk(code, index as usize, &mut pc).ok()
}

fn prepare_code_chunk(
    code: &Bytes,
    index: usize,
    pc: &mut usize,
) -> Result<CodeChunk, CodeChunkError> {
    let start = index * CODE_CHUNK_SIZE;
    let end = (start + CODE_CHUNK_SIZE).min(code.len());
    let leading = (*pc).min(end).saturating_sub(start);
    *pc = (*pc).min(end);
    while *pc < end {
        *pc += instruction_len(code[*pc]);
    }
    let mut bytes = code[start..(*pc).min(code.len())].to_vec();
    bytes.resize(*pc - start, 0);
    // A boundary-straddling instruction reads its immediate here. The dispatcher switches
    // chunks before any padding can execute as code.
    bytes.resize(end - start + 32, 0);
    bytes.push(leading as u8);
    CodeChunk::from_chunked(
        Bytecode::new_chunk(bytes.into())?,
        code.len().try_into().map_err(|_| CodeChunkError::CodeTooLarge)?,
        index.try_into().map_err(|_| CodeChunkError::CodeTooLarge)?,
    )
}

/// Checked aggregate incremental tariff for one operation.
pub const fn code_chunk_gas(cold: u64, warm: u64) -> Option<u64> {
    match (cold.checked_mul(COLD_CODE_CHUNK_GAS), warm.checked_mul(WARM_CODE_CHUNK_GAS)) {
        (Some(cold), Some(warm)) => cold.checked_add(warm),
        _ => None,
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for CodeMetadata {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct MetadataVisitor;

        impl<'de> Visitor<'de> for MetadataVisitor {
            type Value = CodeMetadata;

            fn expecting(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                formatter.write_str("code chunk metadata")
            }

            fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> Result<Self::Value, S::Error> {
                let last_chunk_size =
                    seq.next_element()?.ok_or_else(|| S::Error::invalid_length(0, &self))?;
                let chunk_hashes =
                    seq.next_element()?.ok_or_else(|| S::Error::invalid_length(1, &self))?;
                CodeMetadata::new(last_chunk_size, chunk_hashes).map_err(S::Error::custom)
            }

            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let mut last_chunk_size = None;
                let mut chunk_hashes = None;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "last_chunk_size" => {
                            if last_chunk_size.replace(map.next_value()?).is_some() {
                                return Err(M::Error::duplicate_field("last_chunk_size"));
                            }
                        }
                        "chunk_hashes" => {
                            if chunk_hashes.replace(map.next_value()?).is_some() {
                                return Err(M::Error::duplicate_field("chunk_hashes"));
                            }
                        }
                        _ => {
                            map.next_value::<serde::de::IgnoredAny>()?;
                        }
                    }
                }
                CodeMetadata::new(
                    last_chunk_size.ok_or_else(|| M::Error::missing_field("last_chunk_size"))?,
                    chunk_hashes.ok_or_else(|| M::Error::missing_field("chunk_hashes"))?,
                )
                .map_err(M::Error::custom)
            }
        }

        deserializer.deserialize_struct(
            "CodeMetadata",
            &["last_chunk_size", "chunk_hashes"],
            MetadataVisitor,
        )
    }
}

/// One validated runtime buffer and the context needed to load adjacent chunks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChunkedCodeChunk {
    bytecode: Bytecode,
    code_size: u32,
    index: u32,
}

impl ChunkedCodeChunk {
    /// Complete logical code size.
    pub const fn code_size(&self) -> u32 {
        self.code_size
    }

    /// Position in the account's ordered chunk list.
    pub const fn index(&self) -> u32 {
        self.index
    }

    /// Logical bytes in this chunk, excluding continuation bytes and padding.
    pub const fn payload_len(&self) -> usize {
        let remaining = self.code_size as usize - self.index as usize * CODE_CHUNK_SIZE;
        if remaining < CODE_CHUNK_SIZE { remaining } else { CODE_CHUNK_SIZE }
    }
}

/// Validated physical layout of a stored chunk buffer, independent of account context.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChunkLayout {
    start_offset: u8,
}

impl ChunkLayout {
    /// Validates stored bytes before the ordinary in-memory safety padding is added.
    pub(super) fn decode(bytes: &[u8]) -> Result<Self, CodeChunkError> {
        let invalid = CodeChunkError::InvalidPreparation;
        if bytes.len() > LEGACY_CODE_CHUNK_SIZE {
            return Err(invalid);
        }
        let Some((&start_offset, code)) = bytes.split_last() else { return Err(invalid) };
        let payload_len = code.len().checked_sub(32).ok_or(invalid)?;
        if !(1..=CODE_CHUNK_SIZE).contains(&payload_len)
            || usize::from(start_offset) > payload_len.min(32)
        {
            return Err(invalid);
        }
        let padding_start = chunk_padding_start(&code[..payload_len], start_offset as usize);
        if !code.get(padding_start..).is_some_and(|padding| padding.iter().all(|&byte| byte == 0)) {
            return Err(invalid);
        }
        Ok(Self { start_offset })
    }

    /// Number of original code positions represented by the buffer.
    pub(super) const fn payload_len(self, bytes: &[u8]) -> usize {
        bytes.len() - 33
    }

    /// Leading bytes continuing the previous chunk's immediate.
    pub const fn start_offset(self) -> usize {
        self.start_offset as usize
    }
}

fn chunk_padding_start(payload: &[u8], start_offset: usize) -> usize {
    let mut pc = start_offset;
    while pc < payload.len() {
        pc += instruction_len(payload[pc]);
    }
    pc
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_boundary_is_24_kib() {
        for size in [CODE_CHUNK_SIZE, CODE_CHUNK_SIZE + 1, LEGACY_CODE_CHUNK_SIZE] {
            let code = Bytes::from(alloc::vec![op::STOP; size]);
            assert!(prepare_code_chunks(&code).unwrap().is_none());
            assert!(matches!(code_chunk(&code, 0), Some(CodeChunk::Legacy(_))));
            assert!(code_chunk(&code, 1).is_none());
        }
        let code = Bytes::from(alloc::vec![op::STOP; LEGACY_CODE_CHUNK_SIZE + 1]);
        assert_eq!(prepare_code_chunks(&code).unwrap().unwrap().0.chunk_count(), 2);
    }

    #[test]
    fn streaming_preparation_matches_individual_chunks_across_immediates() {
        let mut code = alloc::vec![op::STOP; 3 * CODE_CHUNK_SIZE + 37];
        code[CODE_CHUNK_SIZE - 1] = op::PUSH32;
        code[2 * CODE_CHUNK_SIZE - 2] = op::RJUMP;
        code[3 * CODE_CHUNK_SIZE - 1] = op::DUPN;
        let code = Bytes::from(code);
        let (metadata, chunks) = prepare_code_chunks(&code).unwrap().unwrap();

        assert_eq!(chunks.len(), 4);
        assert_eq!(chunks[1].start_offset(), 32);
        assert_eq!(chunks[2].start_offset(), 1);
        assert_eq!(chunks[3].start_offset(), 1);
        for (index, chunk) in chunks.iter().enumerate() {
            assert_eq!(chunk, &code_chunk(&code, index as u32).unwrap());
            assert_eq!(metadata.chunk_hashes()[index], chunk.as_bytecode().hash_slow());
            assert_eq!(chunk.payload_len(), metadata.chunk_len(index as u32).unwrap());
        }
    }

    #[test]
    fn metadata_last_chunk_size() {
        for size in [
            LEGACY_CODE_CHUNK_SIZE + 1,
            2 * CODE_CHUNK_SIZE,
            2 * CODE_CHUNK_SIZE + 1,
            MAX_CODE_SIZE,
        ] {
            let metadata = code_metadata(&alloc::vec![op::STOP; size]).unwrap().unwrap();
            let count = size.div_ceil(CODE_CHUNK_SIZE);
            let last_chunk_size = size - (count - 1) * CODE_CHUNK_SIZE;
            assert_eq!(metadata.code_size() as usize, size);
            assert_eq!(metadata.chunk_hashes().len(), count);
            assert_eq!(metadata.last_chunk_size() as usize, last_chunk_size);
            for index in 0..count - 1 {
                assert_eq!(metadata.chunk_len(index as u32), Some(CODE_CHUNK_SIZE));
            }
            assert_eq!(metadata.chunk_len(count as u32 - 1), Some(last_chunk_size));
            assert_eq!(metadata.chunk_len(count as u32), None);
            assert_eq!(metadata.chunk_len(u32::MAX), None);
        }
    }

    #[test]
    fn metadata_rejects_invalid_size_and_count() {
        for (last_chunk_size, count) in [
            (0, 2),
            (CODE_CHUNK_SIZE as u32 + 1, 2),
            (u32::MAX, 2),
            (1, 0),
            (1, 1),
            (LEGACY_CODE_CHUNK_SIZE as u32 - CODE_CHUNK_SIZE as u32, 2),
            (1, MAX_CODE_CHUNKS + 1),
        ] {
            assert_eq!(
                CodeMetadata::new(last_chunk_size, alloc::vec![B256::ZERO; count]),
                Err(CodeChunkError::InvalidMetadata)
            );
        }
    }

    #[test]
    #[cfg(feature = "serde")]
    fn metadata_serde_last_chunk_size() {
        let metadata = CodeMetadata::new(7, alloc::vec![B256::ZERO; 3]).unwrap();
        let json = serde_json::json!({
            "last_chunk_size": 7,
            "chunk_hashes": [B256::ZERO, B256::ZERO, B256::ZERO],
        });
        assert_eq!(serde_json::to_value(&metadata).unwrap(), json);
        assert_eq!(serde_json::from_value::<CodeMetadata>(json).unwrap(), metadata);
        let encoded = postcard::to_allocvec(&metadata).unwrap();
        assert_eq!(postcard::from_bytes::<CodeMetadata>(&encoded).unwrap(), metadata);
        for last_chunk_size in [0, CODE_CHUNK_SIZE as u32 + 1] {
            let json = serde_json::json!({
                "last_chunk_size": last_chunk_size,
                "chunk_hashes": [B256::ZERO, B256::ZERO],
            });
            assert!(serde_json::from_value::<CodeMetadata>(json).is_err());
        }
    }
}
