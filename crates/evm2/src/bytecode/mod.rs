//! Legacy bytecode analysis and jump table.

pub mod analysis;

use alloc::{borrow::Cow, vec::Vec};
use bitvec::vec::BitVec;
use core::{cmp::Ordering, fmt, hash};

pub use analysis::analyze_legacy;

pub mod opcode {
    pub use crate::interpreter::op::*;
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
        Self::new(Default::default())
    }
}

impl JumpTable {
    /// Create new JumpTable directly from an existing BitVec.
    #[inline]
    pub fn new(jumps: BitVec<u8>) -> Self {
        let bit_len = jumps.len();
        Self::from_vec(jumps.into_vec(), bit_len)
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
