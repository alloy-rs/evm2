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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        EvmFeatures,
        bytecode::{CODE_CHUNK_SIZE, code_metadata},
        evm::{AccountInfo, CacheDB, State},
    };
    use alloc::vec;

    #[test]
    fn versioned_extensions_roundtrip_and_reject_malformed_data() {
        let code = Bytes::from(vec![0; CODE_CHUNK_SIZE + 1]);
        let chunked = AccountExtension::chunked(code_metadata(&code).unwrap().unwrap());
        for (version, extension) in [
            (0, AccountExtension::new()),
            (1, chunked),
            (2, AccountExtension::delegated(Address::repeat_byte(7))),
        ] {
            assert_eq!(extension.version(), version);
            assert_eq!(AccountExtension::decode(&extension.encode()).unwrap(), extension);
        }
        for bytes in [vec![0], vec![3], vec![1, 0, 0, 0, 1], vec![2; 20], vec![2; 22], {
            let mut bytes = vec![0; 21];
            bytes[0] = 2;
            bytes
        }] {
            assert_eq!(AccountExtension::decode(&bytes), Err(CodeChunkError::InvalidMetadata));
        }
    }

    #[test]
    fn code_extension_changes_invalidate_immediately_and_rollback_restores_chunks() {
        let owner = Address::repeat_byte(1);
        let mut state = State::new(CacheDB::default());
        state
            .account(&owner)
            .unwrap()
            .set_code_slow(Bytecode::new_legacy(Bytes::from_static(&[0])));
        assert!(state.load_code_chunk(&owner, 0, false).unwrap().unwrap().warm());
        let before = state.account(&owner).unwrap().get().unwrap().clone();
        let checkpoint = state.checkpoint();
        {
            let mut account = state.account(&owner).unwrap();
            account.set_extension(AccountExtension::delegated(Address::repeat_byte(2)));
            assert!(account.code_chunks().is_empty());
        }
        state.rollback(checkpoint, EvmFeatures::empty());
        assert_eq!(state.account(&owner).unwrap().get(), Some(&before));
        assert!(state.code_chunk_is_warm(&owner, 0));
    }

    #[test]
    fn replacing_account_representation_invalidates_before_handle_drop() {
        let owner = Address::repeat_byte(1);
        let bytes = Bytes::from(vec![0; CODE_CHUNK_SIZE + 1]);
        let mut state = State::new(CacheDB::default());
        state.account(&owner).unwrap().set_code_slow(Bytecode::new_legacy(bytes.clone()));
        assert!(state.load_code_chunk(&owner, 0, false).unwrap().unwrap().warm());
        let before = state.account(&owner).unwrap().get().unwrap().clone();
        let checkpoint = state.checkpoint();
        {
            let mut account = state.account(&owner).unwrap();
            let mut replacement = before.clone();
            replacement.extension =
                AccountExtension::chunked(code_metadata(&bytes).unwrap().unwrap());
            account.set_info(replacement);
            assert!(account.code_chunks().is_empty());
            assert_eq!(account.code_hash(), before.code_hash);
        }
        {
            let mut chunk = state.load_code_chunk(&owner, 0, false).unwrap().unwrap();
            assert_eq!(chunk.get().original_bytes().len(), CODE_CHUNK_SIZE);
            assert!(chunk.warm());
        }
        state.rollback(checkpoint, EvmFeatures::empty());
        assert_eq!(state.account(&owner).unwrap().get(), Some(&before));
        let chunk = state.load_code_chunk(&owner, 0, true).unwrap().unwrap();
        assert_eq!(chunk.get().original_bytes().len(), CODE_CHUNK_SIZE + 1);
        assert!(chunk.is_warm());
    }

    #[cfg(feature = "serde")]
    #[test]
    fn account_serialization_preserves_empty_layout_and_typed_extensions() {
        #[derive(serde::Serialize)]
        struct LegacyAccountInfo {
            balance: alloy_primitives::U256,
            nonce: u64,
            code_hash: B256,
            code: Option<Bytecode>,
        }
        let account = AccountInfo::default();
        let legacy = LegacyAccountInfo {
            balance: account.balance,
            nonce: account.nonce,
            code_hash: account.code_hash,
            code: account.code.clone(),
        };
        let encoded = rmp_serde::to_vec(&legacy).unwrap();
        assert_eq!(encoded, rmp_serde::to_vec(&account).unwrap());
        assert_eq!(rmp_serde::from_slice::<AccountInfo>(&encoded).unwrap(), account);
        assert!(serde_json::to_value(&account).unwrap().get("extension").is_none());
        let delegated =
            AccountInfo::default().with_code(Bytecode::new_eip7702(Address::repeat_byte(9)));
        let encoded = rmp_serde::to_vec(&delegated).unwrap();
        assert_eq!(rmp_serde::from_slice::<AccountInfo>(&encoded).unwrap(), delegated);
    }
}
