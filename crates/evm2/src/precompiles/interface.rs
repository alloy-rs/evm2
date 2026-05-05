//! Interface for the precompiles. It contains the precompile result type,
//! the precompile output type, and the precompile error type.

#![allow(dead_code)]

use alloc::{borrow::Cow, string::String, sync::Arc, vec::Vec};
use core::fmt::{self, Debug};

use crate::{
    evm::precompile::PrecompileOutput,
    precompiles::bls12_381::{G1Point, G1PointScalar, G2Point, G2PointScalar},
};
use thiserror::Error;

/// Type-erased error type.
#[derive(Clone, Debug)]
pub(crate) struct AnyError(Arc<dyn core::error::Error + Send + Sync>);

impl AnyError {
    /// Creates a new [`AnyError`] from any error type.
    pub(crate) fn new(err: impl core::error::Error + Send + Sync + 'static) -> Self {
        Self(Arc::new(err))
    }
}

impl PartialEq for AnyError {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for AnyError {}

impl core::hash::Hash for AnyError {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        (Arc::as_ptr(&self.0) as *const ()).hash(state);
    }
}

impl fmt::Display for AnyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl core::error::Error for AnyError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        self.0.source()
    }
}

#[derive(Debug)]
struct StringError(String);

impl fmt::Display for StringError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl core::error::Error for StringError {}

impl From<String> for AnyError {
    fn from(value: String) -> Self {
        Self::new(StringError(value))
    }
}

impl From<&'static str> for AnyError {
    fn from(value: &'static str) -> Self {
        Self::new(StringError(value.into()))
    }
}

/// A precompile operation result type for individual Ethereum precompile functions.
pub type EthPrecompileResult = Result<PrecompileOutput, PrecompileHalt>;

/// Crypto operations trait for precompiles.
pub trait Crypto: Send + Sync + Debug {
    /// Compute SHA-256 hash
    #[inline]
    fn sha256(&self, input: &[u8]) -> [u8; 32] {
        use sha2::Digest;
        let output = sha2::Sha256::digest(input);
        output.into()
    }

    /// Compute RIPEMD-160 hash
    #[inline]
    fn ripemd160(&self, input: &[u8]) -> [u8; 32] {
        use ripemd::Digest;
        let mut hasher = ripemd::Ripemd160::new();
        hasher.update(input);

        let mut output = [0u8; 32];
        hasher.finalize_into((&mut output[12..]).into());
        output
    }

    /// BN254 elliptic curve addition.
    #[inline]
    fn bn254_g1_add(&self, p1: &[u8], p2: &[u8]) -> Result<[u8; 64], PrecompileHalt> {
        crate::precompiles::bn254::crypto_backend::g1_point_add(p1, p2)
    }

    /// BN254 elliptic curve scalar multiplication.
    #[inline]
    fn bn254_g1_mul(&self, point: &[u8], scalar: &[u8]) -> Result<[u8; 64], PrecompileHalt> {
        crate::precompiles::bn254::crypto_backend::g1_point_mul(point, scalar)
    }

    /// BN254 pairing check.
    #[inline]
    fn bn254_pairing_check(&self, pairs: &[(&[u8], &[u8])]) -> Result<bool, PrecompileHalt> {
        crate::precompiles::bn254::crypto_backend::pairing_check(pairs)
    }

    /// secp256k1 ECDSA signature recovery.
    #[inline]
    fn secp256k1_ecrecover(
        &self,
        sig: &[u8; 64],
        recid: u8,
        msg: &[u8; 32],
    ) -> Result<[u8; 32], PrecompileHalt> {
        crate::precompiles::secp256k1::ecrecover_bytes(sig, recid, msg)
            .ok_or(PrecompileHalt::Secp256k1RecoverFailed)
    }

    /// Modular exponentiation.
    #[inline]
    fn modexp(&self, base: &[u8], exp: &[u8], modulus: &[u8]) -> Result<Vec<u8>, PrecompileHalt> {
        Ok(crate::precompiles::modexp::modexp(base, exp, modulus))
    }

    /// Blake2 compression function.
    #[inline]
    fn blake2_compress(&self, rounds: u32, h: &mut [u64; 8], m: &[u64; 16], t: &[u64; 2], f: bool) {
        crate::precompiles::blake2::compress(rounds, h, m, t, f);
    }

    /// secp256r1 (P-256) signature verification.
    #[inline]
    fn secp256r1_verify_signature(&self, msg: &[u8; 32], sig: &[u8; 64], pk: &[u8; 64]) -> bool {
        crate::precompiles::secp256r1::verify_signature(msg, sig, pk).is_some()
    }

    /// KZG point evaluation.
    #[inline]
    fn verify_kzg_proof(
        &self,
        z: &[u8; 32],
        y: &[u8; 32],
        commitment: &[u8; 48],
        proof: &[u8; 48],
    ) -> Result<(), PrecompileHalt> {
        if !crate::precompiles::kzg_point_evaluation::verify_kzg_proof(commitment, z, y, proof) {
            return Err(PrecompileHalt::BlobVerifyKzgProofFailed);
        }

        Ok(())
    }

    /// BLS12-381 G1 addition (returns 96-byte unpadded G1 point)
    fn bls12_381_g1_add(&self, a: G1Point, b: G1Point) -> Result<[u8; 96], PrecompileHalt> {
        crate::precompiles::bls12_381::crypto_backend::p1_add_affine_bytes(a, b)
    }

    /// BLS12-381 G1 multi-scalar multiplication (returns 96-byte unpadded G1 point)
    fn bls12_381_g1_msm(
        &self,
        pairs: &mut dyn Iterator<Item = Result<G1PointScalar, PrecompileHalt>>,
    ) -> Result<[u8; 96], PrecompileHalt> {
        crate::precompiles::bls12_381::crypto_backend::p1_msm_bytes(pairs)
    }

    /// BLS12-381 G2 addition (returns 192-byte unpadded G2 point)
    fn bls12_381_g2_add(&self, a: G2Point, b: G2Point) -> Result<[u8; 192], PrecompileHalt> {
        crate::precompiles::bls12_381::crypto_backend::p2_add_affine_bytes(a, b)
    }

    /// BLS12-381 G2 multi-scalar multiplication (returns 192-byte unpadded G2 point)
    fn bls12_381_g2_msm(
        &self,
        pairs: &mut dyn Iterator<Item = Result<G2PointScalar, PrecompileHalt>>,
    ) -> Result<[u8; 192], PrecompileHalt> {
        crate::precompiles::bls12_381::crypto_backend::p2_msm_bytes(pairs)
    }

    /// BLS12-381 pairing check.
    fn bls12_381_pairing_check(
        &self,
        pairs: &[(G1Point, G2Point)],
    ) -> Result<bool, PrecompileHalt> {
        crate::precompiles::bls12_381::crypto_backend::pairing_check_bytes(pairs)
    }

    /// BLS12-381 map field element to G1.
    fn bls12_381_fp_to_g1(&self, fp: &[u8; 48]) -> Result<[u8; 96], PrecompileHalt> {
        crate::precompiles::bls12_381::crypto_backend::map_fp_to_g1_bytes(fp)
    }

    /// BLS12-381 map field element to G2.
    fn bls12_381_fp2_to_g2(&self, fp2: ([u8; 48], [u8; 48])) -> Result<[u8; 192], PrecompileHalt> {
        crate::precompiles::bls12_381::crypto_backend::map_fp2_to_g2_bytes(&fp2.0, &fp2.1)
    }
}

/// Non-fatal halt reasons for precompiles.
///
/// These represent conditions that halt precompile execution but do not abort
/// the entire EVM transaction.
#[derive(Clone, Debug, Error, PartialEq, Eq, Hash)]
pub enum PrecompileHalt {
    /// out of gas is the main error. Others are here just for completeness
    #[error("out of gas")]
    OutOfGas,
    /// Blake2 errors
    #[error("wrong input length for blake2")]
    Blake2WrongLength,
    /// Blake2 wrong final indicator flag
    #[error("wrong final indicator flag for blake2")]
    Blake2WrongFinalIndicatorFlag,
    /// Modexp errors
    #[error("modexp exp overflow")]
    ModexpExpOverflow,
    /// Modexp base overflow
    #[error("modexp base overflow")]
    ModexpBaseOverflow,
    /// Modexp mod overflow
    #[error("modexp mod overflow")]
    ModexpModOverflow,
    /// Modexp limit all input sizes.
    #[error("Modexp limit all input sizes.")]
    ModexpEip7823LimitSize,
    /// Bn254 errors
    #[error("field point not a member of bn254 curve")]
    Bn254FieldPointNotAMember,
    /// Bn254 affine g failed to create
    #[error("failed to create affine g point for bn254 curve")]
    Bn254AffineGFailedToCreate,
    /// Bn254 pair length
    #[error("bn254 invalid pair length")]
    Bn254PairLength,
    // Blob errors
    /// The input length is not exactly 192 bytes
    #[error("invalid blob input length")]
    BlobInvalidInputLength,
    /// The commitment does not match the versioned hash
    #[error("mismatched blob version")]
    BlobMismatchedVersion,
    /// The proof verification failed
    #[error("verifying blob kzg proof failed")]
    BlobVerifyKzgProofFailed,
    /// Non-canonical field element
    #[error("non-canonical field element")]
    NonCanonicalFp,
    /// BLS12-381 G1 point not on curve
    #[error("bls12-381 g1 point not on curve")]
    Bls12381G1NotOnCurve,
    /// BLS12-381 G1 point not in correct subgroup
    #[error("bls12-381 g1 point not in correct subgroup")]
    Bls12381G1NotInSubgroup,
    /// BLS12-381 G2 point not on curve
    #[error("bls12-381 g2 point not on curve")]
    Bls12381G2NotOnCurve,
    /// BLS12-381 G2 point not in correct subgroup
    #[error("bls12-381 g2 point not in correct subgroup")]
    Bls12381G2NotInSubgroup,
    /// BLS12-381 scalar input length error
    #[error("bls12-381 scalar input length error")]
    Bls12381ScalarInputLength,
    /// BLS12-381 G1 add input length error
    #[error("bls12-381 g1 add input length error")]
    Bls12381G1AddInputLength,
    /// BLS12-381 G1 msm input length error
    #[error("bls12-381 g1 msm input length error")]
    Bls12381G1MsmInputLength,
    /// BLS12-381 G2 add input length error
    #[error("bls12-381 g2 add input length error")]
    Bls12381G2AddInputLength,
    /// BLS12-381 G2 msm input length error
    #[error("bls12-381 g2 msm input length error")]
    Bls12381G2MsmInputLength,
    /// BLS12-381 pairing input length error
    #[error("bls12-381 pairing input length error")]
    Bls12381PairingInputLength,
    /// BLS12-381 map fp to g1 input length error
    #[error("bls12-381 map fp to g1 input length error")]
    Bls12381MapFpToG1InputLength,
    /// BLS12-381 map fp2 to g2 input length error
    #[error("bls12-381 map fp2 to g2 input length error")]
    Bls12381MapFp2ToG2InputLength,
    /// BLS12-381 padding error
    #[error("bls12-381 fp 64 top bytes of input are not zero")]
    Bls12381FpPaddingInvalid,
    /// BLS12-381 fp padding length error
    #[error("bls12-381 fp padding length error")]
    Bls12381FpPaddingLength,
    /// BLS12-381 g1 padding length error
    #[error("bls12-381 g1 padding length error")]
    Bls12381G1PaddingLength,
    /// BLS12-381 g2 padding length error
    #[error("bls12-381 g2 padding length error")]
    Bls12381G2PaddingLength,
    /// KZG invalid G1 point
    #[error("kzg invalid g1 point")]
    KzgInvalidG1Point,
    /// KZG G1 point not on curve
    #[error("kzg g1 point not on curve")]
    KzgG1PointNotOnCurve,
    /// KZG G1 point not in correct subgroup
    #[error("kzg g1 point not in correct subgroup")]
    KzgG1PointNotInSubgroup,
    /// KZG input length error
    #[error("kzg invalid input length")]
    KzgInvalidInputLength,
    /// secp256k1 ecrecover failed
    #[error("secp256k1 signature recovery failed")]
    Secp256k1RecoverFailed,
    /// Catch-all variant for precompile halt reasons without a dedicated variant.
    #[error("{0}")]
    Other(Cow<'static, str>),
}

impl PrecompileHalt {
    /// Returns another halt reason with the given message.
    pub(crate) fn other(err: impl Into<String>) -> Self {
        Self::Other(Cow::Owned(err.into()))
    }

    /// Returns another halt reason with the given static string.
    pub(crate) const fn other_static(err: &'static str) -> Self {
        Self::Other(Cow::Borrowed(err))
    }

    /// Returns `true` if the halt reason is out of gas.
    pub(crate) const fn is_oog(&self) -> bool {
        matches!(self, Self::OutOfGas)
    }
}

impl From<crate::interpreter::InstrStop> for PrecompileHalt {
    #[inline]
    fn from(_: crate::interpreter::InstrStop) -> Self {
        Self::OutOfGas
    }
}

/// Fatal precompile error type.
///
/// These errors represent unrecoverable conditions that abort the entire EVM
/// transaction. They propagate as `EVMError::Custom`.
///
/// For non-fatal halt reasons (like out-of-gas or invalid input), see
/// [`PrecompileHalt`] which is expressed through precompile output status.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum PrecompileError {
    /// Unrecoverable error that halts EVM execution.
    Fatal(String),
    /// Unrecoverable error that halts EVM execution.
    FatalAny(AnyError),
}

impl PrecompileError {
    /// Returns `true` if the error is `Fatal` or `FatalAny`.
    pub(crate) const fn is_fatal(&self) -> bool {
        true
    }
}

impl core::error::Error for PrecompileError {}

impl fmt::Display for PrecompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fatal(s) => write!(f, "fatal: {s}"),
            Self::FatalAny(s) => write!(f, "fatal: {s}"),
        }
    }
}

/// Default implementation of the Crypto trait using the existing crypto libraries.
#[derive(Clone, Copy, Debug)]
pub(crate) struct DefaultCrypto;

impl Crypto for DefaultCrypto {}
