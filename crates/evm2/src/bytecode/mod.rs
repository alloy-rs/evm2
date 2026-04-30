//! EVM bytecode.

use crate::once_lock::OnceLock;
use alloc::{borrow::Cow, sync::Arc, vec::Vec};
use alloy_primitives::{Address, B256, Bytes, keccak256};
use analysis::analyze_legacy;
use core::{cmp::Ordering, fmt, hash};
use thiserror::Error;

mod analysis;

#[cfg(feature = "serde")]
mod serde_impl;

/// EIP-7702 version magic.
pub const EIP7702_MAGIC: u16 = 0xEF01;

/// EIP-7702 version magic bytes.
pub const EIP7702_MAGIC_BYTES: &[u8] = &EIP7702_MAGIC.to_be_bytes();

/// EIP-7702 version.
pub const EIP7702_VERSION: u8 = 0;

/// EIP-7702 bytecode length.
///
/// 2 (magic) + 1 (version) + 20 (address) = 23 bytes.
pub const EIP7702_BYTECODE_LEN: usize = 23;

/// EIP-7702 decode errors.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum Eip7702DecodeError {
    /// Invalid length of the raw bytecode.
    #[error("Eip7702 is not 23 bytes long")]
    InvalidLength,
    /// Invalid magic number.
    #[error("Bytecode is not starting with 0xEF01")]
    InvalidMagic,
    /// Unsupported version.
    #[error("Unsupported Eip7702 version.")]
    UnsupportedVersion,
}

/// Bytecode decode errors.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum BytecodeDecodeError {
    /// EIP-7702 decode error.
    #[error(transparent)]
    Eip7702(#[from] Eip7702DecodeError),
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
#[non_exhaustive]
pub enum BytecodeKind {
    /// Legacy analyzed bytecode with jump table.
    #[default]
    LegacyAnalyzed,
    /// EIP-7702 delegated bytecode.
    Eip7702,
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
        Self::default_ref().clone()
    }

    #[inline]
    fn default_ref() -> &'static Self {
        static DEFAULT: OnceLock<Bytecode> = OnceLock::new();
        DEFAULT.get_or_init(|| {
            Self(Arc::new(BytecodeInner {
                kind: BytecodeKind::LegacyAnalyzed,
                bytecode: Bytes::from_static(&[crate::interpreter::op::STOP]),
                original_len: 0,
                jump_table: JumpTable::default(),
            }))
        })
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
    ///
    /// # Panics
    ///
    /// Panics if bytecode is in incorrect format. If you want to handle errors use
    /// [`Self::new_raw_checked`].
    #[inline]
    pub fn new_raw(bytecode: Bytes) -> Self {
        Self::new_raw_checked(bytecode).expect("Expect correct bytecode")
    }

    /// Creates a new EIP-7702 [`Bytecode`] from [`Address`].
    #[inline]
    pub fn new_eip7702(address: Address) -> Self {
        let raw: Bytes = [EIP7702_MAGIC_BYTES, &[EIP7702_VERSION], &address[..]].concat().into();
        Self(Arc::new(BytecodeInner {
            kind: BytecodeKind::Eip7702,
            original_len: raw.len(),
            bytecode: raw,
            jump_table: JumpTable::default(),
        }))
    }

    /// Creates a new raw [`Bytecode`].
    ///
    /// Returns an error on incorrect bytecode format.
    #[inline]
    pub fn new_raw_checked(bytes: Bytes) -> Result<Self, BytecodeDecodeError> {
        if bytes.starts_with(EIP7702_MAGIC_BYTES) {
            Self::new_eip7702_raw(bytes).map_err(Into::into)
        } else {
            Ok(Self::new_legacy(bytes))
        }
    }

    /// Creates a new EIP-7702 [`Bytecode`] from raw bytes.
    ///
    /// Returns an error if the bytes are not valid EIP-7702 bytecode.
    #[inline]
    pub fn new_eip7702_raw(bytes: Bytes) -> Result<Self, Eip7702DecodeError> {
        if bytes.len() != EIP7702_BYTECODE_LEN {
            return Err(Eip7702DecodeError::InvalidLength);
        }
        if !bytes.starts_with(EIP7702_MAGIC_BYTES) {
            return Err(Eip7702DecodeError::InvalidMagic);
        }
        if bytes[2] != EIP7702_VERSION {
            return Err(Eip7702DecodeError::UnsupportedVersion);
        }
        Ok(Self(Arc::new(BytecodeInner {
            kind: BytecodeKind::Eip7702,
            original_len: bytes.len(),
            bytecode: bytes,
            jump_table: JumpTable::default(),
        })))
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

    /// Returns `true` if bytecode is EIP-7702.
    #[inline]
    pub fn is_eip7702(&self) -> bool {
        self.kind() == BytecodeKind::Eip7702
    }

    /// Returns the EIP-7702 delegated address if this is EIP-7702 bytecode.
    #[inline]
    pub fn eip7702_address(&self) -> Option<Address> {
        if self.is_eip7702() { Some(Address::from_slice(&self.0.bytecode[3..23])) } else { None }
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
        unsafe { self.0.bytecode.get_unchecked(..self.0.original_len) }
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
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
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
    use crate::interpreter::op as opcode;
    use alloy_primitives::{Address, Bytes};

    #[test]
    fn test_new_empty() {
        for bytecode in [
            Bytecode::default(),
            Bytecode::new(),
            Bytecode::new(),
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

    #[test]
    fn eip7702_sanity_decode() {
        let raw = Bytes::from_static(&[0xEF, 0x01, 0xDE, 0xAD, 0xBE, 0xEF]);
        assert_eq!(Bytecode::new_eip7702_raw(raw), Err(Eip7702DecodeError::InvalidLength));

        let mut raw = [0u8; EIP7702_BYTECODE_LEN];
        raw[..2].copy_from_slice(EIP7702_MAGIC_BYTES);
        raw[2] = 1;
        assert_eq!(
            Bytecode::new_eip7702_raw(Bytes::copy_from_slice(&raw)),
            Err(Eip7702DecodeError::UnsupportedVersion)
        );

        raw[2] = EIP7702_VERSION;
        raw[3..7].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
        let raw = Bytes::copy_from_slice(&raw);
        let bytecode = Bytecode::new_eip7702_raw(raw.clone()).unwrap();
        assert!(bytecode.is_eip7702());
        assert_eq!(bytecode.eip7702_address(), Some(Address::from_slice(&raw[3..])));
        assert_eq!(bytecode.original_bytes(), raw);
    }

    #[test]
    fn eip7702_from_address() {
        let address = Address::new([0x01; 20]);
        let bytecode = Bytecode::new_eip7702(address);
        assert_eq!(bytecode.eip7702_address(), Some(address));
        assert_eq!(bytecode.original_bytes().len(), EIP7702_BYTECODE_LEN);
        assert_eq!(&bytecode.original_byte_slice()[..3], &[0xEF, 0x01, 0x00]);
    }

    #[test]
    #[should_panic(expected = "slice bit length 8 is less than bit_len 10")]
    fn test_jump_table_from_slice_panic() {
        let _ = JumpTable::from_slice(&[0x00], 10);
    }

    #[test]
    fn test_jump_table_from_slice() {
        let jump_table = JumpTable::from_slice(&[0x00], 3);
        assert_eq!(jump_table.len(), 3);
    }

    #[test]
    fn test_jump_table_is_valid() {
        let jump_table = JumpTable::from_slice(&[0x0D, 0x06], 13);

        assert_eq!(jump_table.len(), 13);

        assert!(jump_table.is_valid(0));
        assert!(!jump_table.is_valid(1));
        assert!(jump_table.is_valid(2));
        assert!(jump_table.is_valid(3));
        assert!(!jump_table.is_valid(4));
        assert!(!jump_table.is_valid(5));
        assert!(!jump_table.is_valid(6));
        assert!(!jump_table.is_valid(7));
        assert!(!jump_table.is_valid(8));
        assert!(jump_table.is_valid(9));
        assert!(jump_table.is_valid(10));
        assert!(!jump_table.is_valid(11));
        assert!(!jump_table.is_valid(12));
    }

    #[cfg(feature = "serde")]
    #[test]
    fn test_jump_table_serde_roundtrip() {
        let original = JumpTable::from_slice(&[0x0D, 0x06], 13);

        let serialized = serde_json::to_string(&original).unwrap();
        let deserialized: JumpTable = serde_json::from_str(&serialized).unwrap();

        assert_eq!(original.len(), deserialized.len());
        assert_eq!(original.table, deserialized.table);
        assert_eq!(original, deserialized);

        for i in 0..13 {
            assert_eq!(original.is_valid(i), deserialized.is_valid(i), "mismatch at index {i}");
        }
    }
}
