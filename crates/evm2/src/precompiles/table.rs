use crate::{
    interpreter::{GasTracker, Message},
    precompiles::{
        PrecompileId, PrecompileResult, blake2, bls12_381, bn254, hash, identity,
        kzg_point_evaluation, modexp, secp256k1, secp256r1,
    },
};
use alloc::vec::Vec;
use alloy_primitives::{Address, map::AddressMap};
use core::fmt::{self, Display};

/// Precompile implementation function.
pub type PrecompileFn = fn(&Message, &mut GasTracker) -> PrecompileResult;

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

    /// Returns this precompile descriptor with a different address.
    #[inline]
    pub fn with_address(self, address: Address) -> Self {
        Self { address, data: self.data }
    }

    /// Returns this precompile descriptor with different data.
    #[inline]
    pub fn with_data(self, data: PrecompileData) -> Self {
        Self { address: self.address, data }
    }

    /// Returns the precompile data.
    #[inline]
    pub const fn data(&self) -> &PrecompileData {
        &self.data
    }

    /// Consumes the precompile descriptor and returns its data.
    #[inline]
    pub fn into_data(self) -> PrecompileData {
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

fn dummy_precompile(_message: &Message, _gas: &mut GasTracker) -> PrecompileResult {
    unreachable!("dummy precompile data must be replaced before use")
}

/// Address-free precompile data.
#[derive(Clone, Debug)]
pub struct PrecompileData {
    /// Precompile implementation function.
    run: PrecompileFn,
    /// Precompile ID.
    id: PrecompileId,
}

impl PrecompileData {
    const DUMMY: Self = Self::new(PrecompileId::custom("__dummy__"), dummy_precompile);

    /// Creates precompile data.
    #[inline]
    pub const fn new(id: PrecompileId, f: PrecompileFn) -> Self {
        Self { id, run: f }
    }

    /// Returns this precompile data with a different ID.
    #[inline]
    pub fn with_id(self, id: PrecompileId) -> Self {
        Self { id, run: self.run }
    }

    /// Returns this precompile data with a different implementation function.
    #[inline]
    pub fn with_run(self, f: PrecompileFn) -> Self {
        Self { id: self.id, run: f }
    }

    /// Returns the precompile ID.
    #[inline]
    pub const fn id(&self) -> &PrecompileId {
        &self.id
    }

    /// Returns the precompile implementation function.
    #[inline]
    pub const fn run(&self) -> PrecompileFn {
        self.run
    }
}

/// Precompile dispatch map.
#[derive(Clone, Debug, Default)]
pub struct PrecompileMap {
    inner: AddressMap<PrecompileData>,
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

    /// Maps the precompile at `address`, if it exists.
    #[inline]
    pub fn map_precompile<F>(&mut self, address: &Address, f: F)
    where
        F: FnOnce(PrecompileData) -> PrecompileData,
    {
        if let Some(data) = self.inner.get_mut(address) {
            *data = f(core::mem::replace(data, PrecompileData::DUMMY));
        }
    }

    /// Maps all precompiles.
    #[inline]
    pub fn map_precompiles<F>(&mut self, mut f: F)
    where
        F: FnMut(&Address, PrecompileData) -> PrecompileData,
    {
        for (address, data) in &mut self.inner {
            *data = f(address, core::mem::replace(data, PrecompileData::DUMMY));
        }
    }

    /// Applies a transformation to the precompile at `address`.
    ///
    /// The closure receives the existing precompile data, if any, and returns the data that should
    /// be installed at `address`.
    #[inline]
    pub fn apply_precompile<F>(&mut self, address: &Address, f: F)
    where
        F: FnOnce(Option<PrecompileData>) -> Option<PrecompileData>,
    {
        if let Some(data) = self.inner.get_mut(address) {
            let current = core::mem::replace(data, PrecompileData::DUMMY);
            if let Some(new_data) = f(Some(current)) {
                *data = new_data;
            } else {
                self.inner.remove(address);
            }
        } else if let Some(data) = f(None) {
            self.inner.insert(*address, data);
        }
    }

    /// Builder-style version of [`Self::map_precompile`].
    #[inline]
    pub fn with_mapped_precompile<F>(mut self, address: &Address, f: F) -> Self
    where
        F: FnOnce(PrecompileData) -> PrecompileData,
    {
        self.map_precompile(address, f);
        self
    }

    /// Builder-style version of [`Self::map_precompiles`].
    #[inline]
    pub fn with_mapped_precompiles<F>(mut self, f: F) -> Self
    where
        F: FnMut(&Address, PrecompileData) -> PrecompileData,
    {
        self.map_precompiles(f);
        self
    }

    /// Builder-style version of [`Self::apply_precompile`].
    #[inline]
    pub fn with_applied_precompile<F>(mut self, address: &Address, f: F) -> Self
    where
        F: FnOnce(Option<PrecompileData>) -> Option<PrecompileData>,
    {
        self.apply_precompile(address, f);
        self
    }

    /// Builder-style version of [`Self::extend`].
    #[inline]
    pub fn with_extended_precompiles(
        mut self,
        precompiles: impl IntoIterator<Item = Precompile>,
    ) -> Self {
        self.extend(precompiles);
        self
    }

    /// Moves precompiles from source addresses to destination addresses.
    ///
    /// All sources are validated before the map is mutated.
    pub fn move_precompiles<I>(&mut self, moves: I) -> Result<(), MovePrecompileError>
    where
        I: IntoIterator<Item = (Address, Address)>,
    {
        let moves = moves.into_iter().filter(|(source, dest)| source != dest).collect::<Vec<_>>();

        for (source, _) in &moves {
            if !self.contains(source) {
                return Err(MovePrecompileError::NotAPrecompile(*source));
            }
        }

        let mut moved = Vec::with_capacity(moves.len());
        for (source, dest) in moves {
            if let Some(precompile) = self.remove(source) {
                moved.push(precompile.with_address(dest));
            }
        }

        self.extend(moved);
        Ok(())
    }

    /// Builder-style version of [`Self::move_precompiles`].
    #[inline]
    pub fn with_moved_precompiles<I>(mut self, moves: I) -> Result<Self, MovePrecompileError>
    where
        I: IntoIterator<Item = (Address, Address)>,
    {
        self.move_precompiles(moves)?;
        Ok(self)
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

/// Error that can occur when moving precompiles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MovePrecompileError {
    /// The source address is not a precompile.
    NotAPrecompile(Address),
}

impl Display for MovePrecompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAPrecompile(address) => {
                write!(f, "source address {address} is not a precompile")
            }
        }
    }
}

impl core::error::Error for MovePrecompileError {}

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
            $vis const $name: $crate::precompiles::Precompile = {
                fn run(
                    message: &$crate::interpreter::Message,
                    gas: &mut $crate::interpreter::GasTracker,
                ) -> $crate::precompiles::PrecompileResult {
                    $f(message.input.as_ref(), gas)
                }

                $crate::precompiles::Precompile::new(
                    $crate::define_precompiles!(@address $address),
                    $id,
                    run,
                )
            };
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evm::precompile::PrecompileOutput;
    use alloy_primitives::{Bytes, address};
    use core::assert_matches;

    fn test_run_a(_message: &Message, _gas: &mut GasTracker) -> PrecompileResult {
        Ok(PrecompileOutput::new(Bytes::from_static(b"a")))
    }

    fn test_run_b(_message: &Message, _gas: &mut GasTracker) -> PrecompileResult {
        Ok(PrecompileOutput::new(Bytes::from_static(b"b")))
    }

    #[test]
    fn map_precompile_updates_data_at_target_address() {
        let address = IDENTITY.address();
        let mut map = PrecompileMap::from_precompiles([IDENTITY]);

        map.map_precompile(&address, |precompile| {
            precompile.with_id(PrecompileId::Sha256).with_run(test_run_a)
        });

        let precompile = map.get(&address).unwrap();
        assert_eq!(precompile.address(), address);
        assert_eq!(precompile.id(), &PrecompileId::Sha256);
    }

    #[test]
    fn apply_precompile_inserts_and_removes_at_target_address() {
        let address = address!("0x0000000000000000000000000000000000000101");
        let mut map = PrecompileMap::new();

        map.apply_precompile(&address, |_| {
            Some(PrecompileData::new(PrecompileId::Identity, test_run_a))
        });

        assert!(map.contains(&address));

        map.apply_precompile(&address, |_| None);

        assert!(!map.contains(&address));
    }

    #[test]
    fn map_precompiles_preserves_existing_addresses() {
        let mut map = PrecompileMap::from_precompiles([IDENTITY, SHA256]);

        map.map_precompiles(|_, precompile| {
            assert_matches!(precompile.id(), PrecompileId::Identity | PrecompileId::Sha256);
            precompile.with_id(PrecompileId::Ripemd160).with_run(test_run_b)
        });

        assert_eq!(map.get(&IDENTITY.address()).unwrap().id(), &PrecompileId::Ripemd160);
        assert_eq!(map.get(&SHA256.address()).unwrap().id(), &PrecompileId::Ripemd160);
    }

    #[test]
    fn move_precompiles_validates_before_mutating() {
        let source = IDENTITY.address();
        let missing = address!("0x0000000000000000000000000000000000000999");
        let dest = address!("0x0000000000000000000000000000000000001000");
        let mut map = PrecompileMap::from_precompiles([IDENTITY]);

        let err = map.move_precompiles([(source, dest), (missing, SHA256.address())]);

        assert_eq!(err, Err(MovePrecompileError::NotAPrecompile(missing)));
        assert!(map.contains(&source));
        assert!(!map.contains(&dest));
    }

    #[test]
    fn move_precompiles_moves_after_validation() {
        let identity = IDENTITY.address();
        let sha256 = SHA256.address();
        let new_identity = address!("0x0000000000000000000000000000000000001001");
        let new_sha256 = address!("0x0000000000000000000000000000000000001002");
        let mut map = PrecompileMap::from_precompiles([IDENTITY, SHA256]);

        map.move_precompiles([(identity, new_identity), (sha256, new_sha256)]).unwrap();

        assert!(!map.contains(&identity));
        assert!(!map.contains(&sha256));
        assert!(map.contains(&new_identity));
        assert!(map.contains(&new_sha256));
    }

    #[test]
    fn move_precompiles_skips_duplicate_sources_after_first_move() {
        let identity = IDENTITY.address();
        let first = address!("0x0000000000000000000000000000000000001001");
        let second = address!("0x0000000000000000000000000000000000001002");
        let mut map = PrecompileMap::from_precompiles([IDENTITY]);

        map.move_precompiles([(identity, first), (identity, second)]).unwrap();

        assert!(map.contains(&first));
        assert!(!map.contains(&second));
    }
}
