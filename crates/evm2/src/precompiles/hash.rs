//! Hash precompiles, it contains SHA-256 and RIPEMD-160 hash precompiles
//! More details in [`run_sha256`] and [`run_ripemd160`]
use super::calc_linear_cost;
use crate::precompiles::{EthPrecompileOutput, EthPrecompileResult, Gas};

/// Computes the SHA-256 hash of the input data
///
/// This function follows specifications defined in the following references:
/// - [Ethereum Yellow Paper](https://ethereum.github.io/yellowpaper/paper.pdf)
/// - [Solidity Documentation on Mathematical and Cryptographic Functions](https://docs.soliditylang.org/en/develop/units-and-global-variables.html#mathematical-and-cryptographic-functions)
/// - [ 0x02](https://etherscan.io/address/0000000000000000000000000000000000000002)
pub(crate) fn run_sha256(input: &[u8], gas: &mut Gas) -> EthPrecompileResult {
    let cost = calc_linear_cost(input.len(), 60, 12);
    gas.spend(cost)?;
    let output = gas.crypto().sha256(input);
    Ok(EthPrecompileOutput::new(output.to_vec().into()))
}

/// Computes the RIPEMD-160 hash of the input data
///
/// This function follows specifications defined in the following references:
/// - [Ethereum Yellow Paper](https://ethereum.github.io/yellowpaper/paper.pdf)
/// - [Solidity Documentation on Mathematical and Cryptographic Functions](https://docs.soliditylang.org/en/develop/units-and-global-variables.html#mathematical-and-cryptographic-functions)
/// - [ 03](https://etherscan.io/address/0000000000000000000000000000000000000003)
pub(crate) fn run_ripemd160(input: &[u8], gas: &mut Gas) -> EthPrecompileResult {
    let gas_used = calc_linear_cost(input.len(), 600, 120);
    gas.spend(gas_used)?;
    let output = gas.crypto().ripemd160(input);
    Ok(EthPrecompileOutput::new(output.to_vec().into()))
}
