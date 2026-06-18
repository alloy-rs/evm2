use crate::blockchaintest::{
    Account, Block, BlockHash, BlockHeader, BlockchainTest, BlockchainTestCase, DecodedBlock,
    ForkSpec, SealEngine, State, Transaction, Withdrawal,
};
use alloy_eip7928::BlockAccessList;
use alloy_primitives::{Address, B256, Bytes};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub(crate) fn to_vec(suite: &BlockchainTest) -> Result<Vec<u8>, postcard::Error> {
    postcard::to_allocvec(&BlockchainTestRef::new(suite))
}

pub(crate) fn from_bytes(bytes: &[u8]) -> Result<BlockchainTest, postcard::Error> {
    postcard::from_bytes::<BlockchainTestWire>(bytes).map(Into::into)
}

#[derive(Serialize)]
struct BlockchainTestRef<'a>(Vec<(&'a String, BlockchainTestCaseRef<'a>)>);

impl<'a> BlockchainTestRef<'a> {
    fn new(suite: &'a BlockchainTest) -> Self {
        Self(suite.0.iter().map(|(name, case)| (name, BlockchainTestCaseRef::new(case))).collect())
    }
}

#[derive(Serialize)]
struct BlockchainTestCaseRef<'a> {
    genesis_block_header: &'a BlockHeader,
    genesis_rlp: &'a Option<Bytes>,
    blocks: Vec<BlockRef<'a>>,
    post_state: &'a Option<BTreeMap<Address, Account>>,
    pre: &'a State,
    block_hashes: &'a Vec<BlockHash>,
    lastblockhash: &'a B256,
    network: &'a ForkSpec,
    seal_engine: &'a SealEngine,
}

impl<'a> BlockchainTestCaseRef<'a> {
    fn new(case: &'a BlockchainTestCase) -> Self {
        Self {
            genesis_block_header: &case.genesis_block_header,
            genesis_rlp: &case.genesis_rlp,
            blocks: case.blocks.iter().map(BlockRef::new).collect(),
            post_state: &case.post_state,
            pre: &case.pre,
            block_hashes: &case.block_hashes,
            lastblockhash: &case.lastblockhash,
            network: &case.network,
            seal_engine: &case.seal_engine,
        }
    }
}

#[derive(Serialize)]
struct BlockRef<'a> {
    block_header: &'a Option<BlockHeader>,
    rlp_decoded: &'a Option<DecodedBlock>,
    rlp: &'a Bytes,
    expect_exception: &'a Option<String>,
    transactions: &'a Option<Vec<Transaction>>,
    uncle_headers: &'a Option<Vec<BlockHeader>>,
    withdrawals: &'a Option<Vec<Withdrawal>>,
    block_access_list: &'a Option<BlockAccessList>,
}

impl<'a> BlockRef<'a> {
    const fn new(block: &'a Block) -> Self {
        Self {
            block_header: &block.block_header,
            rlp_decoded: &block.rlp_decoded,
            rlp: &block.rlp,
            expect_exception: &block.expect_exception,
            transactions: &block.transactions,
            uncle_headers: &block.uncle_headers,
            withdrawals: &block.withdrawals,
            block_access_list: &block.block_access_list,
        }
    }
}

#[derive(Deserialize)]
struct BlockchainTestWire(Vec<(String, BlockchainTestCaseWire)>);

impl From<BlockchainTestWire> for BlockchainTest {
    fn from(value: BlockchainTestWire) -> Self {
        Self(value.0.into_iter().map(|(name, case)| (name, case.into())).collect())
    }
}

#[derive(Deserialize)]
struct BlockchainTestCaseWire {
    genesis_block_header: BlockHeader,
    genesis_rlp: Option<Bytes>,
    blocks: Vec<BlockWire>,
    post_state: Option<BTreeMap<Address, Account>>,
    pre: State,
    block_hashes: Vec<BlockHash>,
    lastblockhash: B256,
    network: ForkSpec,
    seal_engine: SealEngine,
}

impl From<BlockchainTestCaseWire> for BlockchainTestCase {
    fn from(value: BlockchainTestCaseWire) -> Self {
        Self {
            genesis_block_header: value.genesis_block_header,
            genesis_rlp: value.genesis_rlp,
            blocks: value.blocks.into_iter().map(Into::into).collect(),
            post_state: value.post_state,
            pre: value.pre,
            block_hashes: value.block_hashes,
            lastblockhash: value.lastblockhash,
            network: value.network,
            seal_engine: value.seal_engine,
        }
    }
}

#[derive(Deserialize)]
struct BlockWire {
    block_header: Option<BlockHeader>,
    rlp_decoded: Option<DecodedBlock>,
    rlp: Bytes,
    expect_exception: Option<String>,
    transactions: Option<Vec<Transaction>>,
    uncle_headers: Option<Vec<BlockHeader>>,
    withdrawals: Option<Vec<Withdrawal>>,
    block_access_list: Option<BlockAccessList>,
}

impl From<BlockWire> for Block {
    fn from(value: BlockWire) -> Self {
        Self {
            block_header: value.block_header,
            rlp_decoded: value.rlp_decoded,
            rlp: value.rlp,
            expect_exception: value.expect_exception,
            transactions: value.transactions,
            uncle_headers: value.uncle_headers,
            withdrawals: value.withdrawals,
            block_access_list: value.block_access_list,
        }
    }
}
