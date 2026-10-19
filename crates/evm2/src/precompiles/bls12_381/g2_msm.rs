//! BLS12-381 G2 msm precompile. More details in [`run`]

use super::utils::{pad_g2_point, remove_g2_padding};
use crate::{
    interpreter::GasTracker,
    precompiles::{
        PrecompileHalt, PrecompileOutput, PrecompileResult,
        bls12_381_const::{
            DISCOUNT_TABLE_G2_MSM, G2_MSM_BASE_GAS_FEE, G2_MSM_INPUT_LENGTH, SCALAR_LENGTH,
        },
        bls12_381_utils::msm_required_gas,
    },
};

/// Implements EIP-2537 G2MSM precompile.
/// G2 multi-scalar-multiplication call expects `288*k` bytes as an input that is interpreted
/// as byte concatenation of `k` slices each of them being a byte concatenation
/// of encoding of G2 point (`256` bytes) and encoding of a scalar value (`32`
/// bytes).
/// Output is an encoding of multi-scalar-multiplication operation result - single G2
/// point (`256` bytes).
/// See also: <https://eips.ethereum.org/EIPS/eip-2537#abi-for-g2-multiexponentiation>
pub fn run(input: &[u8], gas: &mut GasTracker) -> PrecompileResult {
    let input_len = input.len();
    if input_len == 0 || !input_len.is_multiple_of(G2_MSM_INPUT_LENGTH) {
        return Err(PrecompileHalt::Bls12381G2MsmInputLength.into());
    }

    let input_chunks = input.as_chunks::<G2_MSM_INPUT_LENGTH>().0;
    let k = input_chunks.len();
    let required_gas = msm_required_gas(k, &DISCOUNT_TABLE_G2_MSM, G2_MSM_BASE_GAS_FEE);
    gas.spend(required_gas)?;

    let mut valid_pairs_iter = input_chunks.iter().map(|pair| {
        let (padded_g2, scalar_array) = pair.split_last_chunk::<SCALAR_LENGTH>().unwrap();

        // Remove padding from G2 point - this validates padding format
        let [x_0, x_1, y_0, y_1] = remove_g2_padding(padded_g2)?;

        Ok(((*x_0, *x_1, *y_0, *y_1), *scalar_array))
    });

    let unpadded_result = crate::precompiles::crypto().bls12_381_g2_msm(&mut valid_pairs_iter)?;

    // Pad the result for EVM compatibility
    let padded_result = pad_g2_point(&unpadded_result);

    Ok(PrecompileOutput::new(padded_result.into()))
}

#[cfg(test)]
mod test {
    use super::*;
    use alloy_primitives::{U256, hex};

    /// Scalars are reduced modulo the subgroup order, so `m * r + k` acts like `k`.
    #[test]
    fn g2_msm_scalars_reduced_mod_r() {
        let r = U256::from_be_bytes(hex!(
            "73eda753299d7d483339d80809a1d80553bda402fffe5bfeffffffff00000001"
        ));
        let point = hex!(
            "00000000000000000000000000000000024aa2b2f08f0a91260805272dc51051c6e47ad4fa403b02b4510b647ae3d1770bac0326a805bbefd48056c8c121bdb80000000000000000000000000000000013e02b6052719f607dacd3a088274f65596bd0d09920b61ab5da61bbdc7f5049334cf11213945d57e5ac7d055d042b7e000000000000000000000000000000000ce5d527727d6e118cc9cdc6da2e351aadfd9baa8cbdd3a76d429a695160d12c923ac9cc3baca289e193548608b82801000000000000000000000000000000000606c4a02ea734cc32acd2b02bc28b99cb3e287e85a763af267492ab572e99ab3f370d275cec1da1aaa9075ff05f79be"
        );
        let msm = |pairs: &[(u64, u64)]| {
            let input = pairs
                .iter()
                .flat_map(|&(multiple, k)| {
                    let scalar = r * U256::from(multiple) + U256::from(k);
                    [point.as_slice(), scalar.to_be_bytes::<32>().as_slice()].concat()
                })
                .collect::<Vec<_>>();
            run(&input, &mut GasTracker::new(u64::MAX)).unwrap().bytes().to_vec()
        };

        let infinity = vec![0u8; 256];
        assert_eq!(msm(&[(1, 0)]), infinity);
        assert_eq!(msm(&[(2, 0), (1, 0)]), infinity);
        for multiple in 1..=2 {
            assert_eq!(msm(&[(multiple, 5)]), msm(&[(0, 5)]));
            assert_eq!(msm(&[(multiple, 5), (0, 3)]), msm(&[(0, 5), (0, 3)]));
            assert_eq!(msm(&[(multiple, 0), (0, 3)]), msm(&[(0, 3)]));
        }
        assert_ne!(msm(&[(0, 5)]), infinity);
        assert_ne!(msm(&[(0, 5)]), msm(&[(0, 6)]));
    }
}
