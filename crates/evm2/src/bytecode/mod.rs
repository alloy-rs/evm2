//! EVM bytecode.

use crate::{
    constants::{EIP7702_BYTECODE_LEN, EIP7702_MAGIC_BYTES, EIP7702_VERSION},
    interpreter::op,
    once_lock::OnceLock,
};
use alloc::sync::Arc;
use alloy_primitives::{Address, B256, Bytes, KECCAK256_EMPTY, keccak256};
use analysis::{analyze_legacy, pad_legacy};
use core::{cmp::Ordering, hash};
use thiserror::Error;

mod analysis;
mod jump_table;

#[cfg(feature = "serde")]
mod serde_impl;

pub use jump_table::{JumpTable, JumpTableRef};

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
    /// Lazily initialized jump table for legacy bytecode.
    jump_table: OnceLock<JumpTable>,
    /// Cached hash of the original bytecode.
    hash: OnceLock<B256>,
}

/// The kind of bytecode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Ord, PartialOrd, Default)]
#[non_exhaustive]
pub enum BytecodeKind {
    /// Legacy bytecode with a lazily initialized jump table.
    #[default]
    Legacy,
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
    /// Creates an empty legacy [`Bytecode`] backed by exactly one STOP opcode.
    #[inline]
    pub fn new() -> Self {
        Self::default_ref().clone()
    }

    #[inline]
    fn default_ref() -> &'static Self {
        static DEFAULT: OnceLock<Bytecode> = OnceLock::new();
        DEFAULT.get_or_init(|| {
            Self(Arc::new(BytecodeInner {
                kind: BytecodeKind::Legacy,
                bytecode: Bytes::from_static(&[op::STOP]),
                original_len: 0,
                jump_table: OnceLock::new(),
                hash: {
                    let hash = OnceLock::new();
                    let _ = hash.set(KECCAK256_EMPTY);
                    hash
                },
            }))
        })
    }

    /// Creates a new legacy [`Bytecode`] with padding and deferred jump analysis.
    ///
    /// Appends 33 zero bytes unless the input already ends with 33 zeros.
    /// Empty code retains its single STOP representation.
    #[inline(never)]
    pub fn new_legacy(raw: Bytes) -> Self {
        if raw.is_empty() {
            return Self::new();
        }

        let original_len = raw.len();
        let bytecode = pad_legacy(raw);
        Self(Arc::new(BytecodeInner {
            kind: BytecodeKind::Legacy,
            original_len,
            bytecode,
            jump_table: OnceLock::new(),
            hash: OnceLock::new(),
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
            jump_table: OnceLock::new(),
            hash: OnceLock::new(),
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
            jump_table: OnceLock::new(),
            hash: OnceLock::new(),
        })))
    }

    /// Create new checked bytecode from pre-analyzed components.
    ///
    /// # Safety
    ///
    /// `bytecode` must satisfy the same padding invariants produced by
    /// `pad_legacy`. In particular, execution must never cause the
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
            kind: BytecodeKind::Legacy,
            bytecode,
            original_len,
            jump_table: {
                let cached = OnceLock::new();
                let _ = cached.set(jump_table);
                cached
            },
            hash: OnceLock::new(),
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
        self.kind() == BytecodeKind::Legacy
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

    /// Returns the legacy jump table, initializing it on first access.
    #[inline]
    pub fn legacy_jump_table(&self) -> Option<&JumpTable> {
        if self.is_legacy() { Some(self.jump_table()) } else { None }
    }

    #[inline]
    pub(crate) fn jump_table(&self) -> &JumpTable {
        self.0.jump_table.get_or_init(|| {
            if self.is_legacy() {
                analyze_legacy(self.original_byte_slice())
            } else {
                JumpTable::default()
            }
        })
    }

    /// Calculates or returns cached hash of the bytecode.
    #[inline]
    pub fn hash_slow(&self) -> B256 {
        *self.0.hash.get_or_init(|| keccak256(self.original_byte_slice()))
    }

    /// Returns a reference to the potentially padded bytecode bytes.
    #[inline]
    pub fn bytes(&self) -> &Bytes {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interpreter::op;
    use alloy_primitives::{Address, Bytes};

    #[test]
    fn test_new_empty() {
        for bytecode in [
            Bytecode::default(),
            Bytecode::new(),
            Bytecode::new(),
            Bytecode::new_legacy(Bytes::new()),
        ] {
            assert_eq!(bytecode.kind(), BytecodeKind::Legacy);
            assert_eq!(bytecode.len(), 0);
            assert_eq!(bytecode.bytes_slice(), [op::STOP]);
        }
    }

    #[test]
    fn test_new_analyzed() {
        let raw = Bytes::from_static(&[op::PUSH1, 0x01]);
        let bytecode = Bytecode::new_legacy(raw);
        let _ = unsafe {
            Bytecode::new_analyzed(
                bytecode.bytes().clone(),
                bytecode.len(),
                bytecode.legacy_jump_table().unwrap().clone(),
            )
        };
    }

    #[test]
    #[should_panic(expected = "original_len is greater than bytecode length")]
    fn test_panic_on_large_original_len() {
        let bytecode = Bytecode::new_legacy(Bytes::from_static(&[op::PUSH1, 0x01]));
        let _ = unsafe {
            Bytecode::new_analyzed(
                bytecode.bytes().clone(),
                100,
                bytecode.legacy_jump_table().unwrap().clone(),
            )
        };
    }

    #[test]
    #[should_panic(expected = "jump table length is less than original length")]
    fn test_panic_on_short_jump_table() {
        let bytecode = Bytecode::new_legacy(Bytes::from_static(&[op::PUSH1, 0x01]));
        let jump_table = JumpTable::from_slice(&[0], 1);
        let _ = unsafe { Bytecode::new_analyzed(bytecode.bytes().clone(), 2, jump_table) };
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
    fn analysis_is_lazy_and_shared() {
        let raw = Bytes::from_static(&[op::JUMPDEST, op::PUSH1, op::JUMPDEST, op::PUSH32]);
        for bytecode in [
            Bytecode::new_legacy(raw.clone()),
            Bytecode::new_raw(raw.clone()),
            Bytecode::new_raw_checked(raw.clone()).unwrap(),
        ] {
            let cloned = bytecode.clone();
            assert_eq!(bytecode.original_byte_slice(), raw.as_ref());
            assert_eq!(bytecode.original_bytes().as_ptr(), bytecode.bytes().as_ptr());
            assert_eq!(bytecode.len(), raw.len());
            assert!(!bytecode.is_empty());
            assert_eq!(bytecode, cloned);
            assert_eq!(bytecode.cmp(&cloned), Ordering::Equal);
            assert_eq!(bytecode.hash_slow(), keccak256(&raw));
            assert_eq!(bytecode.bytes_slice().len(), raw.len() + 33);
            assert_eq!(&bytecode.bytes_slice()[raw.len()..], &[0; 33]);
            assert!(bytecode.0.jump_table.get().is_none());

            let table = bytecode.legacy_jump_table().unwrap();
            assert_eq!(table.len(), raw.len());
            assert!(table.is_valid(0));
            assert!(!table.is_valid(2)); // JUMPDEST in PUSH data.
            assert!(!table.is_valid(raw.len())); // Padding is not original code.
            assert!(core::ptr::eq(table, cloned.legacy_jump_table().unwrap()));
            assert!(core::ptr::eq(table, bytecode.jump_table()));
        }
    }

    #[test]
    fn interpreter_accessor_initializes_analysis() {
        let bytecode = Bytecode::new_legacy(Bytes::from_static(&[op::JUMPDEST]));
        assert!(bytecode.0.jump_table.get().is_none());
        let table = bytecode.jump_table();
        assert!(table.is_valid(0));
        assert!(core::ptr::eq(table, bytecode.0.jump_table.get().unwrap()));
        assert!(core::ptr::eq(table, bytecode.legacy_jump_table().unwrap()));
    }

    #[test]
    fn supplied_analysis_is_preserved() {
        let padded = Bytes::from_static(&[op::JUMPDEST, op::PUSH1, 0, op::STOP]);
        let ptr = padded.as_ptr();
        let table = JumpTable::from_slice(&[1], 2);
        let table_ptr = table.as_slice().as_ptr();
        // SAFETY: PUSH1 has a complete immediate and a terminating STOP.
        let bytecode = unsafe { Bytecode::new_analyzed(padded, 2, table) };
        assert!(bytecode.0.jump_table.get().is_some());
        assert_eq!(bytecode.bytes().as_ptr(), ptr);
        assert_eq!(bytecode.bytes().len(), 4);
        assert_eq!(bytecode.original_byte_slice(), &[op::JUMPDEST, op::PUSH1]);
        assert_eq!(bytecode.jump_table().as_slice().as_ptr(), table_ptr);
        assert!(bytecode.jump_table().is_valid(0));
    }

    #[test]
    fn empty_and_delegated_jump_tables() {
        assert!(Bytecode::new().jump_table().is_empty());
        let delegated = Bytecode::new_eip7702(Address::with_last_byte(op::JUMPDEST));
        let from_raw = Bytecode::new_eip7702_raw(delegated.original_bytes()).unwrap();
        for bytecode in [delegated, from_raw] {
            assert!(bytecode.legacy_jump_table().is_none());
            assert!(bytecode.0.jump_table.get().is_none());
            assert_eq!(bytecode.bytes_slice(), bytecode.original_byte_slice());
            assert!(bytecode.jump_table().is_empty());
        }
    }

    #[test]
    fn existing_zero_suffix_is_original_code() {
        let mut raw = [0; 34];
        raw[0] = op::PUSH32;
        let raw = Bytes::copy_from_slice(&raw);
        let bytecode = Bytecode::new_legacy(raw.clone());
        assert_eq!(bytecode.bytes().as_ptr(), raw.as_ptr());
        assert_eq!(bytecode.len(), raw.len());
        assert_eq!(bytecode.original_bytes(), raw);
        assert_eq!(bytecode.hash_slow(), keccak256(&raw));
        assert!(bytecode.0.jump_table.get().is_none());
        assert_eq!(bytecode.jump_table().len(), raw.len());
    }
}
