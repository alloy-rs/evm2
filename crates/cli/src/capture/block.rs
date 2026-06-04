use super::CaptureError;
use alloy_consensus::{Block as ConsensusBlock, EthereumTxEnvelope, TxEip4844};
use alloy_rlp::Decodable;

pub(super) type MainnetBlock = ConsensusBlock<EthereumTxEnvelope<TxEip4844>>;

pub(super) fn decode_consensus_block(raw_block: &[u8]) -> Result<MainnetBlock, CaptureError> {
    let mut slice = raw_block;
    let block = MainnetBlock::decode(&mut slice).map_err(CaptureError::DecodeRawBlock)?;
    if !slice.is_empty() {
        return Err(CaptureError::TrailingRawBlockRlp);
    }
    Ok(block)
}
