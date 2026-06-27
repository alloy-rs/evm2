//! Normalized capture model used between RPC collection and EEST export.
//!
//! Capture has two distinct phases. The builder first reads blocks and trace output from RPC
//! and reduces them into the small set of data needed to replay the block range: a base pre-state,
//! optional final post-state, historical block hashes, deduplicated bytecode, execution versions,
//! and the blocks with recovered transaction signers. The exporter then turns this model into
//! the EEST blockchain-test JSON shape.
//!
//! Keeping this model separate from the serialized EEST structs lets capture stay focused on
//! normalization rules, such as excluding values already produced by earlier in-range execution and
//! storing bytecode once by code hash. It also keeps the exporter as a format boundary instead of
//! mixing RPC trace interpretation with JSON layout details.

use super::MainnetBlock;
use alloy_primitives::{Address, B256, Bytes, U256};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CapturedCase {
    pub(super) versions: CapturedVersions,
    pub(super) code_table: CodeTable,
    pub(super) pre_state: State,
    pub(super) post_state: Option<State>,
    pub(super) input: CapturedInput,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CapturedVersions {
    pub(super) versions: Vec<CapturedVersion>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CapturedVersion {
    pub(super) spec_id: u32,
}

pub(super) const fn captured_version(spec: evm2::SpecId) -> CapturedVersion {
    CapturedVersion { spec_id: spec as u32 }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct CodeTable {
    pub(super) codes: Vec<Code>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Code {
    pub(super) code_hash: B256,
    pub(super) bytecode: Bytes,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct State {
    pub(super) accounts: Vec<AccountState>,
    pub(super) block_hashes: Vec<BlockHash>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AccountState {
    pub(super) address: Address,
    pub(super) balance: U256,
    pub(super) nonce: u64,
    pub(super) code_hash: B256,
    pub(super) storage: Vec<StorageEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct StorageEntry {
    pub(super) slot: B256,
    pub(super) value: U256,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct BlockHash {
    pub(super) number: u64,
    pub(super) hash: B256,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum CapturedInput {
    Block(Box<CapturedBlock>),
    Blocks(CapturedBlocks),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CapturedBlocks {
    pub(super) blocks: Vec<CapturedBlock>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CapturedBlock {
    pub(super) block: MainnetBlock,
    pub(super) transactions: Vec<CapturedTransaction>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CapturedTransaction {
    pub(super) signer: Address,
}
