//! Module that contains the bytecode struct with legacy bytecode analysis.

mod analysis;

use alloc::{borrow::Cow, sync::Arc, vec::Vec};
use alloy_primitives::{B256, Bytes, keccak256};
use analysis::analyze_legacy;
use core::{cmp::Ordering, fmt, hash};

pub mod opcode {
    pub use crate::interpreter::op::*;
}

/// Ethereum EVM bytecode.
#[derive(Clone, Debug)]
pub struct Bytecode(Arc<BytecodeInner>);

/// Inner bytecode representation.
///
/// This struct is flattened to avoid nested allocations. The `kind` field determines
/// how the bytecode should be interpreted.
#[derive(Debug)]
struct BytecodeInner {
    /// The kind of bytecode.
    kind: BytecodeKind,
    /// The bytecode bytes.
    ///
    /// For legacy bytecode, this may be padded with zeros at the end.
    bytecode: Bytes,
    /// The original length of the bytecode before padding.
    original_len: usize,
    /// The jump table for legacy bytecode.
    jump_table: JumpTable,
}

/// The kind of bytecode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Ord, PartialOrd, Default)]
pub enum BytecodeKind {
    /// Legacy analyzed bytecode with jump table.
    #[default]
    LegacyAnalyzed,
}

impl Default for Bytecode {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl PartialEq for Bytecode {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.original_byte_slice() == other.original_byte_slice()
    }
}

impl Eq for Bytecode {}

impl hash::Hash for Bytecode {
    #[inline]
    fn hash<H: hash::Hasher>(&self, state: &mut H) {
        self.original_byte_slice().hash(state);
    }
}

impl PartialOrd for Bytecode {
    #[inline]
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Bytecode {
    #[inline]
    fn cmp(&self, other: &Self) -> Ordering {
        self.original_byte_slice().cmp(other.original_byte_slice())
    }
}

impl Bytecode {
    /// Creates a new legacy analyzed [`Bytecode`] with exactly one STOP opcode.
    #[inline]
    pub fn new() -> Self {
        Self(Arc::new(BytecodeInner {
            kind: BytecodeKind::LegacyAnalyzed,
            bytecode: Bytes::from_static(&[opcode::STOP]),
            original_len: 0,
            jump_table: JumpTable::default(),
        }))
    }

    /// Creates a new legacy [`Bytecode`] by analyzing raw bytes.
    #[inline]
    pub fn new_legacy(raw: Bytes) -> Self {
        if raw.is_empty() {
            return Self::new();
        }

        let original_len = raw.len();
        let (jump_table, bytecode) = analyze_legacy(raw);
        Self(Arc::new(BytecodeInner {
            kind: BytecodeKind::LegacyAnalyzed,
            original_len,
            bytecode,
            jump_table,
        }))
    }

    /// Creates a new raw [`Bytecode`].
    #[inline]
    pub fn new_raw(bytecode: Bytes) -> Self {
        Self::new_legacy(bytecode)
    }

    /// Create new checked bytecode from pre-analyzed components.
    ///
    /// # Safety
    ///
    /// `bytecode` must satisfy the same padding invariants produced by
    /// `analyze_legacy`. In particular, execution must never cause the
    /// interpreter to read past the backing allocation when decoding opcode
    /// immediates (`PUSH1`-`PUSH32` via `read_slice`, and `DUPN`/`SWAPN`/
    /// `EXCHANGE` via `read_u8`).
    ///
    /// [`Bytecode::new_legacy`] handles this automatically.
    /// This constructor is only for restoring trusted, previously analyzed
    /// bytecode where the padding was already applied.
    ///
    /// Violating this causes undefined behavior during execution due to
    /// out-of-bounds reads from raw pointers.
    ///
    /// # Panics
    ///
    /// * If `original_len` is greater than `bytecode.len()`
    /// * If jump table length is less than `original_len`
    /// * If bytecode is empty
    #[inline]
    pub unsafe fn new_analyzed(
        bytecode: Bytes,
        original_len: usize,
        jump_table: JumpTable,
    ) -> Self {
        assert!(original_len <= bytecode.len(), "original_len is greater than bytecode length");
        assert!(original_len <= jump_table.len(), "jump table length is less than original length");
        assert!(!bytecode.is_empty(), "bytecode cannot be empty");
        Self(Arc::new(BytecodeInner {
            kind: BytecodeKind::LegacyAnalyzed,
            bytecode,
            original_len,
            jump_table,
        }))
    }

    /// Returns the kind of bytecode.
    #[inline]
    pub fn kind(&self) -> BytecodeKind {
        self.0.kind
    }

    /// Returns `true` if bytecode is legacy.
    #[inline]
    pub fn is_legacy(&self) -> bool {
        self.kind() == BytecodeKind::LegacyAnalyzed
    }

    /// Returns jump table if bytecode is legacy analyzed.
    #[inline]
    pub fn legacy_jump_table(&self) -> Option<&JumpTable> {
        if self.is_legacy() { Some(&self.0.jump_table) } else { None }
    }

    /// Calculates hash of the bytecode.
    #[inline]
    pub fn hash_slow(&self) -> B256 {
        keccak256(self.original_byte_slice())
    }

    /// Returns a reference to the bytecode bytes.
    ///
    /// For legacy bytecode, this includes padding.
    #[inline]
    pub fn bytecode(&self) -> &Bytes {
        &self.0.bytecode
    }

    /// Pointer to the bytecode bytes.
    #[inline]
    pub fn bytecode_ptr(&self) -> *const u8 {
        self.0.bytecode.as_ptr()
    }

    /// Returns a clone of the bytecode bytes.
    #[inline]
    pub fn bytes(&self) -> Bytes {
        self.0.bytecode.clone()
    }

    /// Returns a reference to the bytecode bytes.
    #[inline]
    pub fn bytes_ref(&self) -> &Bytes {
        &self.0.bytecode
    }

    /// Returns the bytecode as a slice.
    #[inline]
    pub fn bytes_slice(&self) -> &[u8] {
        &self.0.bytecode
    }

    /// Returns the original bytecode without padding.
    #[inline]
    pub fn original_bytes(&self) -> Bytes {
        self.0.bytecode.slice(..self.0.original_len)
    }

    /// Returns the original bytecode as a byte slice without padding.
    #[inline]
    pub fn original_byte_slice(&self) -> &[u8] {
        &self.0.bytecode[..self.0.original_len]
    }

    /// Returns the length of the original bytes (without padding).
    #[inline]
    pub fn len(&self) -> usize {
        self.0.original_len
    }

    /// Returns whether the bytecode is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.0.original_len == 0
    }
}

/// A table of valid `jump` destinations.
///
/// It is immutable and memory efficient, with one bit per byte in the bytecode.
pub struct JumpTable {
    table: Cow<'static, [u8]>,
    bit_len: usize,
}

impl Clone for JumpTable {
    #[inline]
    fn clone(&self) -> Self {
        Self {
            table: match &self.table {
                Cow::Borrowed(b) => Cow::Borrowed(b),
                Cow::Owned(o) => Cow::Owned(o.clone()),
            },
            bit_len: self.bit_len,
        }
    }
}

impl fmt::Debug for JumpTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JumpTable").field("map", &self.as_slice()).finish()
    }
}

impl PartialEq for JumpTable {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.as_slice().eq(other.as_slice())
    }
}

impl Eq for JumpTable {}

impl PartialOrd for JumpTable {
    #[inline]
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for JumpTable {
    #[inline]
    fn cmp(&self, other: &Self) -> Ordering {
        self.as_slice().cmp(other.as_slice())
    }
}

impl hash::Hash for JumpTable {
    #[inline]
    fn hash<H: hash::Hasher>(&self, state: &mut H) {
        self.as_slice().hash(state);
    }
}

impl Default for JumpTable {
    #[inline]
    fn default() -> Self {
        Self::from_vec(Vec::new(), 0)
    }
}

impl JumpTable {
    #[inline]
    pub(crate) fn new(bit_len: usize) -> Self {
        let bytes = bit_len.div_ceil(8);
        Self::from_vec(alloc::vec![0; bytes], bit_len)
    }

    #[inline]
    pub(crate) fn set(&mut self, pc: usize) {
        debug_assert!(pc < self.bit_len);
        self.table.to_mut()[pc >> 3] |= 1 << (pc & 7);
    }

    /// Constructs a jump map from raw bytes and length.
    ///
    /// Bit length represents number of used bits inside slice.
    ///
    /// # Panics
    ///
    /// Panics if number of bits in slice is less than bit_len.
    #[inline]
    pub fn from_slice(slice: &[u8], bit_len: usize) -> Self {
        Self::size_assert(slice.len(), bit_len);
        Self::from_vec(slice.to_vec(), bit_len)
    }

    #[inline]
    fn from_vec(slice: Vec<u8>, bit_len: usize) -> Self {
        #[cfg(debug_assertions)]
        Self::size_assert(slice.len(), bit_len);
        Self { table: slice.into(), bit_len }
    }

    /// Constructs a jump map from raw bytes and length.
    ///
    /// Bit length represents number of used bits inside slice.
    ///
    /// # Panics
    ///
    /// Panics if number of bits in slice is less than bit_len.
    #[inline]
    pub fn from_static_slice(slice: &'static [u8], bit_len: usize) -> Self {
        Self::size_assert(slice.len(), bit_len);
        Self { table: Cow::Borrowed(slice), bit_len }
    }

    #[inline]
    fn size_assert(len: usize, bit_len: usize) {
        const BYTE_LEN: usize = 8;
        assert!(
            len * BYTE_LEN >= bit_len,
            "slice bit length {} is less than bit_len {}",
            len * BYTE_LEN,
            bit_len
        );
    }

    /// Gets the raw bytes of the jump map.
    #[inline]
    pub fn as_slice(&self) -> &[u8] {
        &self.table
    }

    /// Gets the bit length of the jump map.
    #[inline]
    pub const fn len(&self) -> usize {
        self.bit_len
    }

    /// Returns true if the jump map is empty.
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Checks if `pc` is a valid jump destination.
    #[inline]
    pub fn is_valid(&self, pc: usize) -> bool {
        pc < self.bit_len
            && unsafe { *self.as_slice().as_ptr().add(pc >> 3) & (1 << (pc & 7)) != 0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::Bytes;

    #[test]
    fn test_new_empty() {
        for bytecode in [
            Bytecode::default(),
            Bytecode::new(),
            Bytecode::new().clone(),
            Bytecode::new_legacy(Bytes::new()),
        ] {
            assert_eq!(bytecode.kind(), BytecodeKind::LegacyAnalyzed);
            assert_eq!(bytecode.len(), 0);
            assert_eq!(bytecode.bytes_slice(), [opcode::STOP]);
        }
    }

    #[test]
    fn test_new_analyzed() {
        let raw = Bytes::from_static(&[opcode::PUSH1, 0x01]);
        let bytecode = Bytecode::new_legacy(raw);
        let _ = unsafe {
            Bytecode::new_analyzed(
                bytecode.bytecode().clone(),
                bytecode.len(),
                bytecode.legacy_jump_table().unwrap().clone(),
            )
        };
    }

    #[test]
    #[should_panic(expected = "original_len is greater than bytecode length")]
    fn test_panic_on_large_original_len() {
        let bytecode = Bytecode::new_legacy(Bytes::from_static(&[opcode::PUSH1, 0x01]));
        let _ = unsafe {
            Bytecode::new_analyzed(
                bytecode.bytecode().clone(),
                100,
                bytecode.legacy_jump_table().unwrap().clone(),
            )
        };
    }

    #[test]
    #[should_panic(expected = "jump table length is less than original length")]
    fn test_panic_on_short_jump_table() {
        let bytecode = Bytecode::new_legacy(Bytes::from_static(&[opcode::PUSH1, 0x01]));
        let jump_table = JumpTable::from_slice(&[0], 1);
        let _ = unsafe { Bytecode::new_analyzed(bytecode.bytecode().clone(), 2, jump_table) };
    }

    #[test]
    #[should_panic(expected = "bytecode cannot be empty")]
    fn test_panic_on_empty_bytecode() {
        let bytecode = Bytes::from_static(&[]);
        let jump_table = JumpTable::default();
        let _ = unsafe { Bytecode::new_analyzed(bytecode, 0, jump_table) };
    }
}
