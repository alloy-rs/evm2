use crate::error::{Error, Result};
use alloy_primitives::{Address, B256, Bytes, U256};
use serde::{Deserialize, Serialize};
use std::{fs, path::Path};
use wincode::{SchemaRead, io::Reader};

pub(crate) const FORMAT_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) enum Chain {
    Mainnet,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Manifest {
    pub(crate) version: u32,
    pub(crate) chain: Chain,
    pub(crate) generated_at_unix: u64,
    pub(crate) blocks: Vec<Artifact>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Artifact {
    pub(crate) block_number: u64,
    pub(crate) block_hash: B256,
    pub(crate) file_name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Block {
    pub(crate) chain: Chain,
    pub(crate) raw_block: Bytes,
    pub(crate) block_number: u64,
    pub(crate) block_hash: B256,
    pub(crate) parent_hash: B256,
    pub(crate) parent_state_root: B256,
    pub(crate) transactions: Vec<Transaction>,
    pub(crate) state: State,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Transaction {
    pub(crate) tx_hash: B256,
    pub(crate) signer: Address,
    pub(crate) encoded_2718: Bytes,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct State {
    pub(crate) accounts: Vec<Account>,
    pub(crate) storage: Vec<StorageSlot>,
    pub(crate) contracts: Vec<Code>,
    pub(crate) block_hashes: Vec<BlockHash>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Account {
    pub(crate) address: Address,
    pub(crate) balance: U256,
    pub(crate) nonce: u64,
    pub(crate) code_hash: B256,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct StorageSlot {
    pub(crate) address: Address,
    pub(crate) slot: B256,
    pub(crate) value: U256,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Code {
    pub(crate) code_hash: B256,
    pub(crate) bytecode: Bytes,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct BlockHash {
    pub(crate) number: u64,
    pub(crate) hash: B256,
}

pub(crate) fn read_block(path: &Path) -> Result<Block> {
    let bytes =
        fs::read(path).map_err(|source| Error::ReadBlock { path: path.to_path_buf(), source })?;
    let mut remaining = bytes.as_slice();
    let block =
        <serde_wincode::SerdeCompat<Block> as SchemaRead<wincode::config::Configuration>>::get(
            remaining.by_ref(),
        )
        .map_err(|source| Error::DecodeBlock { path: path.to_path_buf(), source })?;
    if remaining.is_empty() {
        Ok(block)
    } else {
        Err(Error::DecodeBlock {
            path: path.to_path_buf(),
            source: wincode::error::ReadError::Custom("trailing bytes"),
        })
    }
}

pub(crate) fn read_manifest(path: &Path) -> Result<Manifest> {
    let bytes = fs::read(path)
        .map_err(|source| Error::ReadManifest { path: path.to_path_buf(), source })?;
    serde_json::from_slice(&bytes)
        .map_err(|source| Error::DecodeManifest { path: path.to_path_buf(), source })
}

pub(crate) const fn validate_manifest(manifest: &Manifest) -> Result<()> {
    if manifest.version != FORMAT_VERSION {
        return Err(Error::UnsupportedManifestVersion {
            actual: manifest.version,
            expected: FORMAT_VERSION,
        });
    }
    Ok(())
}
