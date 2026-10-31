use crate::capture::CaptureError;
use alloy_consensus::Header;
use alloy_hardforks::EthereumHardfork;
use evm2::SpecId;

pub(crate) fn mainnet_spec_for_header(header: &Header) -> Result<SpecId, CaptureError> {
    let hardfork = EthereumHardfork::from_chain_and_timestamp(
        alloy_chains::Chain::mainnet(),
        header.timestamp,
    );
    spec_for_hardfork(hardfork)
        .ok_or(CaptureError::UnsupportedHardfork { timestamp: header.timestamp })
}

const fn spec_for_hardfork(hardfork: Option<EthereumHardfork>) -> Option<SpecId> {
    Some(match hardfork {
        Some(EthereumHardfork::Frontier) => SpecId::FRONTIER,
        Some(EthereumHardfork::Homestead | EthereumHardfork::Dao) => SpecId::HOMESTEAD,
        Some(EthereumHardfork::Tangerine) => SpecId::TANGERINE,
        Some(EthereumHardfork::SpuriousDragon) => SpecId::SPURIOUS_DRAGON,
        Some(EthereumHardfork::Byzantium | EthereumHardfork::Constantinople) => SpecId::BYZANTIUM,
        Some(EthereumHardfork::Petersburg) => SpecId::PETERSBURG,
        Some(EthereumHardfork::Istanbul | EthereumHardfork::MuirGlacier) => SpecId::ISTANBUL,
        Some(EthereumHardfork::Berlin) => SpecId::BERLIN,
        Some(
            EthereumHardfork::London
            | EthereumHardfork::ArrowGlacier
            | EthereumHardfork::GrayGlacier,
        ) => SpecId::LONDON,
        Some(EthereumHardfork::Paris) => SpecId::MERGE,
        Some(EthereumHardfork::Shanghai) => SpecId::SHANGHAI,
        Some(EthereumHardfork::Cancun) => SpecId::CANCUN,
        Some(EthereumHardfork::Prague) => SpecId::PRAGUE,
        Some(
            EthereumHardfork::Osaka
            | EthereumHardfork::Bpo1
            | EthereumHardfork::Bpo2
            | EthereumHardfork::Bpo3
            | EthereumHardfork::Bpo4
            | EthereumHardfork::Bpo5,
        ) => SpecId::OSAKA,
        Some(EthereumHardfork::Amsterdam) => SpecId::AMSTERDAM,
        Some(_) | None => return None,
    })
}
