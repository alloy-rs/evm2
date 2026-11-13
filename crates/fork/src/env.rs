//! The execution environment of a fork block.

use crate::{ForkError, ForkResult};
use alloy_chains::Chain;
use alloy_consensus::BlockHeader;
use alloy_eips::{BlockId, eip7840::BlobParams};
use alloy_hardforks::EthereumHardfork;
use alloy_primitives::{B256, U256};
use alloy_provider::{
    Network, Provider,
    network::{BlockResponse, primitives::HeaderResponse},
};
use evm2::{SpecId, env::BlockEnvExt};
use std::sync::Arc;

/// The chain, specification and block environment a fork executes with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForkEnv {
    /// Chain ID of the forked chain.
    pub chain_id: u64,
    /// Specification active at the fork block, or [`SpecId::DEFAULT`] on chains whose hardforks
    /// are unknown.
    pub spec_id: SpecId,
    /// Block environment of the fork block.
    pub block: BlockEnvExt,
    /// Number of the fork block.
    pub block_number: u64,
    /// Hash of the fork block.
    pub block_hash: B256,
}

impl ForkEnv {
    /// Builds the environment of `header` on `chain_id`.
    pub fn from_header<H: HeaderResponse>(chain_id: u64, header: &H) -> Self {
        let hardfork = EthereumHardfork::from_chain_and_timestamp(
            Chain::from_id(chain_id),
            header.timestamp(),
        );
        Self {
            chain_id,
            spec_id: hardfork.map_or(SpecId::DEFAULT, spec_id),
            block: block_env(header, hardfork.and_then(blob_params)),
            block_number: header.number(),
            block_hash: header.hash(),
        }
    }
}

/// Fetches the environment of `block` and the block itself.
pub async fn fork_env<N: Network, P: Provider<N>>(
    provider: &P,
    block: BlockId,
) -> ForkResult<(ForkEnv, N::BlockResponse)> {
    let (chain_id, response) = tokio::try_join!(
        async { provider.get_chain_id().await.map_err(|err| ForkError::GetChainId(Arc::new(err))) },
        async {
            match provider.get_block(block).await {
                Ok(Some(block)) => Ok(block),
                Ok(None) => Err(ForkError::BlockNotFound(block)),
                Err(err) => Err(ForkError::GetFullBlock(block, Arc::new(err))),
            }
        },
    )?;
    Ok((ForkEnv::from_header(chain_id, response.header()), response))
}

/// Builds a block environment from a header, deriving the blob base fee with `blob_params`.
pub fn block_env<H: BlockHeader>(header: &H, blob_params: Option<BlobParams>) -> BlockEnvExt {
    BlockEnvExt {
        number: U256::from(header.number()),
        beneficiary: header.beneficiary(),
        timestamp: U256::from(header.timestamp()),
        gas_limit: U256::from(header.gas_limit()),
        basefee: U256::from(header.base_fee_per_gas().unwrap_or_default()),
        difficulty: header.difficulty(),
        prevrandao: header.mix_hash().unwrap_or_default().into(),
        blob_basefee: blob_params
            .zip(header.excess_blob_gas())
            .map_or(U256::ZERO, |(params, excess)| U256::from(params.calc_blob_fee(excess))),
        slot_num: U256::from(header.slot_number().unwrap_or_default()),
        ..Default::default()
    }
}

/// Returns the evm2 specification of an Ethereum hardfork.
pub const fn spec_id(hardfork: EthereumHardfork) -> SpecId {
    match hardfork {
        EthereumHardfork::Frontier => SpecId::FRONTIER,
        EthereumHardfork::Homestead | EthereumHardfork::Dao => SpecId::HOMESTEAD,
        EthereumHardfork::Tangerine => SpecId::TANGERINE,
        EthereumHardfork::SpuriousDragon => SpecId::SPURIOUS_DRAGON,
        EthereumHardfork::Byzantium => SpecId::BYZANTIUM,
        EthereumHardfork::Constantinople | EthereumHardfork::Petersburg => SpecId::PETERSBURG,
        EthereumHardfork::Istanbul | EthereumHardfork::MuirGlacier => SpecId::ISTANBUL,
        EthereumHardfork::Berlin => SpecId::BERLIN,
        EthereumHardfork::London
        | EthereumHardfork::ArrowGlacier
        | EthereumHardfork::GrayGlacier => SpecId::LONDON,
        EthereumHardfork::Paris => SpecId::MERGE,
        EthereumHardfork::Shanghai => SpecId::SHANGHAI,
        EthereumHardfork::Cancun => SpecId::CANCUN,
        EthereumHardfork::Prague => SpecId::PRAGUE,
        EthereumHardfork::Osaka
        | EthereumHardfork::Bpo1
        | EthereumHardfork::Bpo2
        | EthereumHardfork::Bpo3
        | EthereumHardfork::Bpo4
        | EthereumHardfork::Bpo5 => SpecId::OSAKA,
        _ => SpecId::AMSTERDAM,
    }
}

/// Returns the blob parameters of an Ethereum hardfork, `None` before Cancun.
pub const fn blob_params(hardfork: EthereumHardfork) -> Option<BlobParams> {
    Some(match hardfork {
        EthereumHardfork::Cancun => BlobParams::cancun(),
        EthereumHardfork::Prague => BlobParams::prague(),
        EthereumHardfork::Osaka => BlobParams::osaka(),
        EthereumHardfork::Bpo1 => BlobParams::bpo1(),
        EthereumHardfork::Bpo2
        | EthereumHardfork::Bpo3
        | EthereumHardfork::Bpo4
        | EthereumHardfork::Bpo5
        | EthereumHardfork::Amsterdam => BlobParams::bpo2(),
        _ if (hardfork as u32) < (EthereumHardfork::Cancun as u32) => return None,
        _ => BlobParams::bpo2(),
    })
}
