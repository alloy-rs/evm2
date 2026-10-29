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
use alloy_primitives::{B256, Bytes, keccak256};
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
#[derive(Clone, Debug, Default)]
pub struct CodeChunk {
    bytecode: Bytecode,
    /// Complete account code size, absent for directly executed code such as initcode.
    code_size: Option<usize>,
    index: u32,
}

impl PartialEq for CodeChunk {
    fn eq(&self, other: &Self) -> bool {
        self.bytecode == other.bytecode
            && self.bytecode.kind() == other.bytecode.kind()
            && self.code_size == other.code_size
            && self.index == other.index
    }
}

impl Eq for CodeChunk {}

impl From<Bytecode> for CodeChunk {
    fn from(code: Bytecode) -> Self {
        Self { bytecode: code.normalize_complete(), code_size: None, index: 0 }
    }
}

impl CodeChunk {
    /// Wraps legacy account code without interpreting a delegation-shaped prefix.
    pub fn new_legacy(bytes: Bytes) -> Self {
        let bytecode = Bytecode::new_legacy(bytes);
        let code_size = Some(bytecode.len());
        Self { bytecode, code_size, index: 0 }
    }

    /// Retains a legacy account's bytecode allocation and its shared analysis.
    pub fn from_legacy_bytecode(code: &Bytecode) -> Self {
        let bytecode = code.normalize_complete();
        let code_size = Some(bytecode.len());
        Self { bytecode, code_size, index: 0 }
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
        if self.is_chunked() { chunk_payload_len(self.bytes()) } else { self.bytecode.len() }
    }

    /// Number of leading immediate bytes belonging to the previous chunk's instruction.
    /// The count is the last stored byte; ordinary legacy records have no trailer.
    pub fn start_offset(&self) -> usize {
        if self.is_chunked() { chunk_start_offset(self.bytes()) } else { 0 }
    }

    /// Attaches account context to a provider's validated, shared chunk bytecode.
    pub fn from_chunked(
        bytecode: Bytecode,
        code_size: u32,
        index: u32,
    ) -> Result<Self, CodeChunkError> {
        let bytecode = match bytecode.kind() {
            BytecodeKind::Chunk => bytecode,
            _ => Bytecode::new_chunk(bytecode.original_bytes())?,
        };
        let start = (index as usize)
            .checked_mul(CODE_CHUNK_SIZE)
            .ok_or(CodeChunkError::InvalidPreparation)?;
        let size = code_size as usize;
        let bytes = bytecode.original_byte_slice();
        if !(LEGACY_CODE_CHUNK_SIZE + 1..=MAX_CODE_SIZE).contains(&size)
            || start >= size
            || chunk_payload_len(bytes) != (size - start).min(CODE_CHUNK_SIZE)
            || (index == 0 && chunk_start_offset(bytes) != 0)
        {
            return Err(CodeChunkError::InvalidPreparation);
        }
        Ok(Self { bytecode, code_size: Some(size), index })
    }

    /// Whether this is one slice of a multi-chunk account runtime.
    pub fn is_chunked(&self) -> bool {
        self.is_account() && matches!(self.bytecode.kind(), BytecodeKind::Chunk)
    }

    /// Checks the loaded buffer's logical size and index against the account.
    pub fn validate_context(&self, code_size: u32, index: u32) -> Result<(), CodeChunkError> {
        if self.is_chunked() && self.code_size == Some(code_size as usize) && self.index == index {
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
        &self.bytecode
    }

    /// Logical chunk index; ordinary unchunked code starts at zero.
    pub const fn index(&self) -> u32 {
        self.index
    }

    /// Complete logical code length, excluding chunk padding and tails.
    pub fn code_size(&self) -> usize {
        self.code_size.unwrap_or_else(|| self.bytecode.len())
    }

    /// Whether this code was loaded from an account and uses account gas rules.
    pub const fn is_account(&self) -> bool {
        self.code_size.is_some()
    }

    /// Whether this is complete code executed without account loading.
    pub const fn is_direct(&self) -> bool {
        self.code_size.is_none()
    }

    /// Whether this is a complete legacy account record.
    pub fn is_legacy(&self) -> bool {
        self.is_account() && !self.is_chunked()
    }

    /// Checks the duplicated immediate bytes shared with the following chunk.
    pub(crate) fn continues_into(&self, next: &Self, overlap: usize) -> bool {
        let Some(code_size) = self.code_size.filter(|_| self.is_chunked()) else { return false };
        let Some(next_code_size) = next.code_size.filter(|_| next.is_chunked()) else {
            return false;
        };
        overlap == next.start_offset()
            && self.index + 1 == next.index
            && code_size == next_code_size
            && self
                .bytes()
                .get(self.payload_len()..self.payload_len() + overlap)
                .zip(next.bytecode.original_byte_slice().get(..overlap))
                .is_some_and(|(current, following)| current == following)
    }
}

/// Immutable layout record stored under the commitment to its ordered chunk hashes.
///
/// Legacy code has no chunk hashes. Chunked code commits to each prepared stored buffer.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct CodeManifest {
    code_size: u32,
    chunk_hashes: Vec<B256>,
}

impl CodeManifest {
    /// Validates the complete logical size and ordered chunk commitments.
    pub fn new(code_size: u32, chunk_hashes: Vec<B256>) -> Result<Self, CodeChunkError> {
        validate_manifest(code_size, chunk_hashes.len())?;
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

    /// Account code hash for chunked code: the hash of the ordered chunk hashes.
    pub fn code_hash(&self) -> Option<B256> {
        self.is_chunked().then(|| {
            let mut bytes = Vec::with_capacity(self.chunk_hashes.len() * B256::len_bytes());
            for hash in &self.chunk_hashes {
                bytes.extend_from_slice(hash.as_slice());
            }
            keccak256(bytes)
        })
    }

    /// Encodes this chunked manifest for storage through the bytecode database API.
    pub fn bytecode(&self) -> Option<Bytecode> {
        self.is_chunked().then(|| {
            let mut bytes = Vec::with_capacity(4 + self.chunk_hashes.len() * B256::len_bytes());
            bytes.extend_from_slice(&self.code_size.to_be_bytes());
            for hash in &self.chunk_hashes {
                bytes.extend_from_slice(hash.as_slice());
            }
            Bytecode::new_legacy(bytes.into())
        })
    }

    /// Decodes a chunked manifest stored through the bytecode database API.
    pub fn from_bytecode(code: &Bytecode) -> Result<Self, CodeChunkError> {
        let bytes = code.original_byte_slice();
        let hashes = bytes.get(4..).ok_or(CodeChunkError::InvalidManifest)?;
        if hashes.is_empty() || hashes.len() % B256::len_bytes() != 0 {
            return Err(CodeChunkError::InvalidManifest);
        }
        let code_size =
            u32::from_be_bytes(bytes[..4].try_into().map_err(|_| CodeChunkError::InvalidManifest)?);
        let chunk_hashes = hashes.as_chunks::<32>().0.iter().copied().map(B256::from).collect();
        Self::new(code_size, chunk_hashes)
    }

    /// Number of committed chunks.
    pub const fn chunk_count(&self) -> usize {
        self.chunk_hashes.len()
    }

    /// Whether this code uses prepared chunk records.
    pub const fn is_chunked(&self) -> bool {
        !self.chunk_hashes.is_empty()
    }

    /// Hash at the given logical chunk index.
    pub fn chunk_hash(&self, index: u32) -> Option<B256> {
        self.chunk_hashes.get(index as usize).copied()
    }

    /// Exact payload length for an in-range index, without provider access.
    pub fn chunk_len(&self, index: u32) -> Option<usize> {
        if !self.is_chunked() {
            return (index == 0).then_some(self.code_size as usize);
        }
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
        if self.is_chunked() {
            ((self.code_size - 1) % CODE_CHUNK_SIZE as u32) + 1
        } else {
            self.code_size
        }
    }
}

const fn validate_manifest(code_size: u32, chunk_count: usize) -> Result<(), CodeChunkError> {
    let size = code_size as usize;
    let valid = if size <= LEGACY_CODE_CHUNK_SIZE {
        chunk_count == 0
    } else {
        size <= MAX_CODE_SIZE && chunk_count == size.div_ceil(CODE_CHUNK_SIZE)
    };
    if !valid {
        return Err(CodeChunkError::InvalidManifest);
    }
    Ok(())
}

/// Deployment or metadata validation failure.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum CodeChunkError {
    /// Runtime exceeds the draft capacity.
    #[error("runtime exceeds the TIP-1143 code size limit")]
    CodeTooLarge,
    /// Size and commitments do not describe the code layout.
    #[error("invalid code manifest")]
    InvalidManifest,
    /// Provider preparation violates the bounded instruction layout.
    #[error("invalid chunk preparation")]
    InvalidPreparation,
}

/// Builds the immutable manifest and any prepared chunk records in one instruction scan.
pub fn build_code_artifacts(
    code: &Bytes,
) -> Result<(CodeManifest, Vec<CodeChunk>), CodeChunkError> {
    if code.len() > MAX_CODE_SIZE {
        return Err(CodeChunkError::CodeTooLarge);
    }
    let code_size = code.len().try_into().map_err(|_| CodeChunkError::CodeTooLarge)?;
    if code.len() <= LEGACY_CODE_CHUNK_SIZE {
        return Ok((CodeManifest::new(code_size, Vec::new())?, Vec::new()));
    }
    let count = code.len().div_ceil(CODE_CHUNK_SIZE);
    let mut chunks = Vec::with_capacity(count);
    let mut hashes = Vec::with_capacity(count);
    let mut instruction_pc = 0;
    for index in 0..count {
        let chunk = prepare_code_chunk(code, index, &mut instruction_pc)?;
        hashes.push(chunk.as_bytecode().hash_slow());
        chunks.push(chunk);
    }
    Ok((CodeManifest::new(code_size, hashes)?, chunks))
}

#[cfg(test)]
pub(crate) fn code_chunk(code: &Bytes, index: u32) -> Option<CodeChunk> {
    if code.len() <= LEGACY_CODE_CHUNK_SIZE {
        return (index == 0).then(|| CodeChunk::new_legacy(code.clone()));
    }
    build_code_artifacts(code).ok()?.1.into_iter().nth(index as usize)
}

fn prepare_code_chunk(
    code: &Bytes,
    index: usize,
    instruction_pc: &mut usize,
) -> Result<CodeChunk, CodeChunkError> {
    let start = index * CODE_CHUNK_SIZE;
    let end = (start + CODE_CHUNK_SIZE).min(code.len());
    let leading = (*instruction_pc).min(end).saturating_sub(start);
    *instruction_pc = (*instruction_pc).min(end);
    while *instruction_pc < end {
        *instruction_pc += instruction_len(code[*instruction_pc]);
    }
    let mut bytes = Vec::with_capacity(end - start + 33);
    bytes.extend_from_slice(&code[start..(*instruction_pc).min(code.len())]);
    bytes.resize(*instruction_pc - start, 0);
    // A boundary-straddling instruction reads its immediate here. The dispatcher switches
    // chunks before any padding can execute as code.
    bytes.resize(end - start + 32, 0);
    bytes.push(encode_chunk_offset(leading)?);
    CodeChunk::from_chunked(
        Bytecode::new_chunk(bytes.into())?,
        code.len().try_into().map_err(|_| CodeChunkError::CodeTooLarge)?,
        index.try_into().map_err(|_| CodeChunkError::CodeTooLarge)?,
    )
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for CodeManifest {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ManifestVisitor;

        impl<'de> Visitor<'de> for ManifestVisitor {
            type Value = CodeManifest;

            fn expecting(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                formatter.write_str("code manifest")
            }

            fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> Result<Self::Value, S::Error> {
                let code_size =
                    seq.next_element()?.ok_or_else(|| S::Error::invalid_length(0, &self))?;
                let chunk_hashes =
                    seq.next_element()?.ok_or_else(|| S::Error::invalid_length(1, &self))?;
                CodeManifest::new(code_size, chunk_hashes).map_err(S::Error::custom)
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
                CodeManifest::new(
                    code_size.ok_or_else(|| M::Error::missing_field("code_size"))?,
                    chunk_hashes.ok_or_else(|| M::Error::missing_field("chunk_hashes"))?,
                )
                .map_err(M::Error::custom)
            }
        }

        deserializer.deserialize_struct(
            "CodeManifest",
            &["code_size", "chunk_hashes"],
            ManifestVisitor,
        )
    }
}

/// Validates stored bytes before the ordinary in-memory safety padding is added.
pub(super) fn validate_chunk(bytes: &[u8]) -> Result<(), CodeChunkError> {
    let invalid = CodeChunkError::InvalidPreparation;
    if bytes.len() > LEGACY_CODE_CHUNK_SIZE {
        return Err(invalid);
    }
    let Some((&encoded_offset, code)) = bytes.split_last() else { return Err(invalid) };
    let start_offset = decode_chunk_offset(encoded_offset)?;
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
    Ok(())
}

/// Number of original code positions represented by a validated stored chunk.
pub(super) const fn chunk_payload_len(bytes: &[u8]) -> usize {
    bytes.len() - 33
}

/// Leading bytes continuing the previous chunk's instruction.
pub(super) const fn chunk_start_offset(bytes: &[u8]) -> usize {
    bytes[bytes.len() - 1] as usize
}

const fn encode_chunk_offset(start_offset: usize) -> Result<u8, CodeChunkError> {
    if start_offset > 32 {
        return Err(CodeChunkError::InvalidPreparation);
    }
    Ok(start_offset as u8)
}

const fn decode_chunk_offset(encoded: u8) -> Result<u8, CodeChunkError> {
    if encoded > 32 {
        return Err(CodeChunkError::InvalidPreparation);
    }
    Ok(encoded)
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
            let (manifest, chunks) = build_code_artifacts(&code).unwrap();
            assert_eq!(manifest.code_size() as usize, size);
            assert!(!manifest.is_chunked());
            assert!(chunks.is_empty());
        }
        let code = Bytes::from(alloc::vec![op::STOP; LEGACY_CODE_CHUNK_SIZE + 1]);
        assert_eq!(build_code_artifacts(&code).unwrap().0.chunk_count(), 2);
    }

    #[test]
    fn chunked_code_hash_commits_to_ordered_chunk_hashes() {
        let code = Bytes::from(alloc::vec![op::STOP; LEGACY_CODE_CHUNK_SIZE + 1]);
        let (manifest, _) = build_code_artifacts(&code).unwrap();
        let concatenated = manifest
            .chunk_hashes()
            .iter()
            .flat_map(|hash| hash.as_slice())
            .copied()
            .collect::<Vec<_>>();

        assert_eq!(manifest.code_hash(), Some(keccak256(concatenated)));
        assert_ne!(manifest.code_hash(), Some(keccak256(code)));
    }

    #[test]
    fn streaming_preparation_matches_individual_chunks_across_immediates() {
        let mut code = alloc::vec![op::STOP; 3 * CODE_CHUNK_SIZE + 37];
        code[CODE_CHUNK_SIZE - 1] = op::PUSH32;
        code[2 * CODE_CHUNK_SIZE - 2] = op::RJUMP;
        code[3 * CODE_CHUNK_SIZE - 1] = op::DUPN;
        let code = Bytes::from(code);
        let (manifest, chunks) = build_code_artifacts(&code).unwrap();

        assert_eq!(chunks.len(), 4);
        assert_eq!(chunks[1].start_offset(), 32);
        assert_eq!(chunks[2].start_offset(), 1);
        assert_eq!(chunks[3].start_offset(), 1);
        for (index, chunk) in chunks.iter().enumerate() {
            assert_eq!(manifest.chunk_hashes()[index], chunk.as_bytecode().hash_slow());
            assert_eq!(chunk.payload_len(), manifest.chunk_len(index as u32).unwrap());
        }
    }

    #[test]
    fn jump_analysis_skips_all_continuations() {
        let mut push = alloc::vec![op::STOP; LEGACY_CODE_CHUNK_SIZE + 1];
        push[CODE_CHUNK_SIZE - 1] = op::PUSH32;
        push[CODE_CHUNK_SIZE] = op::JUMPDEST;
        let push_chunk = build_code_artifacts(&Bytes::from(push)).unwrap().1.remove(1);
        assert_eq!(push_chunk.start_offset(), 32);
        assert!(!push_chunk.is_valid_jumpdest(0));

        let mut rjump = alloc::vec![op::STOP; LEGACY_CODE_CHUNK_SIZE + 1];
        rjump[CODE_CHUNK_SIZE - 1] = op::RJUMP;
        rjump[CODE_CHUNK_SIZE] = op::JUMPDEST;
        let rjump_chunk = build_code_artifacts(&Bytes::from(rjump)).unwrap().1.remove(1);
        assert_eq!(rjump_chunk.start_offset(), 2);
        assert!(!rjump_chunk.is_valid_jumpdest(0));
    }

    #[test]
    fn chunk_layout_rejects_offset_above_instruction_limit() {
        let mut bytes = alloc::vec![0; 34];
        *bytes.last_mut().unwrap() = 33;
        assert_eq!(Bytecode::new_chunk(bytes.into()), Err(CodeChunkError::InvalidPreparation));
    }

    #[test]
    fn manifest_last_chunk_size() {
        for size in [
            LEGACY_CODE_CHUNK_SIZE + 1,
            2 * CODE_CHUNK_SIZE,
            2 * CODE_CHUNK_SIZE + 1,
            MAX_CODE_SIZE,
        ] {
            let manifest =
                build_code_artifacts(&Bytes::from(alloc::vec![op::STOP; size])).unwrap().0;
            let count = size.div_ceil(CODE_CHUNK_SIZE);
            let last_chunk_size = size - (count - 1) * CODE_CHUNK_SIZE;
            assert_eq!(manifest.code_size() as usize, size);
            assert_eq!(manifest.chunk_hashes().len(), count);
            assert_eq!(manifest.last_chunk_size() as usize, last_chunk_size);
            for index in 0..count - 1 {
                assert_eq!(manifest.chunk_len(index as u32), Some(CODE_CHUNK_SIZE));
            }
            assert_eq!(manifest.chunk_len(count as u32 - 1), Some(last_chunk_size));
            assert_eq!(manifest.chunk_len(count as u32), None);
            assert_eq!(manifest.chunk_len(u32::MAX), None);
        }
    }

    #[test]
    fn manifest_rejects_invalid_size_and_count() {
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
                CodeManifest::new(code_size, alloc::vec![B256::ZERO; count]),
                Err(CodeChunkError::InvalidManifest)
            );
        }
    }

    #[test]
    fn manifest_bytecode_roundtrip() {
        let code_size = 2 * CODE_CHUNK_SIZE as u32 + 7;
        let manifest = CodeManifest::new(
            code_size,
            alloc::vec![B256::repeat_byte(1), B256::repeat_byte(2), B256::repeat_byte(3)],
        )
        .unwrap();
        let bytecode = manifest.bytecode().unwrap();

        assert_eq!(CodeManifest::from_bytecode(&bytecode), Ok(manifest));
        assert_eq!(bytecode.len(), 4 + 3 * B256::len_bytes());
    }

    #[test]
    #[cfg(feature = "serde")]
    fn manifest_serde_code_size() {
        let code_size = 2 * CODE_CHUNK_SIZE as u32 + 7;
        let manifest = CodeManifest::new(code_size, alloc::vec![B256::ZERO; 3]).unwrap();
        let json = serde_json::json!({
            "code_size": code_size,
            "chunk_hashes": [B256::ZERO, B256::ZERO, B256::ZERO],
        });
        assert_eq!(serde_json::to_value(&manifest).unwrap(), json);
        assert_eq!(serde_json::from_value::<CodeManifest>(json).unwrap(), manifest);
        let encoded = postcard::to_allocvec(&manifest).unwrap();
        assert_eq!(postcard::from_bytes::<CodeManifest>(&encoded).unwrap(), manifest);
        for code_size in [0, CODE_CHUNK_SIZE as u32 + 1] {
            let json = serde_json::json!({
                "code_size": code_size,
                "chunk_hashes": [B256::ZERO, B256::ZERO],
            });
            assert!(serde_json::from_value::<CodeManifest>(json).is_err());
        }
    }
}
