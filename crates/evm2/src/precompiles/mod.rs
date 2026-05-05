//! EVM precompiled contracts.

use crate::{
    SpecId,
    evm::precompile::{PrecompileProvider, PrecompileStatus},
    interpreter::{Gas, InstrStop},
    once_lock::OnceLock,
};
use alloc::{borrow::Cow, boxed::Box, vec::Vec};
use alloy_primitives::Address;

pub mod blake2;
pub mod bls12_381;
pub mod bls12_381_const;
pub mod bls12_381_utils;
pub mod bn254;
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

pub mod interface;
pub(crate) use interface::*;
pub use interface::{Crypto, EthPrecompileResult, PrecompileHalt};

pub(crate) use crate::evm::precompile::PrecompileOutput;

// silence arkworks lint as bn impl will be used as default if both are enabled.
cfg_if::cfg_if! {
    if #[cfg(feature = "bn")] {
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

/// Global crypto provider instance.
static CRYPTO: OnceLock<Box<dyn Crypto>> = OnceLock::new();

/// Install a custom crypto provider globally.
pub fn install_crypto<C: Crypto + 'static>(crypto: C) -> bool {
    CRYPTO.set(Box::new(crypto)).is_ok()
}

/// Get the installed crypto provider, or the default if none is installed.
pub fn crypto() -> &'static dyn Crypto {
    CRYPTO.get_or_init(|| Box::new(DefaultCrypto)).as_ref()
}

/// Base Ethereum precompile provider.
#[derive(Clone, Debug)]
pub struct BasePrecompiles {
    map: Cow<'static, PrecompileMap>,
}

impl BasePrecompiles {
    /// Creates a precompile provider from a static precompile map.
    #[inline]
    pub const fn new(map: Cow<'static, PrecompileMap>) -> Self {
        Self { map }
    }

    /// Creates a precompile provider for a base EVM specification.
    #[inline]
    pub fn base(spec_id: SpecId) -> Self {
        Self::new(Cow::Borrowed(for_spec(spec_id)))
    }

    /// Creates a precompile map from precompile descriptors.
    #[inline]
    pub fn map(precompiles: impl IntoIterator<Item = Precompile>) -> PrecompileMap {
        PrecompileMap::from_precompiles(precompiles)
    }

    fn run(
        f: fn(&[u8], &mut Gas) -> EthPrecompileResult,
        input: &[u8],
        gas: &mut Gas,
    ) -> Result<PrecompileOutput, InstrStop> {
        let output = match f(input, gas) {
            Ok(output) => output,
            Err(PrecompileHalt::OutOfGas) => {
                gas.spend_all();
                return Err(InstrStop::PrecompileOOG);
            }
            Err(_) => return Err(InstrStop::PrecompileError),
        };
        match output.status() {
            PrecompileStatus::Success => Ok(output),
            PrecompileStatus::Revert => Err(InstrStop::PrecompileError),
            PrecompileStatus::Halt(PrecompileHalt::OutOfGas) => {
                gas.spend_all();
                Err(InstrStop::PrecompileOOG)
            }
            PrecompileStatus::Halt(_) => Err(InstrStop::PrecompileError),
        }
    }
}

impl PrecompileProvider for BasePrecompiles {
    #[inline]
    fn warm_addresses(&self) -> Vec<Address> {
        self.map.as_ref().addresses().collect()
    }

    #[inline]
    fn execute(
        &mut self,
        address: Address,
        input: &[u8],
        gas: &mut Gas,
    ) -> Option<Result<PrecompileOutput, InstrStop>> {
        let precompile = self.map.as_ref().get_data(&address)?;
        Some(Self::run(precompile.run(), input, gas))
    }
}

fn for_spec(spec: SpecId) -> &'static PrecompileMap {
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
            precompiles.extend([MODEXP_OSAKA, SECP256R1_VERIFY]);
        }

        precompiles.shrink_to_fit();

        precompiles
    })
}
