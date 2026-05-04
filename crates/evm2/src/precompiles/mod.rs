//! EVM precompiled contracts.
#![allow(dead_code, unused_imports)]

use alloc::boxed::Box;

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
    pub(crate) use crate::precompiles::kzg_point_evaluation::VERSIONED_HASH_VERSION_KZG;
}

use alloy_primitives::Address;

pub(crate) use interface::*;
pub use interface::{Crypto, PrecompileHalt};
#[allow(deprecated)]
pub(crate) use utils::calc_linear_cost_u32;
pub(crate) use utils::{calc_linear_cost, u64_to_address};

use crate::{
    evm::precompile::{PrecompileOutput as EvmPrecompileOutput, PrecompileProvider},
    interpreter::{InstrStop, SpecId},
    once_lock::OnceLock,
};

// silence arkworks lint as bn impl will be used as default if both are enabled.
cfg_if::cfg_if! {
    if #[cfg(feature = "bn")]{
        use ark_bn254 as _;
        use ark_ff as _;
        use ark_ec as _;
        use ark_serialize as _;
    }
}

use arrayref as _;

// silence arkworks-bls12-381 lint as blst will be used as default if both are enabled.
cfg_if::cfg_if! {
    if #[cfg(feature = "blst")]{
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

/// Ethereum precompile provider.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Precompiles<const SPEC: u8 = { SpecId::OSAKA as u8 }>;

impl<const SPEC: u8> Precompiles<SPEC> {
    const SPEC_ID: SpecId = match SpecId::try_from_u8(SPEC) {
        Some(spec_id) => spec_id,
        None => panic!("invalid EVM specification ID"),
    };

    fn address_value(address: Address) -> Option<u64> {
        let bytes = address.as_slice();
        if bytes[..12].iter().any(|&byte| byte != 0) {
            return None;
        }
        Some(u64::from_be_bytes(bytes[12..].try_into().unwrap()))
    }

    fn run(
        f: fn(&[u8], &mut Gas) -> EthPrecompileResult,
        input: &[u8],
        evm_gas: &mut crate::interpreter::Gas,
    ) -> Result<EvmPrecompileOutput, InstrStop> {
        let mut gas = Gas::from_evm_gas(evm_gas);
        let result = f(input, &mut gas);
        gas.apply_to_evm_gas(evm_gas);
        match result {
            Ok(output) => Ok(EvmPrecompileOutput { output: output.bytes }),
            Err(PrecompileHalt::OutOfGas) => Err(InstrStop::PrecompileOOG),
            Err(_) => Err(InstrStop::PrecompileError),
        }
    }
}

impl<const SPEC: u8> PrecompileProvider for Precompiles<SPEC> {
    fn execute(
        &mut self,
        address: Address,
        input: &[u8],
        gas: &mut crate::interpreter::Gas,
    ) -> Option<Result<EvmPrecompileOutput, InstrStop>> {
        let spec = Self::SPEC_ID;
        let address = Self::address_value(address)?;
        let f = match address {
            1 if spec.enables(SpecId::HOMESTEAD) => secp256k1::run,
            2 if spec.enables(SpecId::HOMESTEAD) => hash::run_sha256,
            3 if spec.enables(SpecId::HOMESTEAD) => hash::run_ripemd160,
            4 if spec.enables(SpecId::HOMESTEAD) => identity::run,
            5 if spec.enables(SpecId::BYZANTIUM) && spec.enables(SpecId::OSAKA) => {
                modexp::run_osaka
            }
            5 if spec.enables(SpecId::BYZANTIUM) && spec.enables(SpecId::BERLIN) => {
                modexp::run_berlin
            }
            5 if spec.enables(SpecId::BYZANTIUM) => modexp::run_byzantium,
            6 if spec.enables(SpecId::BYZANTIUM) && spec.enables(SpecId::ISTANBUL) => {
                bn254::add::run_istanbul
            }
            6 if spec.enables(SpecId::BYZANTIUM) => bn254::add::run_byzantium,
            7 if spec.enables(SpecId::BYZANTIUM) && spec.enables(SpecId::ISTANBUL) => {
                bn254::mul::run_istanbul
            }
            7 if spec.enables(SpecId::BYZANTIUM) => bn254::mul::run_byzantium,
            8 if spec.enables(SpecId::BYZANTIUM) && spec.enables(SpecId::ISTANBUL) => {
                bn254::pair::run_istanbul
            }
            8 if spec.enables(SpecId::BYZANTIUM) => bn254::pair::run_byzantium,
            9 if spec.enables(SpecId::ISTANBUL) => blake2::run,
            0x0a if spec.enables(SpecId::CANCUN) => kzg_point_evaluation::run,
            0x0b if spec.enables(SpecId::PRAGUE) => bls12_381::g1_add::run,
            0x0c if spec.enables(SpecId::PRAGUE) => bls12_381::g1_msm::run,
            0x0d if spec.enables(SpecId::PRAGUE) => bls12_381::g2_add::run,
            0x0e if spec.enables(SpecId::PRAGUE) => bls12_381::g2_msm::run,
            0x0f if spec.enables(SpecId::PRAGUE) => bls12_381::pairing::run,
            0x10 if spec.enables(SpecId::PRAGUE) => bls12_381::map_fp_to_g1::run,
            0x11 if spec.enables(SpecId::PRAGUE) => bls12_381::map_fp2_to_g2::run,
            secp256r1::P256VERIFY_ADDRESS if spec.enables(SpecId::OSAKA) => secp256r1::run_osaka,
            _ => return None,
        };
        Some(Self::run(f, input, gas))
    }
}
