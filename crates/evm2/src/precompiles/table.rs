use crate::{
    interpreter::Gas,
    precompiles::{
        PrecompileId, PrecompileResult, blake2, bls12_381, bn254, hash, identity,
        kzg_point_evaluation, modexp, secp256k1, secp256r1,
    },
};
use alloy_primitives::{Address, map::HashMap};

/// Precompile implementation function.
pub type PrecompileFn = fn(&[u8], &mut Gas) -> PrecompileResult;

/// Precompile descriptor.
#[derive(Clone, Debug)]
pub struct Precompile {
    /// Precompile address.
    address: Address,
    /// Precompile data.
    data: PrecompileData,
}

impl Precompile {
    /// Creates a precompile descriptor.
    #[inline]
    pub const fn new(address: Address, id: PrecompileId, f: PrecompileFn) -> Self {
        Self { address, data: PrecompileData::new(id, f) }
    }

    /// Returns the precompile address.
    #[inline]
    pub const fn address(&self) -> Address {
        self.address
    }

    /// Consumes the precompile descriptor and returns its data.
    #[inline]
    pub(super) fn into_data(self) -> PrecompileData {
        self.data
    }

    /// Returns the precompile ID.
    #[inline]
    pub const fn id(&self) -> &PrecompileId {
        self.data.id()
    }

    /// Returns the precompile implementation function.
    #[inline]
    pub const fn run(&self) -> PrecompileFn {
        self.data.run()
    }
}

/// Address-free precompile data.
#[derive(Clone, Debug)]
pub(super) struct PrecompileData {
    /// Precompile implementation function.
    run: PrecompileFn,
    /// Precompile ID.
    id: PrecompileId,
}

impl PrecompileData {
    /// Creates precompile data.
    #[inline]
    pub(super) const fn new(id: PrecompileId, f: PrecompileFn) -> Self {
        Self { id, run: f }
    }

    /// Returns the precompile ID.
    #[inline]
    pub(super) const fn id(&self) -> &PrecompileId {
        &self.id
    }

    /// Returns the precompile implementation function.
    #[inline]
    pub(super) const fn run(&self) -> PrecompileFn {
        self.run
    }
}

/// Precompile dispatch map.
#[derive(Clone, Debug, Default)]
pub struct PrecompileMap {
    inner: HashMap<Address, PrecompileData>,
}

impl PrecompileMap {
    /// Creates an empty precompile map.
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a precompile map from precompile descriptors.
    #[inline]
    pub fn from_precompiles(precompiles: impl IntoIterator<Item = Precompile>) -> Self {
        let mut map = Self::new();
        map.extend(precompiles);
        map
    }

    /// Extends this map with precompile descriptors.
    #[inline]
    pub fn extend(&mut self, precompiles: impl IntoIterator<Item = Precompile>) {
        for precompile in precompiles {
            self.insert(precompile);
        }
    }

    /// Inserts a precompile descriptor, replacing any existing precompile at the same address.
    #[inline]
    pub fn insert(&mut self, precompile: Precompile) -> Option<Precompile> {
        let address = precompile.address();
        self.inner.insert(address, precompile.into_data()).map(|data| Precompile { address, data })
    }

    /// Removes a precompile by address.
    #[inline]
    pub fn remove(&mut self, address: Address) -> Option<Precompile> {
        self.inner.remove(&address).map(|data| Precompile { address, data })
    }

    /// Removes a precompile by descriptor address.
    #[inline]
    pub fn remove_precompile(&mut self, precompile: &Precompile) -> Option<Precompile> {
        self.remove(precompile.address())
    }

    /// Returns the precompile at `address`, if any.
    #[inline]
    pub fn get(&self, address: &Address) -> Option<Precompile> {
        self.inner.get(address).cloned().map(|data| Precompile { address: *address, data })
    }

    /// Returns the precompile data at `address`, if any.
    #[inline]
    pub(super) fn get_data(&self, address: &Address) -> Option<&PrecompileData> {
        self.inner.get(address)
    }

    /// Returns all precompile addresses.
    #[inline]
    pub fn addresses(&self) -> impl Iterator<Item = Address> + '_ {
        self.inner.keys().copied()
    }

    /// Returns `true` if the map contains `address`.
    #[inline]
    pub fn contains(&self, address: &Address) -> bool {
        self.inner.contains_key(address)
    }

    /// Returns the number of precompiles in this map.
    #[inline]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Returns `true` if this map contains no precompiles.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Shrinks the map to fit its current size.
    #[inline]
    pub fn shrink_to_fit(&mut self) {
        self.inner.shrink_to_fit();
    }
}

/// Defines precompile constants.
#[macro_export]
macro_rules! define_precompiles {
    (@address $address:literal) => {
        $crate::precompiles::u64_to_address($address)
    };
    (@address $address:expr) => {
        $address
    };
    ($(
        $(#[$attr:meta])*
        $vis:vis const $name:ident = ($address:expr, $id:expr) => $f:path;
    )*) => {
        $(
            $(#[$attr])*
            $vis const $name: $crate::precompiles::Precompile =
                $crate::precompiles::Precompile::new($crate::define_precompiles!(@address $address), $id, $f);
        )*
    };
}

pub use crate::define_precompiles;

define_precompiles! {
    /// secp256k1 public key recovery precompile.
    pub const SECP256K1_ECRECOVER = (0x01, PrecompileId::EcRec) => secp256k1::run;
    /// SHA-256 precompile.
    pub const SHA256 = (0x02, PrecompileId::Sha256) => hash::run_sha256;
    /// RIPEMD-160 precompile.
    pub const RIPEMD160 = (0x03, PrecompileId::Ripemd160) => hash::run_ripemd160;
    /// Identity precompile.
    pub const IDENTITY = (0x04, PrecompileId::Identity) => identity::run;
    /// Byzantium modexp precompile.
    pub const MODEXP_BYZANTIUM = (0x05, PrecompileId::ModExp) => modexp::run_byzantium;
    /// Berlin modexp precompile.
    pub const MODEXP_BERLIN = (0x05, PrecompileId::ModExp) => modexp::run_berlin;
    /// Osaka modexp precompile.
    pub const MODEXP_OSAKA = (0x05, PrecompileId::ModExp) => modexp::run_osaka;
    /// Byzantium BN254 addition precompile.
    pub const BN254_ADD_BYZANTIUM = (0x06, PrecompileId::Bn254Add) => bn254::add::run_byzantium;
    /// Istanbul BN254 addition precompile.
    pub const BN254_ADD_ISTANBUL = (0x06, PrecompileId::Bn254Add) => bn254::add::run_istanbul;
    /// Byzantium BN254 multiplication precompile.
    pub const BN254_MUL_BYZANTIUM = (0x07, PrecompileId::Bn254Mul) => bn254::mul::run_byzantium;
    /// Istanbul BN254 multiplication precompile.
    pub const BN254_MUL_ISTANBUL = (0x07, PrecompileId::Bn254Mul) => bn254::mul::run_istanbul;
    /// Byzantium BN254 pairing precompile.
    pub const BN254_PAIR_BYZANTIUM = (0x08, PrecompileId::Bn254Pairing) => bn254::pair::run_byzantium;
    /// Istanbul BN254 pairing precompile.
    pub const BN254_PAIR_ISTANBUL = (0x08, PrecompileId::Bn254Pairing) => bn254::pair::run_istanbul;
    /// BLAKE2 compression precompile.
    pub const BLAKE2F = (0x09, PrecompileId::Blake2F) => blake2::run;
    /// KZG point evaluation precompile.
    pub const KZG_POINT_EVALUATION = (0x0a, PrecompileId::KzgPointEvaluation) => kzg_point_evaluation::run;
    /// BLS12-381 G1 addition precompile.
    pub const BLS12_381_G1_ADD = (0x0b, PrecompileId::Bls12G1Add) => bls12_381::g1_add::run;
    /// BLS12-381 G1 MSM precompile.
    pub const BLS12_381_G1_MSM = (0x0c, PrecompileId::Bls12G1Msm) => bls12_381::g1_msm::run;
    /// BLS12-381 G2 addition precompile.
    pub const BLS12_381_G2_ADD = (0x0d, PrecompileId::Bls12G2Add) => bls12_381::g2_add::run;
    /// BLS12-381 G2 MSM precompile.
    pub const BLS12_381_G2_MSM = (0x0e, PrecompileId::Bls12G2Msm) => bls12_381::g2_msm::run;
    /// BLS12-381 pairing precompile.
    pub const BLS12_381_PAIRING = (0x0f, PrecompileId::Bls12Pairing) => bls12_381::pairing::run;
    /// BLS12-381 map FP to G1 precompile.
    pub const BLS12_381_MAP_FP_TO_G1 = (0x10, PrecompileId::Bls12MapFpToGp1) => bls12_381::map_fp_to_g1::run;
    /// BLS12-381 map FP2 to G2 precompile.
    pub const BLS12_381_MAP_FP2_TO_G2 = (0x11, PrecompileId::Bls12MapFp2ToGp2) => bls12_381::map_fp2_to_g2::run;
    /// secp256r1 signature verification precompile.
    pub const P256VERIFY = (0x100, PrecompileId::P256Verify) => secp256r1::run;
    /// secp256r1 signature verification precompile with Osaka gas rules.
    pub const P256VERIFY_OSAKA = (0x100, PrecompileId::P256Verify) => secp256r1::run_osaka;
}
