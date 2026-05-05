//! EVM precompiled contracts.

use crate::{
    SpecId,
    evm::precompile::{PrecompileProvider, PrecompileStatus},
    interpreter::{Gas, InstrStop},
    once_lock::OnceLock,
};
use alloc::{boxed::Box, vec::Vec};
use alloy_primitives::{
    Address,
    map::{self, HashMap},
};

pub(crate) mod blake2;
pub(crate) mod bls12_381;
pub(crate) mod bls12_381_const;
pub(crate) mod bls12_381_utils;
pub(crate) mod bn254;
pub(crate) mod hash;
pub(crate) mod identity;
pub(crate) mod interface;
pub(crate) mod kzg_point_evaluation;
pub(crate) mod modexp;
pub(crate) mod secp256k1;
pub(crate) mod secp256r1;

/// EIP-7823 constants.
pub(crate) mod eip7823 {
    /// Each of the modexp length inputs must be less than or equal to 1024 bytes.
    pub(crate) const INPUT_SIZE_LIMIT: usize = 1024;
}

pub(crate) use crate::{
    evm::precompile::PrecompileOutput,
    utils::{calc_linear_cost, u64_to_address},
};
pub(crate) use interface::*;
pub use interface::{Crypto, PrecompileHalt};

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

type PrecompileFn = fn(&[u8], &mut Gas) -> EthPrecompileResult;
type PrecompileMap = HashMap<Address, PrecompileFn>;

/// Base Ethereum precompile provider.
#[derive(Clone, Copy, Debug)]
pub struct BasePrecompiles {
    map: &'static PrecompileMap,
}

impl BasePrecompiles {
    /// Creates a precompile provider for a base EVM specification.
    #[inline]
    pub fn base(spec_id: SpecId) -> Self {
        Self { map: for_spec(spec_id) }
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
        self.map.keys().copied().collect()
    }

    #[inline]
    fn execute(
        &mut self,
        address: Address,
        input: &[u8],
        gas: &mut Gas,
    ) -> Option<Result<PrecompileOutput, InstrStop>> {
        let f = *self.map.get(&address)?;
        Some(Self::run(f, input, gas))
    }
}

fn insert(precompiles: &mut PrecompileMap, address: u64, f: PrecompileFn) {
    precompiles.insert(u64_to_address(address), f);
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
        let mut precompiles = map::HashMap::default();

        {
            insert(&mut precompiles, 0x01, secp256k1::run);
            insert(&mut precompiles, 0x02, hash::run_sha256);
            insert(&mut precompiles, 0x03, hash::run_ripemd160);
            insert(&mut precompiles, 0x04, identity::run);
        }

        if spec.enables(SpecId::BYZANTIUM) {
            insert(&mut precompiles, 0x05, modexp::run_byzantium);
            insert(&mut precompiles, 0x06, bn254::add::run_byzantium);
            insert(&mut precompiles, 0x07, bn254::mul::run_byzantium);
            insert(&mut precompiles, 0x08, bn254::pair::run_byzantium);
        }

        if spec.enables(SpecId::ISTANBUL) {
            insert(&mut precompiles, 0x06, bn254::add::run_istanbul);
            insert(&mut precompiles, 0x07, bn254::mul::run_istanbul);
            insert(&mut precompiles, 0x08, bn254::pair::run_istanbul);
            insert(&mut precompiles, 0x09, blake2::run);
        }

        if spec.enables(SpecId::BERLIN) {
            insert(&mut precompiles, 0x05, modexp::run_berlin);
        }

        if spec.enables(SpecId::CANCUN) {
            insert(&mut precompiles, 0x0a, kzg_point_evaluation::run);
        }

        if spec.enables(SpecId::PRAGUE) {
            insert(&mut precompiles, 0x0b, bls12_381::g1_add::run);
            insert(&mut precompiles, 0x0c, bls12_381::g1_msm::run);
            insert(&mut precompiles, 0x0d, bls12_381::g2_add::run);
            insert(&mut precompiles, 0x0e, bls12_381::g2_msm::run);
            insert(&mut precompiles, 0x0f, bls12_381::pairing::run);
            insert(&mut precompiles, 0x10, bls12_381::map_fp_to_g1::run);
            insert(&mut precompiles, 0x11, bls12_381::map_fp2_to_g2::run);
        }

        if spec.enables(SpecId::OSAKA) {
            insert(&mut precompiles, 0x05, modexp::run_osaka);
            insert(&mut precompiles, 0xff, secp256r1::run_osaka);
        }

        precompiles
    })
}
