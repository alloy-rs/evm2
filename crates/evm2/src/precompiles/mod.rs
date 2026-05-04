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

mod id;

/// EIP-7823 constants.
pub(crate) mod eip7823 {
    /// Each of the modexp length inputs must be less than or equal to 1024 bytes.
    pub(crate) const INPUT_SIZE_LIMIT: usize = 1024;
}

/// EIP-4844 constants.
pub(crate) mod eip4844 {
    pub(crate) use crate::precompiles::kzg_point_evaluation::VERSIONED_HASH_VERSION_KZG;
}

pub(crate) use alloy_primitives::{
    self, Address, B256, Bytes, U256, b256, hex, hex_literal, keccak256,
};

pub(crate) use id::PrecompileId;
pub(crate) use interface::{eth_precompile_fn, *};
#[allow(deprecated)]
pub(crate) use utils::calc_linear_cost_u32;
pub(crate) use utils::{calc_linear_cost, u64_to_address};

use core::fmt::{self, Debug};

use crate::once_lock::OnceLock;

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
pub(crate) fn install_crypto<C: Crypto + 'static>(crypto: C) -> bool {
    CRYPTO.set(Box::new(crypto)).is_ok()
}

/// Get the installed crypto provider, or the default if none is installed.
pub(crate) fn crypto() -> &'static dyn Crypto {
    CRYPTO.get_or_init(|| Box::new(DefaultCrypto)).as_ref()
}

/// Precompile wrapper for simple eth function that provides complex interface on execution.
#[derive(Clone)]
pub(crate) struct Precompile {
    /// Unique identifier.
    id: PrecompileId,
    /// Precompile address.
    address: Address,
    /// Precompile function.
    fn_: PrecompileFn,
}

impl Debug for Precompile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Precompile {{ id: {:?}, address: {:?} }}", self.id, self.address)
    }
}

impl From<(PrecompileId, Address, PrecompileFn)> for Precompile {
    fn from((id, address, fn_): (PrecompileId, Address, PrecompileFn)) -> Self {
        Self { id, address, fn_ }
    }
}

impl From<Precompile> for (PrecompileId, Address) {
    fn from(value: Precompile) -> Self {
        (value.id, value.address)
    }
}

impl Precompile {
    /// Create new precompile.
    pub(crate) const fn new(id: PrecompileId, address: Address, fn_: PrecompileFn) -> Self {
        Self { id, address, fn_ }
    }

    /// Returns reference to precompile identifier.
    #[inline]
    pub(crate) const fn id(&self) -> &PrecompileId {
        &self.id
    }

    /// Returns reference to address.
    #[inline]
    pub(crate) const fn address(&self) -> &Address {
        &self.address
    }

    /// Executes the precompile.
    ///
    /// Returns `Ok(PrecompileOutput)` on success or non-fatal halt,
    /// or `Err(PrecompileError)` for fatal/unrecoverable errors.
    #[inline]
    pub(crate) fn execute(&self, input: &[u8], gas: &mut Gas) -> PrecompileResult {
        (self.fn_)(input, gas)
    }
}
