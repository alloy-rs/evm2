//! EVM precompiled contracts.

use crate::{
    BaseEvmTypes, Evm, SpecId,
    evm::precompile::PrecompileProvider,
    interpreter::{GasTracker, Message},
    once_lock::OnceLock,
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
pub struct Precompiles {
    map: Cow<'static, PrecompileMap>,
}

impl Precompiles {
    /// Creates a precompile provider from a static precompile map.
    #[inline]
    pub const fn new(map: Cow<'static, PrecompileMap>) -> Self {
        Self { map }
    }

    /// Creates a precompile provider for a base EVM specification.
    #[inline]
    pub fn base(spec_id: SpecId) -> Self {
        Self::new(Cow::Borrowed(base_precompiles(spec_id)))
    }

    /// Creates a precompile map from precompile descriptors.
    #[inline]
    pub fn map(precompiles: impl IntoIterator<Item = Precompile>) -> PrecompileMap {
        PrecompileMap::from_precompiles(precompiles)
    }

    /// Returns the underlying precompile map.
    #[inline]
    pub fn as_map(&self) -> &PrecompileMap {
        self.map.as_ref()
    }

    /// Returns the underlying precompile map mutably.
    #[inline]
    pub fn as_map_mut(&mut self) -> &mut PrecompileMap {
        self.map.to_mut()
    }
}

impl PrecompileProvider<BaseEvmTypes> for Precompiles {
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
        _evm: &mut Evm<BaseEvmTypes>,
        message: &Message,
        gas: &mut GasTracker,
    ) -> Option<PrecompileResult> {
        let precompile = self.map.as_ref().get_data(&message.code_address)?;
        Some(precompile.run()(message.input.as_ref(), gas))
    }
}

fn base_precompiles(spec: SpecId) -> &'static PrecompileMap {
    static CACHE: [OnceLock<PrecompileMap>; 7] = [const { OnceLock::new() }; 7];

    let index = match spec {
        SpecId::FRONTIER | SpecId::HOMESTEAD | SpecId::TANGERINE | SpecId::SPURIOUS_DRAGON => 0,
        SpecId::BYZANTIUM | SpecId::PETERSBURG => 1,
        SpecId::ISTANBUL => 2,
        SpecId::BERLIN | SpecId::LONDON | SpecId::MERGE | SpecId::SHANGHAI => 3,
        SpecId::CANCUN => 4,
        SpecId::PRAGUE => 5,
        SpecId::OSAKA | SpecId::AMSTERDAM => 6,
    };
    CACHE[index].get_or_init(|| {
        let mut precompiles = PrecompileMap::new();

        {
            precompiles.extend([SECP256K1_ECRECOVER, SHA256, RIPEMD160, IDENTITY]);
        }

        if spec.enables(SpecId::BYZANTIUM) {
            precompiles.extend([
                MODEXP_BYZANTIUM,
                BN254_ADD_BYZANTIUM,
                BN254_MUL_BYZANTIUM,
                BN254_PAIR_BYZANTIUM,
            ]);
        }

        if spec.enables(SpecId::ISTANBUL) {
            precompiles.extend([
                BN254_ADD_ISTANBUL,
                BN254_MUL_ISTANBUL,
                BN254_PAIR_ISTANBUL,
                BLAKE2F,
            ]);
        }

        if spec.enables(SpecId::BERLIN) {
            precompiles.extend([MODEXP_BERLIN]);
        }

        if spec.enables(SpecId::CANCUN) {
            precompiles.extend([KZG_POINT_EVALUATION]);
        }

        if spec.enables(SpecId::PRAGUE) {
            precompiles.extend([
                BLS12_381_G1_ADD,
                BLS12_381_G1_MSM,
                BLS12_381_G2_ADD,
                BLS12_381_G2_MSM,
                BLS12_381_PAIRING,
                BLS12_381_MAP_FP_TO_G1,
                BLS12_381_MAP_FP2_TO_G2,
            ]);
        }

        if spec.enables(SpecId::OSAKA) {
            precompiles.extend([MODEXP_OSAKA, P256VERIFY_OSAKA]);
        }

        precompiles.shrink_to_fit();

        precompiles
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::address;

    #[test]
    fn provider_helpers_clone_static_base_on_mutation() {
        let identity = IDENTITY.address();
        let moved = address!("0x0000000000000000000000000000000000001000");
        let mut precompiles = Precompiles::base(SpecId::BERLIN);

        precompiles.as_map_mut().move_precompiles([(identity, moved)]).unwrap();

        assert!(!precompiles.as_map().contains(&identity));
        assert!(precompiles.as_map().contains(&moved));
        assert!(Precompiles::base(SpecId::BERLIN).as_map().contains(&identity));
    }
}
