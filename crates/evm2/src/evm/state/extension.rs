//! Account-owned code representation and its versioned encoding.

use crate::bytecode::{Bytecode, CodeChunkError, CodeMetadata, MAX_CODE_CHUNKS};
use alloc::{sync::Arc, vec::Vec};
use alloy_primitives::{Address, B256, Bytes};

/// Authoritative code metadata, shared by account snapshots.
///
/// Empty means ordinary code. Version 1 contains chunk commitments; version 2 contains
/// a delegation target. The bytecode database does not determine an account's type.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccountExtension(Option<Arc<CodeExtension>>);

#[derive(Debug, PartialEq, Eq)]
enum CodeExtension {
    Chunks(CodeMetadata),
    Delegation(Address),
}

impl AccountExtension {
    /// Creates an ordinary account extension without allocating.
    pub const fn new() -> Self {
        Self(None)
    }

    /// Creates version 1 metadata from validated chunk commitments.
    pub fn chunked(metadata: CodeMetadata) -> Self {
        Self(Some(Arc::new(CodeExtension::Chunks(metadata))))
    }

    /// Creates version 2 metadata. Account loading also checks its marker hash.
    pub fn delegated(target: Address) -> Self {
        Self(Some(Arc::new(CodeExtension::Delegation(target))))
    }

    /// Derives account metadata when explicitly installing complete bytecode.
    pub(crate) fn for_code(code: &Bytecode) -> Self {
        code.eip7702_address().map(Self::delegated).unwrap_or_default()
    }

    /// Returns whether this account uses ordinary code.
    pub const fn is_empty(&self) -> bool {
        self.0.is_none()
    }

    /// Returns the extension version; zero denotes an empty extension.
    pub fn version(&self) -> u8 {
        match self.0.as_deref() {
            None => 0,
            Some(CodeExtension::Chunks(_)) => 1,
            Some(CodeExtension::Delegation(_)) => 2,
        }
    }

    /// Returns version 1's original code size and ordered chunk hashes.
    pub fn code_metadata(&self) -> Option<&CodeMetadata> {
        match self.0.as_deref() {
            Some(CodeExtension::Chunks(metadata)) => Some(metadata),
            _ => None,
        }
    }

    /// Returns version 2's delegation target.
    pub fn delegation_target(&self) -> Option<Address> {
        match self.0.as_deref() {
            Some(CodeExtension::Delegation(target)) => Some(*target),
            _ => None,
        }
    }

    /// Encodes empty bytes, `1 || code_size_be_u32 || hashes`, or `2 || target`.
    pub fn encode(&self) -> Bytes {
        match self.0.as_deref() {
            None => Bytes::new(),
            Some(CodeExtension::Chunks(metadata)) => {
                let mut bytes = Vec::with_capacity(5 + 32 * metadata.chunk_hashes().len());
                bytes.push(1);
                bytes.extend_from_slice(&metadata.code_size().to_be_bytes());
                for hash in metadata.chunk_hashes() {
                    bytes.extend_from_slice(hash.as_slice());
                }
                bytes.into()
            }
            Some(CodeExtension::Delegation(target)) => {
                let mut bytes = Vec::with_capacity(21);
                bytes.push(2);
                bytes.extend_from_slice(target.as_slice());
                bytes.into()
            }
        }
    }

    /// Decodes and validates a complete extension, rejecting unknown versions and trailing data.
    pub fn decode(bytes: &[u8]) -> Result<Self, CodeChunkError> {
        match bytes {
            [] => Ok(Self::new()),
            [1, a, b, c, d, hashes @ ..]
                if hashes.len().is_multiple_of(32) && hashes.len() <= 32 * MAX_CODE_CHUNKS =>
            {
                let size = u32::from_be_bytes([*a, *b, *c, *d]);
                let hashes = hashes.as_chunks::<32>().0.iter().copied().map(B256::from).collect();
                CodeMetadata::new(size, hashes).map(Self::chunked)
            }
            [2, target @ ..] if target.len() == 20 => {
                let target = Address::from_slice(target);
                if target.is_zero() {
                    return Err(CodeChunkError::InvalidMetadata);
                }
                Ok(Self::delegated(target))
            }
            _ => Err(CodeChunkError::InvalidMetadata),
        }
    }
}

#[cfg(feature = "serde")]
impl serde::Serialize for AccountExtension {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.encode().serialize(serializer)
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for AccountExtension {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let bytes = Bytes::deserialize(deserializer)?;
        Self::decode(&bytes).map_err(serde::de::Error::custom)
    }
}
