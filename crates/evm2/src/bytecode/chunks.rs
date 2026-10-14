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
pub struct CodeChunk(CodeChunkKind);

#[derive(Clone, Debug)]
pub(crate) enum CodeChunkKind {
    /// Complete bytecode, including initcode, executed without account chunk loading.
    Direct(Bytecode),
    /// Complete legacy account code, without chunk context or an offset trailer.
    Legacy(Bytecode),
    /// One slice of a larger runtime, with its logical size and index.
    Chunked(ChunkedCodeChunk),
}

impl PartialEq for CodeChunk {
    fn eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (CodeChunkKind::Direct(code), CodeChunkKind::Direct(other))
            | (CodeChunkKind::Legacy(code), CodeChunkKind::Legacy(other)) => {
                code == other && code.kind() == other.kind()
            }
            (CodeChunkKind::Chunked(chunk), CodeChunkKind::Chunked(other)) => chunk == other,
            _ => false,
        }
    }
}

impl Eq for CodeChunk {}

impl Default for CodeChunk {
    fn default() -> Self {
        Self(CodeChunkKind::Direct(Bytecode::default()))
    }
}

impl From<Bytecode> for CodeChunk {
    fn from(code: Bytecode) -> Self {
        let code = normalize_complete_code(&code);
        Self(CodeChunkKind::Direct(code))
    }
}

impl CodeChunk {
    /// Wraps legacy account code without interpreting a delegation-shaped prefix.
    pub fn new_legacy(bytes: Bytes) -> Self {
        Self(CodeChunkKind::Legacy(Bytecode::new_legacy(bytes)))
    }

    /// Retains a legacy account's bytecode allocation and its shared analysis.
    pub fn from_legacy_bytecode(code: &Bytecode) -> Self {
        Self(CodeChunkKind::Legacy(normalize_complete_code(code)))
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
            Self(CodeChunkKind::Direct(code) | CodeChunkKind::Legacy(code)) => code.len(),
            Self(CodeChunkKind::Chunked(chunk)) => chunk.payload_len(),
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
        let bytecode = match bytecode.kind() {
            BytecodeKind::Chunk(_) => bytecode,
            _ => Bytecode::new_chunk(bytecode.original_bytes())?,
        };
        let BytecodeKind::Chunk(layout) = bytecode.kind() else { unreachable!() };
        let start = (index as usize)
            .checked_mul(CODE_CHUNK_SIZE)
            .ok_or(CodeChunkError::InvalidPreparation)?;
        let size = code_size as usize;
        let bytes = bytecode.original_byte_slice();
        if !(LEGACY_CODE_CHUNK_SIZE + 1..=MAX_CODE_SIZE).contains(&size)
            || start >= size
            || layout.payload_len(bytes) != (size - start).min(CODE_CHUNK_SIZE)
            || (index == 0 && layout.start_offset != 0)
        {
            return Err(CodeChunkError::InvalidPreparation);
        }
        Ok(Self(CodeChunkKind::Chunked(ChunkedCodeChunk { bytecode, code_size, index })))
    }

    /// Chunked runtime buffer and its logical context, absent for complete code.
    pub const fn chunked(&self) -> Option<&ChunkedCodeChunk> {
        match self {
            Self(CodeChunkKind::Direct(_) | CodeChunkKind::Legacy(_)) => None,
            Self(CodeChunkKind::Chunked(chunk)) => Some(chunk),
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
            Self(CodeChunkKind::Direct(code) | CodeChunkKind::Legacy(code)) => code,
            Self(CodeChunkKind::Chunked(chunk)) => &chunk.bytecode,
        }
    }

    /// Logical chunk index; ordinary unchunked code starts at zero.
    pub const fn index(&self) -> u32 {
        match self {
            Self(CodeChunkKind::Direct(_) | CodeChunkKind::Legacy(_)) => 0,
            Self(CodeChunkKind::Chunked(chunk)) => chunk.index,
        }
    }

    /// Complete logical code length, excluding chunk padding and tails.
    pub fn code_size(&self) -> usize {
        match self {
            Self(CodeChunkKind::Direct(code) | CodeChunkKind::Legacy(code)) => code.len(),
            Self(CodeChunkKind::Chunked(chunk)) => chunk.code_size as usize,
        }
    }

    /// Whether this code was loaded from an account and uses account gas rules.
    pub const fn is_account(&self) -> bool {
        matches!(self.0, CodeChunkKind::Legacy(_) | CodeChunkKind::Chunked(_))
    }

    /// Whether this is complete code executed without account loading.
    pub const fn is_direct(&self) -> bool {
        matches!(self.0, CodeChunkKind::Direct(_))
    }

    /// Whether this is a complete legacy account record.
    pub const fn is_legacy(&self) -> bool {
        matches!(self.0, CodeChunkKind::Legacy(_))
    }

    pub(crate) const fn kind(&self) -> &CodeChunkKind {
        &self.0
    }

    /// Checks the duplicated immediate bytes shared with the following chunk.
    pub(crate) fn continues_into(&self, next: &Self, overlap: usize) -> bool {
        let Some(active) = self.chunked() else { return false };
        let Some(next) = next.chunked() else { return false };
        overlap == next.start_offset()
            && active.index + 1 == next.index
            && active.code_size == next.code_size
            && self
                .bytes()
                .get(active.payload_len()..active.payload_len() + overlap)
                .zip(next.bytecode.original_byte_slice().get(..overlap))
                .is_some_and(|(current, following)| current == following)
    }
}

/// Validated commitments present only for multi-chunk code.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct CodeMetadata {
    code_size: u32,
    chunk_hashes: Vec<B256>,
}

impl CodeMetadata {
    /// Validates the complete logical size and ordered hash count.
    /// Payload authentication belongs to ingestion.
    pub fn new(code_size: u32, chunk_hashes: Vec<B256>) -> Result<Self, CodeChunkError> {
        validate_metadata(code_size, chunk_hashes.len())?;
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
                self.code_size as usize - index * CODE_CHUNK_SIZE
            } else {
                CODE_CHUNK_SIZE
            }
        })
    }

    /// Committed original payload length of the last chunk.
    pub const fn last_chunk_size(&self) -> u32 {
        ((self.code_size - 1) % CODE_CHUNK_SIZE as u32) + 1
    }
}

fn validate_metadata(code_size: u32, chunk_count: usize) -> Result<(), CodeChunkError> {
    let size = code_size as usize;
    if !(LEGACY_CODE_CHUNK_SIZE + 1..=MAX_CODE_SIZE).contains(&size)
        || chunk_count != size.div_ceil(CODE_CHUNK_SIZE)
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
    let mut scan = InstructionScan::default();
    for index in 0..count {
        let chunk = prepare_code_chunk(code, index, &mut scan)?;
        hashes.push(chunk.as_bytecode().hash_slow());
        chunks.push(chunk);
    }
    let code_size = code.len().try_into().map_err(|_| CodeChunkError::CodeTooLarge)?;
    Ok(Some((CodeMetadata::new(code_size, hashes)?, chunks)))
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
    let mut scan = InstructionScan::default();
    while scan.pc < start {
        scan.instruction_start = scan.pc;
        scan.pc += instruction_len(code[scan.pc]);
    }
    prepare_code_chunk(code, index as usize, &mut scan).ok()
}

fn prepare_code_chunk(
    code: &Bytes,
    index: usize,
    scan: &mut InstructionScan,
) -> Result<CodeChunk, CodeChunkError> {
    let start = index * CODE_CHUNK_SIZE;
    let end = (start + CODE_CHUNK_SIZE).min(code.len());
    let leading = scan.pc.min(end).saturating_sub(start);
    let jumpdest_offset = if leading > 0
        && code
            .get(scan.instruction_start)
            .is_some_and(|opcode| (op::PUSH1..=op::PUSH32).contains(opcode))
    {
        leading
    } else {
        0
    };
    scan.pc = scan.pc.min(end);
    while scan.pc < end {
        scan.instruction_start = scan.pc;
        scan.pc += instruction_len(code[scan.pc]);
    }
    let mut bytes = Vec::with_capacity(end - start + 33);
    bytes.extend_from_slice(&code[start..scan.pc.min(code.len())]);
    bytes.resize(scan.pc - start, 0);
    // A boundary-straddling instruction reads its immediate here. The dispatcher switches
    // chunks before any padding can execute as code.
    bytes.resize(end - start + 32, 0);
    bytes.push(encode_chunk_offset(leading, jumpdest_offset)?);
    CodeChunk::from_chunked(
        Bytecode::new_chunk(bytes.into())?,
        code.len().try_into().map_err(|_| CodeChunkError::CodeTooLarge)?,
        index.try_into().map_err(|_| CodeChunkError::CodeTooLarge)?,
    )
}

#[derive(Clone, Copy, Debug, Default)]
struct InstructionScan {
    pc: usize,
    instruction_start: usize,
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
                let code_size =
                    seq.next_element()?.ok_or_else(|| S::Error::invalid_length(0, &self))?;
                let chunk_hashes =
                    seq.next_element()?.ok_or_else(|| S::Error::invalid_length(1, &self))?;
                CodeMetadata::new(code_size, chunk_hashes).map_err(S::Error::custom)
            }

            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let mut code_size = None;
                let mut chunk_hashes = None;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "code_size" => {
                            if code_size.replace(map.next_value()?).is_some() {
                                return Err(M::Error::duplicate_field("code_size"));
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
                    code_size.ok_or_else(|| M::Error::missing_field("code_size"))?,
                    chunk_hashes.ok_or_else(|| M::Error::missing_field("chunk_hashes"))?,
                )
                .map_err(M::Error::custom)
            }
        }

        deserializer.deserialize_struct(
            "CodeMetadata",
            &["code_size", "chunk_hashes"],
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
    fn start_offset(&self) -> usize {
        match self.bytecode.kind() {
            BytecodeKind::Chunk(layout) => layout.start_offset(),
            _ => 0,
        }
    }

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
    jumpdest_offset: u8,
}

impl ChunkLayout {
    /// Validates stored bytes before the ordinary in-memory safety padding is added.
    pub(super) fn decode(bytes: &[u8]) -> Result<Self, CodeChunkError> {
        let invalid = CodeChunkError::InvalidPreparation;
        if bytes.len() > LEGACY_CODE_CHUNK_SIZE {
            return Err(invalid);
        }
        let Some((&encoded_offset, code)) = bytes.split_last() else { return Err(invalid) };
        let (start_offset, jumpdest_offset) = decode_chunk_offset(encoded_offset)?;
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
        Ok(Self { start_offset, jumpdest_offset })
    }

    /// Number of original code positions represented by the buffer.
    pub(super) const fn payload_len(self, bytes: &[u8]) -> usize {
        bytes.len() - 33
    }

    /// Leading bytes continuing the previous chunk's immediate.
    pub const fn start_offset(self) -> usize {
        self.start_offset as usize
    }

    /// Leading PUSH continuation bytes excluded from legacy jump destination analysis.
    pub const fn jumpdest_offset(self) -> usize {
        self.jumpdest_offset as usize
    }
}

const fn encode_chunk_offset(
    start_offset: usize,
    jumpdest_offset: usize,
) -> Result<u8, CodeChunkError> {
    if start_offset > 32 || !matches!(jumpdest_offset, 0) && jumpdest_offset != start_offset {
        return Err(CodeChunkError::InvalidPreparation);
    }
    Ok(start_offset as u8 | if jumpdest_offset == 0 { 0 } else { 0x80 })
}

const fn decode_chunk_offset(encoded: u8) -> Result<(u8, u8), CodeChunkError> {
    let start_offset = encoded & 0x7f;
    if start_offset > 32 || encoded == 0x80 {
        return Err(CodeChunkError::InvalidPreparation);
    }
    let jumpdest_offset = if encoded & 0x80 == 0 { 0 } else { start_offset };
    Ok((start_offset, jumpdest_offset))
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

fn normalize_complete_code(code: &Bytecode) -> Bytecode {
    if !matches!(code.kind(), BytecodeKind::Chunk(_)) {
        return code.clone();
    }
    let bytes = code.original_bytes();
    Bytecode::new_raw_checked(bytes.clone()).unwrap_or_else(|_| Bytecode::new_legacy(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_boundary_is_24_kib() {
        for size in [CODE_CHUNK_SIZE, CODE_CHUNK_SIZE + 1, LEGACY_CODE_CHUNK_SIZE] {
            let code = Bytes::from(alloc::vec![op::STOP; size]);
            assert!(prepare_code_chunks(&code).unwrap().is_none());
            assert!(code_chunk(&code, 0).is_some_and(|chunk| chunk.is_legacy()));
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
    fn jump_analysis_skips_only_push_continuations() {
        let mut push = alloc::vec![op::STOP; LEGACY_CODE_CHUNK_SIZE + 1];
        push[CODE_CHUNK_SIZE - 1] = op::PUSH32;
        push[CODE_CHUNK_SIZE] = op::JUMPDEST;
        let push_chunk = code_chunk(&Bytes::from(push), 1).unwrap();
        assert_eq!(push_chunk.start_offset(), 32);
        assert!(!push_chunk.is_valid_jumpdest(0));

        let mut rjump = alloc::vec![op::STOP; LEGACY_CODE_CHUNK_SIZE + 1];
        rjump[CODE_CHUNK_SIZE - 1] = op::RJUMP;
        rjump[CODE_CHUNK_SIZE] = op::JUMPDEST;
        let rjump_chunk = code_chunk(&Bytes::from(rjump), 1).unwrap();
        assert_eq!(rjump_chunk.start_offset(), 2);
        assert!(rjump_chunk.is_valid_jumpdest(0));
    }

    #[test]
    fn chunk_layout_rejects_noncanonical_push_flag_without_continuation() {
        let mut bytes = alloc::vec![0; 34];
        *bytes.last_mut().unwrap() = 0x80;
        assert_eq!(Bytecode::new_chunk(bytes.into()), Err(CodeChunkError::InvalidPreparation));
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
        for (code_size, count) in [
            (0, 2),
            (LEGACY_CODE_CHUNK_SIZE as u32, 2),
            (u32::MAX, 2),
            (LEGACY_CODE_CHUNK_SIZE as u32 + 1, 0),
            (LEGACY_CODE_CHUNK_SIZE as u32 + 1, 1),
            (2 * CODE_CHUNK_SIZE as u32 + 1, 2),
            (MAX_CODE_SIZE as u32, MAX_CODE_CHUNKS + 1),
        ] {
            assert_eq!(
                CodeMetadata::new(code_size, alloc::vec![B256::ZERO; count]),
                Err(CodeChunkError::InvalidMetadata)
            );
        }
    }

    #[test]
    #[cfg(feature = "serde")]
    fn metadata_serde_code_size() {
        let code_size = 2 * CODE_CHUNK_SIZE as u32 + 7;
        let metadata = CodeMetadata::new(code_size, alloc::vec![B256::ZERO; 3]).unwrap();
        let json = serde_json::json!({
            "code_size": code_size,
            "chunk_hashes": [B256::ZERO, B256::ZERO, B256::ZERO],
        });
        assert_eq!(serde_json::to_value(&metadata).unwrap(), json);
        assert_eq!(serde_json::from_value::<CodeMetadata>(json).unwrap(), metadata);
        let encoded = postcard::to_allocvec(&metadata).unwrap();
        assert_eq!(postcard::from_bytes::<CodeMetadata>(&encoded).unwrap(), metadata);
        for code_size in [0, CODE_CHUNK_SIZE as u32 + 1] {
            let json = serde_json::json!({
                "code_size": code_size,
                "chunk_hashes": [B256::ZERO, B256::ZERO],
            });
            assert!(serde_json::from_value::<CodeMetadata>(json).is_err());
        }
    }
}
