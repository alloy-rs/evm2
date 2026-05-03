//! # evm2-precompiles
//!
//! Implementations of EVM precompiled contracts.
#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

#[cfg_attr(
    all(any(target_arch = "x86", target_arch = "x86_64"), target_feature = "avx2"),
    expect(unreachable_code)
)]
pub mod blake2;
pub mod bls12_381;
pub mod bls12_381_const;
pub mod bls12_381_utils;
pub mod bn254;
pub mod hash;
pub mod identity;
pub mod interface;
pub mod kzg_point_evaluation;
pub mod modexp;
pub mod secp256k1;
pub mod secp256r1;
pub mod utils;

mod id;

extern crate self as primitives;

/// EIP-7823 constants.
pub mod eip7823 {
    /// Each of the modexp length inputs must be less than or equal to 1024 bytes.
    pub const INPUT_SIZE_LIMIT: usize = 1024;
}

/// EIP-4844 constants.
pub mod eip4844 {
    pub use crate::kzg_point_evaluation::VERSIONED_HASH_VERSION_KZG;
}

/// Hardfork specification IDs.
pub mod hardfork {
    /// Specification IDs and their activation block.
    #[repr(u8)]
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
    #[allow(non_camel_case_types)]
    pub enum SpecId {
        /// Frontier hard fork.
        FRONTIER = 0,
        /// Frontier Thawing hard fork.
        FRONTIER_THAWING,
        /// Homestead hard fork.
        HOMESTEAD,
        /// DAO Fork hard fork.
        DAO_FORK,
        /// Tangerine Whistle hard fork.
        TANGERINE,
        /// Spurious Dragon hard fork.
        SPURIOUS_DRAGON,
        /// Byzantium hard fork.
        BYZANTIUM,
        /// Constantinople hard fork.
        CONSTANTINOPLE,
        /// Petersburg hard fork.
        PETERSBURG,
        /// Istanbul hard fork.
        ISTANBUL,
        /// Muir Glacier hard fork.
        MUIR_GLACIER,
        /// Berlin hard fork.
        BERLIN,
        /// London hard fork.
        LONDON,
        /// Arrow Glacier hard fork.
        ARROW_GLACIER,
        /// Gray Glacier hard fork.
        GRAY_GLACIER,
        /// Paris/Merge hard fork.
        MERGE,
        /// Shanghai hard fork.
        SHANGHAI,
        /// Cancun hard fork.
        CANCUN,
        /// Prague hard fork.
        PRAGUE,
        /// Osaka hard fork.
        #[default]
        OSAKA,
        /// Amsterdam hard fork.
        AMSTERDAM,
    }

    pub use SpecId::*;
}

pub use alloy_primitives::{self, Address, B256, Bytes, U256, b256, hex, hex_literal, keccak256};

pub use id::PrecompileId;
pub use interface::*;
#[allow(deprecated)]
pub use utils::calc_linear_cost_u32;
pub use utils::{calc_linear_cost, u64_to_address};

#[cfg(not(feature = "std"))]
mod once_lock {
    use alloc::boxed::Box;
    use once_cell::race::OnceBox;

    /// A thread-safe cell which can be written to only once.
    #[derive(Debug)]
    pub struct OnceLock<T> {
        inner: OnceBox<T>,
    }

    impl<T> Default for OnceLock<T> {
        fn default() -> Self {
            Self::new()
        }
    }

    impl<T> OnceLock<T> {
        /// Creates a new empty OnceLock.
        #[inline]
        pub const fn new() -> Self {
            Self { inner: OnceBox::new() }
        }

        /// Gets the contents of the OnceLock, initializing it if necessary.
        #[inline]
        pub fn get_or_init<F>(&self, f: F) -> &T
        where
            F: FnOnce() -> T,
        {
            self.inner.get_or_init(|| Box::new(f()))
        }

        /// Gets the contents of the OnceLock, returning None if it is not initialized.
        #[inline]
        pub fn get(&self) -> Option<&T> {
            self.inner.get()
        }

        /// Sets the contents of the OnceLock.
        #[inline]
        pub fn set(&self, value: T) -> Result<(), T> {
            self.inner.set(Box::new(value)).map_err(|e| *e)
        }
    }
}

#[cfg(feature = "std")]
use once_cell as _;
#[cfg(feature = "std")]
pub use std::sync::OnceLock;

#[cfg(not(feature = "std"))]
pub use once_lock::OnceLock;

use core::fmt::{self, Debug};
use hardfork::SpecId;

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

/// Precompile wrapper for simple eth function that provides complex interface on execution.
#[derive(Clone)]
pub struct Precompile {
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
    pub const fn new(id: PrecompileId, address: Address, fn_: PrecompileFn) -> Self {
        Self { id, address, fn_ }
    }

    /// Returns reference to precompile identifier.
    #[inline]
    pub const fn id(&self) -> &PrecompileId {
        &self.id
    }

    /// Returns reference to address.
    #[inline]
    pub const fn address(&self) -> &Address {
        &self.address
    }

    /// Executes the precompile.
    ///
    /// Returns `Ok(PrecompileOutput)` on success or non-fatal halt,
    /// or `Err(PrecompileError)` for fatal/unrecoverable errors.
    #[inline]
    pub fn execute(&self, input: &[u8], gas: &mut Gas) -> PrecompileResult {
        (self.fn_)(input, gas)
    }
}

/// Ethereum hardfork spec ids. Represents the specs where precompiles had a change.
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub enum PrecompileSpecId {
    /// Frontier spec.
    HOMESTEAD,
    /// Byzantium spec introduced
    /// * [EIP-198](https://eips.ethereum.org/EIPS/eip-198) a EIP-198: Big integer modular
    ///   exponentiation (at 0x05 address).
    /// * [EIP-196](https://eips.ethereum.org/EIPS/eip-196) a bn_add (at 0x06 address) and bn_mul
    ///   (at 0x07 address) precompile
    /// * [EIP-197](https://eips.ethereum.org/EIPS/eip-197) a bn_pair (at 0x08 address) precompile
    BYZANTIUM,
    /// Istanbul spec introduced
    /// * [`EIP-152: Add BLAKE2 compression function`](https://eips.ethereum.org/EIPS/eip-152) `F`
    ///   precompile (at 0x09 address).
    /// * [`EIP-1108: Reduce alt_bn128 precompile gas costs`](https://eips.ethereum.org/EIPS/eip-1108).
    ///   It reduced the gas cost of the bn_add, bn_mul, and bn_pair precompiles.
    ISTANBUL,
    /// Berlin spec made a change to:
    /// * [`EIP-2565: ModExp Gas Cost`](https://eips.ethereum.org/EIPS/eip-2565). It changed the gas
    ///   cost of the modexp precompile.
    BERLIN,
    /// Cancun spec added
    /// * [`EIP-4844: Shard Blob Transactions`](https://eips.ethereum.org/EIPS/eip-4844). It added
    ///   the KZG point evaluation precompile (at 0x0A address).
    CANCUN,
    /// Prague spec added bls precompiles [`EIP-2537: Precompile for BLS12-381 curve operations`](https://eips.ethereum.org/EIPS/eip-2537).
    /// * `BLS12_G1ADD` at address 0x0b
    /// * `BLS12_G1MSM` at address 0x0c
    /// * `BLS12_G2ADD` at address 0x0d
    /// * `BLS12_G2MSM` at address 0x0e
    /// * `BLS12_PAIRING_CHECK` at address 0x0f
    /// * `BLS12_MAP_FP_TO_G1` at address 0x10
    /// * `BLS12_MAP_FP2_TO_G2` at address 0x11
    PRAGUE,
    /// Osaka spec added changes to modexp precompile:
    /// * [`EIP-7823: Set upper bounds for MODEXP`](https://eips.ethereum.org/EIPS/eip-7823).
    /// * [`EIP-7883: ModExp Gas Cost Increase`](https://eips.ethereum.org/EIPS/eip-7883)
    OSAKA,
}

impl From<SpecId> for PrecompileSpecId {
    fn from(spec_id: SpecId) -> Self {
        Self::from_spec_id(spec_id)
    }
}

impl PrecompileSpecId {
    /// The latest known precompile spec. This may refer to a highly experimental hard fork
    /// that is not yet finalized or deployed on any network.
    ///
    /// **Warning**: This value will change between minor versions as new hard forks are added.
    /// Do not rely on it for stable behavior.
    #[doc(alias = "MAX")]
    pub const NEXT: Self = Self::OSAKA;

    /// Returns `true` if the given specification ID is enabled in this spec.
    #[inline]
    pub const fn is_enabled_in(self, other: Self) -> bool {
        self as u8 >= other as u8
    }

    /// Returns the appropriate precompile Spec for the primitive [SpecId].
    pub const fn from_spec_id(spec_id: SpecId) -> Self {
        use SpecId::*;
        match spec_id {
            FRONTIER | FRONTIER_THAWING | HOMESTEAD | DAO_FORK | TANGERINE | SPURIOUS_DRAGON => {
                Self::HOMESTEAD
            }
            BYZANTIUM | CONSTANTINOPLE | PETERSBURG => Self::BYZANTIUM,
            ISTANBUL | MUIR_GLACIER => Self::ISTANBUL,
            BERLIN | LONDON | ARROW_GLACIER | GRAY_GLACIER | MERGE | SHANGHAI => Self::BERLIN,
            CANCUN => Self::CANCUN,
            PRAGUE => Self::PRAGUE,
            OSAKA | AMSTERDAM => Self::OSAKA,
        }
    }
}
