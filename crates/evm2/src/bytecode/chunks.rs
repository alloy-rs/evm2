//! Original TIP-1143 payloads, commitments, and deployment validation.

use super::{Bytecode, BytecodeKind, JumpTable};
use crate::{
    interpreter::{instructions::encode_rjump_offset, op},
    once_lock::OnceLock,
};
use alloc::{sync::Arc, vec, vec::Vec};
use alloy_primitives::{B256, Bytes, keccak256};
use thiserror::Error;

/// Original bytes in a full runtime chunk.
pub const CODE_CHUNK_SIZE: usize = 24 * 1024 - 35;
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

/// An original provider payload with optional execution preparation and cached analysis.
///
/// Construction deliberately does not authenticate bytes. Ingestion authenticates hashes;
/// execution checks the payload length against the requesting account's metadata.
#[derive(Clone, Debug)]
pub struct CodeChunk {
    bytes: Bytes,
    kind: Option<BytecodeKind>,
    prepared: Option<PreparedCodeChunk>,
    analysis: Arc<[OnceLock<Bytecode>; 2]>,
}

impl Default for CodeChunk {
    fn default() -> Self {
        Self::from_bytecode(&Bytecode::default())
    }
}

impl From<Bytecode> for CodeChunk {
    fn from(code: Bytecode) -> Self {
        Self::from_bytecode(&code)
    }
}

impl CodeChunk {
    /// Wraps an original payload returned by a provider.
    pub fn new(bytes: Bytes) -> Self {
        Self {
            bytes,
            kind: None,
            prepared: None,
            analysis: Arc::new(core::array::from_fn(|_| OnceLock::new())),
        }
    }

    /// Returns original bytes, excluding any execution-only padding.
    pub const fn original_bytes(&self) -> &Bytes {
        &self.bytes
    }

    /// Preserves the kind of an existing single-chunk database record.
    pub fn from_bytecode(code: &Bytecode) -> Self {
        let chunk = Self {
            bytes: code.original_bytes(),
            kind: Some(code.kind()),
            prepared: None,
            analysis: Arc::new(core::array::from_fn(|_| OnceLock::new())),
        };
        let _ = chunk.analysis[0].set(code.clone());
        chunk
    }

    /// Returns the provider's known kind, or `None` for an unclassified payload.
    pub const fn kind(&self) -> Option<BytecodeKind> {
        self.kind
    }

    /// Builds an execution view. Multi-chunk payloads always use legacy analysis.
    /// Delegation is selected only by an explicit persisted kind, never by payload prefix.
    pub fn bytecode(&self, multi_chunk: bool) -> Bytecode {
        if self.prepared.is_some() {
            return self.execution_view(0).bytecode;
        }
        self.analysis[usize::from(multi_chunk)]
            .get_or_init(|| {
                if !multi_chunk
                    && self.kind == Some(BytecodeKind::Eip7702)
                    && let Ok(code) = Bytecode::new_eip7702_raw(self.bytes.clone())
                {
                    return code;
                }
                Bytecode::new_legacy(self.bytes.clone())
            })
            .clone()
    }

    /// Restores bounded preparation previously authenticated against complete original code.
    pub fn with_preparation(
        bytes: Bytes,
        code_size: u32,
        index: u32,
        leading_data_len: u8,
        jump_data_len: u8,
        lookahead: Bytes,
    ) -> Result<Self, CodeChunkError> {
        let start = (index as usize)
            .checked_mul(CODE_CHUNK_SIZE)
            .ok_or(CodeChunkError::InvalidPreparation)?;
        let size = code_size as usize;
        if !(CODE_CHUNK_SIZE + 1..=MAX_CODE_SIZE).contains(&size)
            || start >= size
            || bytes.len() != (size - start).min(CODE_CHUNK_SIZE)
            || usize::from(leading_data_len) > bytes.len().min(32)
            || usize::from(jump_data_len) > bytes.len().min(32)
            || (index == 0 && (leading_data_len != 0 || jump_data_len != 0))
            || lookahead.len() != (size - start - bytes.len()).min(32)
        {
            return Err(CodeChunkError::InvalidPreparation);
        }
        let mut pc = usize::from(leading_data_len);
        while pc < bytes.len() {
            pc += instruction_len(bytes[pc]);
        }
        let spill = pc - bytes.len();
        let mut continuation = lookahead[..spill.min(lookahead.len())].to_vec();
        continuation.resize(spill, 0);
        let next_chunk = (start + pc < size).then_some(index + 1);
        Ok(Self {
            bytes,
            kind: Some(BytecodeKind::Legacy),
            analysis: Arc::new(core::array::from_fn(|_| OnceLock::new())),
            prepared: Some(PreparedCodeChunk {
                code_size,
                index,
                leading_data_len,
                jump_data_len,
                lookahead,
                continuation: continuation.into(),
                next_chunk,
                cache: Arc::new(OnceLock::new()),
            }),
        })
    }

    /// Authenticated execution layout, absent on unchanged legacy records.
    pub const fn prepared(&self) -> Option<&PreparedCodeChunk> {
        self.prepared.as_ref()
    }

    /// Checks preparation against the requesting account's logical size and index.
    pub fn validate_context(&self, code_size: u32, index: u32) -> Result<(), CodeChunkError> {
        if self.prepared.as_ref().is_some_and(|p| p.code_size == code_size && p.index == index) {
            Ok(())
        } else {
            Err(CodeChunkError::InvalidPreparation)
        }
    }

    /// Complete original jump map, including explicitly valid replacement prefix positions.
    /// Unlike a single execution view's safe map, this includes every entry variant.
    pub fn is_valid_jumpdest(&self, local_pc: usize) -> bool {
        if let Some(prepared) = &self.prepared {
            self.execution_layouts(prepared).jumps.is_valid(local_pc)
        } else {
            self.bytecode(false).legacy_jump_table().is_some_and(|map| map.is_valid(local_pc))
        }
    }

    /// Selects a bounded execution view for a validated entry offset.
    /// Only interpreter activation may consume a nonzero-entry view: it must start at that
    /// exact offset and replace the view on every taken jump with a different tail alignment.
    /// Each of at most 33 possible tails is materialized only once per immutable chunk.
    pub(crate) fn execution_view(&self, entry: usize) -> PreparedExecutionView {
        let Some(prepared) = &self.prepared else {
            return PreparedExecutionView {
                bytecode: self.bytecode(false),
                tail_offset: self.bytes.len(),
            };
        };
        assert!(entry < self.bytes.len(), "execution entry is outside original chunk");
        let layouts = self.execution_layouts(prepared);
        let spill = usize::from(layouts.tails[entry]);
        let tail_offset = self.bytes.len() + spill;
        let next_offset = (prepared.index as usize * CODE_CHUNK_SIZE + tail_offset
            < prepared.code_size as usize)
            .then_some(spill);
        let bytecode = layouts.variants[spill]
            .get_or_init(|| {
                let mut bytes = self.bytes.to_vec();
                bytes[..usize::from(prepared.leading_data_len)].fill(0x5b);
                if usize::from(layouts.tails[0]) != spill {
                    bytes[0] = 0;
                }
                bytes.extend_from_slice(&prepared.lookahead[..spill.min(prepared.lookahead.len())]);
                bytes.resize(tail_offset, 0);
                if next_offset.is_some() {
                    // The next original instruction has the tail's global PC. RJUMP is
                    // three bytes long, so -3 reaches it in the successor payload.
                    bytes.push(op::RJUMP);
                    bytes.extend_from_slice(&encode_rjump_offset(-3).unwrap());
                } else {
                    bytes.push(0);
                }
                let mut map = JumpTable::new(self.bytes.len());
                for (pc, &overrun) in layouts.tails.iter().enumerate() {
                    if usize::from(overrun) == spill && layouts.jumps.is_valid(pc) {
                        map.as_mut_slice()[pc / 8] |= 1 << (pc % 8);
                    }
                }
                // SAFETY: offset zero either has this tail or is guarded by STOP. Every entry
                // admitted by this view's map has the same verified tail.
                // Every immediate is complete and execution ends at STOP or RJUMP.
                // TIP runtime validates against the full separate map before switching variants.
                unsafe { Bytecode::new_analyzed(bytes.into(), self.bytes.len(), map) }
            })
            .clone();
        PreparedExecutionView { bytecode, tail_offset }
    }

    fn execution_layouts<'a>(&self, prepared: &'a PreparedCodeChunk) -> &'a ExecutionLayouts {
        prepared.cache.get_or_init(|| {
            let mut jumps = JumpTable::new(self.bytes.len());
            let mut pc = usize::from(prepared.jump_data_len);
            while pc < self.bytes.len() {
                if self.bytes[pc] == 0x5b {
                    jumps.as_mut_slice()[pc / 8] |= 1 << (pc % 8);
                }
                pc += jump_instruction_len(self.bytes[pc]);
            }
            for pc in 0..usize::from(prepared.leading_data_len) {
                jumps.as_mut_slice()[pc / 8] |= 1 << (pc % 8);
            }
            let mut tails = vec![0u8; self.bytes.len()];
            for pc in (0..self.bytes.len()).rev() {
                let opcode =
                    if pc < usize::from(prepared.leading_data_len) { 0x5b } else { self.bytes[pc] };
                let next = pc + instruction_len(opcode);
                tails[pc] = if next < self.bytes.len() {
                    tails[next]
                } else {
                    (next - self.bytes.len()) as u8
                };
            }
            ExecutionLayouts { jumps, tails, variants: core::array::from_fn(|_| OnceLock::new()) }
        })
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

    /// Ordered original-payload commitments.
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

/// Validates original creation output and computes optional commitments without analysis.
pub fn code_metadata(code: &[u8]) -> Result<Option<CodeMetadata>, CodeChunkError> {
    validate_code(code)?;
    if code.len() <= CODE_CHUNK_SIZE {
        return Ok(None);
    }
    let size = u32::try_from(code.len()).map_err(|_| CodeChunkError::CodeTooLarge)?;
    CodeMetadata::new(size, code.chunks(CODE_CHUNK_SIZE).map(keccak256).collect()).map(Some)
}

/// Derives one original payload and its bounded preparation from authenticated full code.
pub fn code_chunk(code: &Bytes, index: u32) -> Option<CodeChunk> {
    let start = usize::try_from(index).ok()?.checked_mul(CODE_CHUNK_SIZE)?;
    if start >= code.len() {
        return None;
    }
    let end = start.checked_add(CODE_CHUNK_SIZE)?.min(code.len());
    if code.len() <= CODE_CHUNK_SIZE {
        return Some(CodeChunk::from_bytecode(&Bytecode::new_legacy(code.clone())));
    }
    let mut jump_pc = 0;
    while jump_pc < start {
        jump_pc += jump_instruction_len(code[jump_pc]);
    }
    let jump_data_len = jump_pc.min(end).saturating_sub(start) as u8;
    let mut pc = 0;
    while pc < start {
        pc += instruction_len(code[pc]);
    }
    let leading = pc.min(end).saturating_sub(start);
    CodeChunk::with_preparation(
        code.slice(start..end),
        code.len() as u32,
        index,
        leading as u8,
        jump_data_len,
        code.slice(end..(end + 32).min(code.len())),
    )
    .ok()
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

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[cfg(feature = "serde")]
    #[test]
    fn metadata_deserialization_validates_invariants() {
        let metadata = CodeMetadata::new(24577, vec![B256::ZERO; 2]).unwrap();
        let mut json = serde_json::to_value(&metadata).unwrap();
        assert_eq!(serde_json::from_value::<CodeMetadata>(json.clone()).unwrap(), metadata);
        json["code_size"] = 0.into();
        assert!(serde_json::from_value::<CodeMetadata>(json).is_err());
    }

    #[test]
    fn unclassified_payload_never_infers_delegation_from_prefix() {
        let marker = Bytecode::new_eip7702(alloy_primitives::Address::repeat_byte(3));
        let chunk = CodeChunk::new(marker.original_bytes());
        assert!(chunk.bytecode(false).is_legacy());
        assert_eq!(CodeChunk::from_bytecode(&marker).bytecode(false), marker);
        assert!(chunk.bytecode(true).is_legacy());
        let chunk = CodeChunk::new(Bytes::from_static(&[0xef, 1]));
        assert!(chunk.bytecode(false).is_legacy());
        assert!(chunk.bytecode(true).is_legacy());
    }

    #[test]
    fn chunk_clones_share_lazy_analysis_and_jump_tables() {
        let bytes = Bytes::from(vec![0x5b; CODE_CHUNK_SIZE + 1]);
        for chunk in [
            CodeChunk::new(Bytes::from_static(&[0x5b, 0])),
            CodeChunk::from_bytecode(&Bytecode::new_legacy(Bytes::from_static(&[0x5b, 0]))),
            code_chunk(&bytes, 0).unwrap(),
        ] {
            let cloned = chunk.clone();
            let first = chunk.bytecode(false);
            let second = cloned.bytecode(false);
            assert_eq!(first.bytes_slice().as_ptr(), second.bytes_slice().as_ptr());
            assert!(core::ptr::eq(
                first.legacy_jump_table().unwrap(),
                second.legacy_jump_table().unwrap(),
            ));
        }
    }

    #[test]
    fn prepares_all_crossing_push_widths_and_accepts_replacement_destinations() {
        for width in 1..=32usize {
            for before in 1..=width {
                let mut raw = vec![0; CODE_CHUNK_SIZE + 40];
                let push = CODE_CHUNK_SIZE - before;
                raw[push] = 0x5f + width as u8;
                for i in 1..=width {
                    raw[push + i] = i as u8;
                }
                let original = Bytes::from(raw);
                let first = code_chunk(&original, 0).unwrap();
                let second = code_chunk(&original, 1).unwrap();
                let leading = width + 1 - before;
                assert_eq!(first.prepared().unwrap().continuation().len(), leading);
                assert_eq!(second.prepared().unwrap().leading_data_len() as usize, leading);
                let first_view = first.bytecode(true);
                assert_eq!(
                    &first_view.bytes()[push + 1..push + 1 + width],
                    &original[push + 1..push + 1 + width]
                );
                let second_view = second.bytecode(true);
                for i in 0..leading {
                    assert_eq!(second_view.bytes()[i], 0x5b);
                    assert!(second_view.legacy_jump_table().unwrap().is_valid(i));
                }
                assert_eq!(first.original_bytes(), &original.slice(..CODE_CHUNK_SIZE));
                assert_eq!(second.original_bytes(), &original.slice(CODE_CHUNK_SIZE..));
                assert!(first_view.bytes().len() <= LEGACY_CODE_CHUNK_SIZE);
                assert_eq!(first.prepared().unwrap().next_chunk(), Some(1));
                assert_eq!(second.prepared().unwrap().next_chunk(), None);
            }
        }
    }

    #[test]
    fn final_continuation_chunk_and_truncated_push_have_distinct_views() {
        let mut raw = vec![0; CODE_CHUNK_SIZE + 3];
        raw[CODE_CHUNK_SIZE - 1] = 0x7f;
        raw[CODE_CHUNK_SIZE..].copy_from_slice(&[0x5b, 0x60, 0x56]);
        let original = Bytes::from(raw);
        let first = code_chunk(&original, 0).unwrap();
        let second = code_chunk(&original, 1).unwrap();
        assert_eq!(first.prepared().unwrap().continuation().len(), 32);
        assert_eq!(&first.prepared().unwrap().continuation()[..3], &[0x5b, 0x60, 0x56]);
        assert_eq!(first.prepared().unwrap().next_chunk(), None);
        assert_eq!(second.prepared().unwrap().leading_data_len(), 3);
        assert!(second.prepared().unwrap().continuation().is_empty());
        assert_eq!(&second.bytecode(true).bytes()[..4], &[0x5b, 0x5b, 0x5b, 0]);
    }

    #[test]
    fn capacity_and_prepared_layout_bounds() {
        for len in [0, 24540, 24541, 24542, 24575, 24576, 24577, MAX_CODE_SIZE] {
            let raw = Bytes::from(vec![0; len]);
            assert!(validate_code(&raw).is_ok());
            assert_eq!(code_metadata(&raw).unwrap().is_some(), len > CODE_CHUNK_SIZE);
        }
        assert_eq!(MAX_CODE_SIZE, 981640);
        assert_eq!(MAX_INITCODE_SIZE, 1966080);
        assert_eq!(validate_code(&vec![0; MAX_CODE_SIZE + 1]), Err(CodeChunkError::CodeTooLarge));
        assert!(
            CodeChunk::with_preparation(Bytes::from_static(&[0x60]), 0, 0, 0, 0, Bytes::new())
                .is_err()
        );
        assert!(
            CodeChunk::with_preparation(Bytes::from_static(&[0]), 24542, 40, 1, 1, Bytes::new())
                .is_err()
        );
    }

    #[test]
    fn prepared_map_preserves_global_push_analysis_across_extra_immediates() {
        let mut raw = vec![0; CODE_CHUNK_SIZE + 40];
        raw[CODE_CHUNK_SIZE - 5..CODE_CHUNK_SIZE].copy_from_slice(&[0xe6, 0x60, 0x7f, 0x5b, 0x7f]);
        raw[CODE_CHUNK_SIZE..CODE_CHUNK_SIZE + 32].fill(0x5b);
        let original = Bytes::from(raw);
        let reference = Bytecode::new_legacy(original.clone());
        let reference_map = reference.legacy_jump_table().unwrap();
        for index in 0..2 {
            let chunk = code_chunk(&original, index).unwrap();
            let leading = usize::from(chunk.prepared().unwrap().leading_data_len());
            for local in 0..chunk.original_bytes().len() {
                assert_eq!(
                    chunk.is_valid_jumpdest(local),
                    local < leading
                        || reference_map.is_valid(index as usize * CODE_CHUNK_SIZE + local)
                );
            }
        }
        let first = code_chunk(&original, 0).unwrap();
        assert!(first.is_valid_jumpdest(CODE_CHUNK_SIZE - 2));
    }

    // Cloning is intentional: prepared analyses must be shared across chunk handles.
    #[test]
    #[allow(clippy::redundant_clone)]
    fn alternative_entry_uses_its_own_safe_tail_and_reuses_analysis() {
        let mut raw = vec![0; CODE_CHUNK_SIZE + 40];
        raw[CODE_CHUNK_SIZE - 5..CODE_CHUNK_SIZE].copy_from_slice(&[0xe6, 0x60, 0x7f, 0x5b, 0x7f]);
        for (i, byte) in raw[CODE_CHUNK_SIZE..].iter_mut().enumerate() {
            *byte = i as u8;
        }
        let original = Bytes::from(raw);
        let chunk = code_chunk(&original, 0).unwrap();
        let canonical = chunk.execution_view(0);
        let alternate = chunk.execution_view(CODE_CHUNK_SIZE - 2);
        assert_eq!(canonical.tail_offset, CODE_CHUNK_SIZE + 30);
        assert_eq!(alternate.tail_offset, CODE_CHUNK_SIZE + 32);
        assert_eq!(alternate.bytecode.bytes()[0], 0);
        assert_eq!(&alternate.bytecode.bytes()[alternate.tail_offset..], &[op::RJUMP, 0x5a, 0x58]);
        assert!(chunk.is_valid_jumpdest(CODE_CHUNK_SIZE - 2));
        assert!(!canonical.bytecode.legacy_jump_table().unwrap().is_valid(CODE_CHUNK_SIZE - 2));
        assert!(alternate.bytecode.legacy_jump_table().unwrap().is_valid(CODE_CHUNK_SIZE - 2));
        assert_eq!(
            &alternate.bytecode.bytes()[CODE_CHUNK_SIZE..CODE_CHUNK_SIZE + 32],
            &original[CODE_CHUNK_SIZE..CODE_CHUNK_SIZE + 32]
        );
        assert_eq!(
            alternate.bytecode.bytes().as_ptr(),
            chunk.clone().execution_view(CODE_CHUNK_SIZE - 2).bytecode.bytes().as_ptr()
        );
        assert!(alternate.bytecode.bytes().len() <= LEGACY_CODE_CHUNK_SIZE);
    }

    #[test]
    fn every_tail_variant_is_bounded_and_knows_exact_logical_end() {
        for remaining in [3, 32, 33] {
            let mut raw = vec![0; CODE_CHUNK_SIZE + remaining];
            raw[CODE_CHUNK_SIZE - 33..CODE_CHUNK_SIZE].fill(0x7f);
            raw[CODE_CHUNK_SIZE..].fill(0xab);
            let original = Bytes::from(raw);
            let chunk = code_chunk(&original, 0).unwrap();
            for spill in 0..=32 {
                let entry = CODE_CHUNK_SIZE - 33 + spill;
                let view = chunk.execution_view(entry);
                assert_eq!(view.tail_offset, CODE_CHUNK_SIZE + spill);
                assert_eq!(
                    view.bytecode.bytes()[view.tail_offset],
                    if spill < remaining { op::RJUMP } else { op::STOP }
                );
                assert!(view.bytecode.bytes().len() <= LEGACY_CODE_CHUNK_SIZE);
                for immediate in 1..=32 {
                    assert_eq!(
                        view.bytecode.bytes()[entry + immediate],
                        original.get(entry + immediate).copied().unwrap_or(0)
                    );
                }
                assert_eq!(
                    view.bytecode.bytes().as_ptr(),
                    chunk.execution_view(entry).bytecode.bytes().as_ptr()
                );
            }
        }
    }

    #[test]
    fn ordinary_immediates_and_generated_tails_are_not_jump_targets() {
        let mut raw = vec![0; CODE_CHUNK_SIZE + 3];
        raw[..2].copy_from_slice(&[0x60, 0x5b]);
        raw[10..12].copy_from_slice(&[0xe6, 0x5b]);
        let chunk = code_chunk(&Bytes::from(raw), 0).unwrap();
        let view = chunk.bytecode(true);
        let map = view.legacy_jump_table().unwrap();
        assert!(!map.is_valid(1));
        assert!(map.is_valid(11)); // Existing PUSH-only jump analysis admits DUPN operand JUMPDEST.
        assert!(!map.is_valid(CODE_CHUNK_SIZE));
    }
}

/// Bounded execution-only layout authenticated at creation or ingestion.
#[derive(Clone, Debug)]
pub struct PreparedCodeChunk {
    code_size: u32,
    index: u32,
    lookahead: Bytes,
    cache: Arc<OnceLock<ExecutionLayouts>>,
    leading_data_len: u8,
    jump_data_len: u8,
    continuation: Bytes,
    next_chunk: Option<u32>,
}

impl PreparedCodeChunk {
    /// Prefix replaced with valid JUMPDEST instructions in the execution view.
    pub const fn leading_data_len(&self) -> u8 {
        self.leading_data_len
    }
    /// Prefix skipped by the original global PUSH-only jump destination analysis.
    pub const fn jump_data_len(&self) -> u8 {
        self.jump_data_len
    }
    /// Full bounded original lookahead shared by all possible entry variants.
    pub const fn lookahead(&self) -> &Bytes {
        &self.lookahead
    }
    /// Complete original size used to authenticate this preparation context.
    pub const fn code_size(&self) -> u32 {
        self.code_size
    }
    /// Logical chunk index used to authenticate this preparation context.
    pub const fn index(&self) -> u32 {
        self.index
    }
    /// Original immediate bytes continued after the nominal slice, including final zero padding.
    pub const fn continuation(&self) -> &Bytes {
        &self.continuation
    }
    /// Next logical chunk for an internal transfer, or final STOP.
    pub const fn next_chunk(&self) -> Option<u32> {
        self.next_chunk
    }
    /// Local offset of RJUMP or final STOP.
    pub fn tail_offset(&self, payload_len: usize) -> usize {
        payload_len + self.continuation.len()
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

impl PartialEq for CodeChunk {
    fn eq(&self, other: &Self) -> bool {
        self.bytes == other.bytes && self.kind == other.kind && self.prepared == other.prepared
    }
}

impl Eq for CodeChunk {}

fn jump_instruction_len(opcode: u8) -> usize {
    if (0x60..=0x7f).contains(&opcode) { usize::from(opcode - 0x5f) + 1 } else { 1 }
}

/// Owned execution view for a particular entry alignment.
#[derive(Clone, Debug)]
pub(crate) struct PreparedExecutionView {
    /// Shared bytecode; its jump map admits only entries with this same safe tail.
    pub bytecode: Bytecode,
    /// Local generated transfer/STOP offset.
    pub tail_offset: usize,
}

#[derive(Debug)]
struct ExecutionLayouts {
    jumps: JumpTable,
    tails: Vec<u8>,
    variants: [OnceLock<Bytecode>; 33],
}

impl PartialEq for PreparedCodeChunk {
    fn eq(&self, other: &Self) -> bool {
        self.code_size == other.code_size
            && self.index == other.index
            && self.leading_data_len == other.leading_data_len
            && self.jump_data_len == other.jump_data_len
            && self.lookahead == other.lookahead
    }
}

impl Eq for PreparedCodeChunk {}
