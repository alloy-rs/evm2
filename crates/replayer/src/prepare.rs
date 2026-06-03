use crate::{
    corpus,
    error::{Error, Result},
    ethereum,
};
use alloy_consensus::{Block as ConsensusBlock, EthereumTxEnvelope, Header, TxEip4844};
use alloy_eips::eip2718::Encodable2718;
use alloy_primitives::{B256, Bytes};
use alloy_rlp::Decodable;
use evm2::{SpecId, env::BlockEnv, ethereum::RecoveredTxEnvelope, evm::InMemoryDB};

type MainnetBlock = ConsensusBlock<EthereumTxEnvelope<TxEip4844>>;

#[derive(Debug)]
pub(crate) struct PreparedBlock {
    pub(crate) block_number: u64,
    pub(crate) block_hash: B256,
    pub(crate) gas_used: u64,
    pub(crate) parent_hash: B256,
    pub(crate) parent_beacon_block_root: Option<B256>,
    pub(crate) spec: SpecId,
    pub(crate) block_env: BlockEnv,
    pub(crate) db: InMemoryDB,
    pub(crate) transactions: Vec<PreparedTransaction>,
}

#[derive(Debug)]
pub(crate) struct PreparedTransaction {
    pub(crate) tx_hash: B256,
    pub(crate) tx: RecoveredTxEnvelope,
}

pub(crate) fn prepare_block(block: &corpus::Block) -> Result<PreparedBlock> {
    let consensus_block = decode_consensus_block(&block.raw_block)?;
    validate_block_identity(block, &consensus_block.header)?;

    if consensus_block.body.transactions.len() != block.transactions.len() {
        return Err(Error::TransactionCountMismatch {
            expected: block.transactions.len(),
            actual: consensus_block.body.transactions.len(),
        });
    }

    let spec = match block.chain {
        corpus::Chain::Mainnet => ethereum::mainnet_spec_for_header(&consensus_block.header),
    };
    let block_env = ethereum::block_env_for_header(&consensus_block.header, spec)?;
    let db = ethereum::db_from_state(&block.state);
    let mut transactions = Vec::with_capacity(block.transactions.len());
    for (index, (corpus_tx, consensus_tx)) in
        block.transactions.iter().zip(consensus_block.body.transactions.iter()).enumerate()
    {
        validate_transaction(index, corpus_tx, consensus_tx)?;
        transactions.push(PreparedTransaction {
            tx_hash: corpus_tx.tx_hash,
            tx: ethereum::tx_from_consensus(corpus_tx.signer, consensus_tx),
        });
    }

    Ok(PreparedBlock {
        block_number: block.block_number,
        block_hash: block.block_hash,
        gas_used: consensus_block.header.gas_used,
        parent_hash: consensus_block.header.parent_hash,
        parent_beacon_block_root: consensus_block.header.parent_beacon_block_root,
        spec,
        block_env,
        db,
        transactions,
    })
}

fn decode_consensus_block(raw_block: &Bytes) -> Result<MainnetBlock> {
    let mut slice = raw_block.as_ref();
    let block =
        MainnetBlock::decode(&mut slice).map_err(|source| Error::DecodeRawBlock { source })?;
    if !slice.is_empty() {
        return Err(Error::TrailingRawBlockRlp);
    }
    Ok(block)
}

fn validate_block_identity(block: &corpus::Block, header: &Header) -> Result<()> {
    let actual_hash = header.hash_slow();
    if block.block_hash != actual_hash {
        return Err(Error::BlockHashMismatch { expected: block.block_hash, actual: actual_hash });
    }
    if block.block_number != header.number {
        return Err(Error::BlockNumberMismatch {
            expected: block.block_number,
            actual: header.number,
        });
    }
    if block.parent_hash != header.parent_hash {
        return Err(Error::ParentHashMismatch {
            expected: block.parent_hash,
            actual: header.parent_hash,
        });
    }
    Ok(())
}

fn validate_transaction(
    index: usize,
    corpus_tx: &corpus::Transaction,
    consensus_tx: &EthereumTxEnvelope<TxEip4844>,
) -> Result<()> {
    let actual_hash = *consensus_tx.tx_hash();
    if corpus_tx.tx_hash != actual_hash {
        return Err(Error::TransactionHashMismatch {
            index,
            expected: corpus_tx.tx_hash,
            actual: actual_hash,
        });
    }

    let encoded = consensus_tx.encoded_2718();
    if corpus_tx.encoded_2718.as_ref() != encoded.as_slice() {
        return Err(Error::TransactionEncodingMismatch { index });
    }
    Ok(())
}
