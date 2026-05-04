//! EVM precompiled contracts.

use crate::{
    evm::precompile::{PrecompileOutput as EvmPrecompileOutput, PrecompileProvider},
    interpreter::{Gas, InstrStop, SpecId},
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
pub(crate) mod utils;

/// EIP-7823 constants.
pub(crate) mod eip7823 {
    /// Each of the modexp length inputs must be less than or equal to 1024 bytes.
    pub(crate) const INPUT_SIZE_LIMIT: usize = 1024;
}

/// EIP-4844 constants.
pub(crate) mod eip4844 {
    #[allow(unused_imports)]
    pub(crate) use crate::precompiles::kzg_point_evaluation::VERSIONED_HASH_VERSION_KZG;
}

pub(crate) use interface::*;
pub use interface::{Crypto, PrecompileHalt};
#[allow(deprecated, unused_imports)]
pub(crate) use utils::calc_linear_cost_u32;
pub(crate) use utils::{calc_linear_cost, u64_to_address};

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

/// Ethereum precompile provider.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Precompiles<const SPEC: u8 = { SpecId::OSAKA as u8 }>;

impl<const SPEC: u8> Precompiles<SPEC> {
    const SPEC_ID: SpecId = match SpecId::try_from_u8(SPEC) {
        Some(spec_id) => spec_id,
        None => panic!("invalid EVM specification ID"),
    };

    fn map() -> &'static PrecompileMap {
        for_spec(Self::SPEC_ID)
    }

    fn run(
        f: fn(&[u8], &mut Gas) -> EthPrecompileResult,
        input: &[u8],
        gas: &mut Gas,
    ) -> Result<EvmPrecompileOutput, InstrStop> {
        let output = PrecompileOutput::from_eth_result(f(input, gas));
        match output.status {
            PrecompileStatus::Success => Ok(EvmPrecompileOutput::new(output.bytes)),
            PrecompileStatus::Revert => Err(InstrStop::PrecompileError),
            PrecompileStatus::Halt(PrecompileHalt::OutOfGas) => {
                gas.spend_all();
                Err(InstrStop::PrecompileOOG)
            }
            PrecompileStatus::Halt(_) => Err(InstrStop::PrecompileError),
        }
    }
}

impl<const SPEC: u8> PrecompileProvider for Precompiles<SPEC> {
    fn warm_addresses(&self) -> Vec<Address> {
        Self::map().keys().copied().collect()
    }

    fn execute(
        &mut self,
        address: Address,
        input: &[u8],
        gas: &mut Gas,
    ) -> Option<Result<EvmPrecompileOutput, InstrStop>> {
        let f = *Self::map().get(&address)?;
        Some(Self::run(f, input, gas))
    }
}

fn insert(precompiles: &mut PrecompileMap, address: u64, f: PrecompileFn) {
    precompiles.insert(u64_to_address(address), f);
}

fn for_spec(spec: SpecId) -> &'static PrecompileMap {
    static HOMESTEAD: OnceLock<PrecompileMap> = OnceLock::new();
    static BYZANTIUM: OnceLock<PrecompileMap> = OnceLock::new();
    static ISTANBUL: OnceLock<PrecompileMap> = OnceLock::new();
    static BERLIN: OnceLock<PrecompileMap> = OnceLock::new();
    static CANCUN: OnceLock<PrecompileMap> = OnceLock::new();
    static PRAGUE: OnceLock<PrecompileMap> = OnceLock::new();
    static OSAKA: OnceLock<PrecompileMap> = OnceLock::new();

    let lock = if spec.enables(SpecId::OSAKA) {
        &OSAKA
    } else if spec.enables(SpecId::PRAGUE) {
        &PRAGUE
    } else if spec.enables(SpecId::CANCUN) {
        &CANCUN
    } else if spec.enables(SpecId::BERLIN) {
        &BERLIN
    } else if spec.enables(SpecId::ISTANBUL) {
        &ISTANBUL
    } else if spec.enables(SpecId::BYZANTIUM) {
        &BYZANTIUM
    } else {
        &HOMESTEAD
    };

    lock.get_or_init(|| {
        let mut precompiles = map::HashMap::default();

        insert(&mut precompiles, 1, secp256k1::run);
        insert(&mut precompiles, 2, hash::run_sha256);
        insert(&mut precompiles, 3, hash::run_ripemd160);
        insert(&mut precompiles, 4, identity::run);

        if spec.enables(SpecId::BYZANTIUM) {
            insert(&mut precompiles, 5, modexp::run_byzantium);
            insert(&mut precompiles, 6, bn254::add::run_byzantium);
            insert(&mut precompiles, 7, bn254::mul::run_byzantium);
            insert(&mut precompiles, 8, bn254::pair::run_byzantium);
        }

        if spec.enables(SpecId::ISTANBUL) {
            insert(&mut precompiles, 6, bn254::add::run_istanbul);
            insert(&mut precompiles, 7, bn254::mul::run_istanbul);
            insert(&mut precompiles, 8, bn254::pair::run_istanbul);
            insert(&mut precompiles, 9, blake2::run);
        }

        if spec.enables(SpecId::BERLIN) {
            insert(&mut precompiles, 5, modexp::run_berlin);
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
            insert(&mut precompiles, 5, modexp::run_osaka);
            insert(&mut precompiles, secp256r1::P256VERIFY_ADDRESS, secp256r1::run_osaka);
        }

        precompiles
    })
}
