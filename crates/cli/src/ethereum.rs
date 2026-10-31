use crate::capture::CaptureError;
use alloy_consensus::Header;
use alloy_hardforks::EthereumHardfork;
use evm2::SpecId;

pub(crate) fn mainnet_spec_for_header(header: &Header) -> Result<SpecId, CaptureError> {
    let hardfork = EthereumHardfork::from_chain_and_timestamp(
        alloy_chains::Chain::mainnet(),
        header.timestamp,
    );
    hardfork
        .and_then(spec_for_hardfork)
        .ok_or(CaptureError::UnsupportedHardfork { timestamp: header.timestamp })
}

const fn spec_for_hardfork(hardfork: EthereumHardfork) -> Option<SpecId> {
    Some(match hardfork {
        EthereumHardfork::Frontier => SpecId::FRONTIER,
        EthereumHardfork::Homestead | EthereumHardfork::Dao => SpecId::HOMESTEAD,
        EthereumHardfork::Tangerine => SpecId::TANGERINE,
        EthereumHardfork::SpuriousDragon => SpecId::SPURIOUS_DRAGON,
        EthereumHardfork::Byzantium | EthereumHardfork::Constantinople => SpecId::BYZANTIUM,
        EthereumHardfork::Petersburg => SpecId::PETERSBURG,
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
        EthereumHardfork::Amsterdam => SpecId::AMSTERDAM,
        _ => return None,
    })
}
