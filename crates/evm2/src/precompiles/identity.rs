//! Identity precompile returns
use super::calc_linear_cost;
use crate::precompiles::{
    EthPrecompileOutput, EthPrecompileResult, Gas, Precompile, PrecompileId, eth_precompile_fn,
};
use alloy_primitives::Bytes;

eth_precompile_fn!(identity_precompile, identity_run);

/// Address of the identity precompile.
pub(crate) const FUN: Precompile = Precompile::new(
    PrecompileId::Identity,
    crate::precompiles::u64_to_address(4),
    identity_precompile,
);

/// The base cost of the operation
pub(crate) const IDENTITY_BASE: u64 = 15;
/// The cost per word
pub(crate) const IDENTITY_PER_WORD: u64 = 3;

/// Takes the input bytes, copies them, and returns it as the output.
///
/// See: <https://ethereum.github.io/yellowpaper/paper.pdf>
///
/// See: <https://etherscan.io/address/0000000000000000000000000000000000000004>
pub(crate) fn identity_run(input: &[u8], gas: &mut Gas) -> EthPrecompileResult {
    let gas_used = calc_linear_cost(input.len(), IDENTITY_BASE, IDENTITY_PER_WORD);
    gas.spend(gas_used)?;
    Ok(EthPrecompileOutput::new(Bytes::copy_from_slice(input)))
}
