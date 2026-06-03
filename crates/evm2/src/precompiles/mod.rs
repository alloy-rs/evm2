//! EVM precompiled contracts.

use crate::{
    Evm, EvmTypes, SpecId,
    evm::precompile::PrecompileProvider,
    interpreter::{GasTracker, Message},
};
use alloc::{borrow::Cow, vec::Vec};
use alloy_primitives::Address;

pub mod blake2;
pub mod bls12_381;
pub mod bls12_381_const;
pub mod bls12_381_utils;
pub mod bn254;
mod crypto;
pub use crypto::{Crypto, DefaultCrypto, crypto, install_crypto};

pub mod hash;
pub mod identity;
pub mod kzg_point_evaluation;
pub mod modexp;
pub mod secp256k1;
pub mod secp256r1;

mod id;
pub use id::PrecompileId;

mod table;
pub use table::*;

/// EIP-7823 constants.
pub(crate) mod eip7823 {
    /// Each of the modexp length inputs must be less than or equal to 1024 bytes.
    pub(crate) const INPUT_SIZE_LIMIT: usize = 1024;
}

mod result;
pub use result::{AnyError, PrecompileError, PrecompileHalt, PrecompileResult};

pub(crate) use crate::{
    evm::precompile::PrecompileOutput,
    once_lock::OnceLock,
    utils::{calc_linear_cost, u64_to_address},
};

// Silence backend dependency lints when another backend takes precedence.
cfg_if::cfg_if! {
    if #[cfg(feature = "bn")] {
        use bn as _;
        use ark_bn254 as _;
        use ark_ff as _;
        use ark_ec as _;
        use ark_serialize as _;
    }
}

use arrayref as _;

// silence arkworks-bls12-381 lint as blst will be used as default if both are enabled.
cfg_if::cfg_if! {
    if #[cfg(feature = "blst")] {
        use ark_bls12_381 as _;
        use ark_ff as _;
        use ark_ec as _;
        use ark_serialize as _;
    }
}

// silence aurora-engine-modexp if gmp is enabled
#[cfg(feature = "gmp")]
use aurora_engine_modexp as _;

// silence p256 lint as aws-lc-rs will be used if both are enabled.
#[cfg(feature = "p256-aws-lc-rs")]
use p256 as _;

/// Default precompile provider.
#[derive(Clone, Debug)]
pub struct Precompiles<T: EvmTypes = crate::BaseEvmTypes> {
    map: Cow<'static, PrecompileMap<T>>,
}

impl<T: EvmTypes> Precompiles<T> {
    /// Creates a precompile provider from a static precompile map.
    #[inline]
    pub const fn new(map: Cow<'static, PrecompileMap<T>>) -> Self {
        Self { map }
    }

    /// Creates a precompile provider for a base EVM specification.
    #[inline]
    pub fn base(spec_id: SpecId) -> Self {
        Self::new(Cow::Owned(base_precompiles(spec_id)))
    }

    /// Creates a precompile map from precompile descriptors.
    #[inline]
    pub fn map(precompiles: impl IntoIterator<Item = Precompile<T>>) -> PrecompileMap<T> {
        PrecompileMap::from_precompiles(precompiles)
    }

    /// Returns the underlying precompile map.
    #[inline]
    pub fn as_map(&self) -> &PrecompileMap<T> {
        self.map.as_ref()
    }

    /// Returns the underlying precompile map mutably.
    #[inline]
    pub fn as_map_mut(&mut self) -> &mut PrecompileMap<T> {
        self.map.to_mut()
    }
}

impl<T: EvmTypes> PrecompileProvider<T> for Precompiles<T> {
    #[inline]
    fn warm_addresses(&self) -> Vec<Address> {
        self.map.as_ref().addresses().collect()
    }

    #[inline]
    fn contains(&self, address: &Address) -> bool {
        self.map.as_ref().contains(address)
    }

    #[inline]
    fn execute(
        &mut self,
        _evm: &mut Evm<T>,
        message: &Message<T>,
        gas: &mut GasTracker,
    ) -> Option<PrecompileResult> {
        let precompile = self.map.as_ref().get_data(&message.code_address)?;
        Some(precompile.execute(message, gas))
    }
}

fn base_precompiles<T: EvmTypes>(spec: SpecId) -> PrecompileMap<T> {
    let mut precompiles = PrecompileMap::new();

    {
        precompiles.extend([
            table::SECP256K1_ECRECOVER::precompile(),
            table::SHA256::precompile(),
            table::RIPEMD160::precompile(),
            table::IDENTITY::precompile(),
        ]);
    }

    if spec.enables(SpecId::BYZANTIUM) {
        precompiles.extend([
            table::MODEXP_BYZANTIUM::precompile(),
            table::BN254_ADD_BYZANTIUM::precompile(),
            table::BN254_MUL_BYZANTIUM::precompile(),
            table::BN254_PAIR_BYZANTIUM::precompile(),
        ]);
    }

    if spec.enables(SpecId::ISTANBUL) {
        precompiles.extend([
            table::BN254_ADD_ISTANBUL::precompile(),
            table::BN254_MUL_ISTANBUL::precompile(),
            table::BN254_PAIR_ISTANBUL::precompile(),
            table::BLAKE2F::precompile(),
        ]);
    }

    if spec.enables(SpecId::BERLIN) {
        precompiles.extend([table::MODEXP_BERLIN::precompile()]);
    }

    if spec.enables(SpecId::CANCUN) {
        precompiles.extend([table::KZG_POINT_EVALUATION::precompile()]);
    }

    if spec.enables(SpecId::PRAGUE) {
        precompiles.extend([
            table::BLS12_381_G1_ADD::precompile(),
            table::BLS12_381_G1_MSM::precompile(),
            table::BLS12_381_G2_ADD::precompile(),
            table::BLS12_381_G2_MSM::precompile(),
            table::BLS12_381_PAIRING::precompile(),
            table::BLS12_381_MAP_FP_TO_G1::precompile(),
            table::BLS12_381_MAP_FP2_TO_G2::precompile(),
        ]);
    }

    if spec.enables(SpecId::OSAKA) {
        precompiles
            .extend([table::MODEXP_OSAKA::precompile(), table::P256VERIFY_OSAKA::precompile()]);
    }

    precompiles.shrink_to_fit();

    precompiles
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::address;

    #[test]
    fn provider_helpers_mutate_base_map() {
        let identity = IDENTITY.address();
        let moved = address!("0x0000000000000000000000000000000000001000");
        let mut precompiles = Precompiles::<crate::BaseEvmTypes>::base(SpecId::BERLIN);

        precompiles.as_map_mut().move_precompiles([(identity, moved)]).unwrap();

        assert!(!precompiles.as_map().contains(&identity));
        assert!(precompiles.as_map().contains(&moved));
        assert!(
            Precompiles::<crate::BaseEvmTypes>::base(SpecId::BERLIN).as_map().contains(&identity)
        );
    }
}
