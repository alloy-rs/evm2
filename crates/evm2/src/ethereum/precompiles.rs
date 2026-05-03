//! Ethereum precompile provider and fork selection.

use crate::{
    Evm, EvmConfig,
    evm::precompile::{PrecompileOutput, PrecompileProvider},
    interpreter::{InstrStop, SpecId},
};
use alloc::vec::Vec;
use alloy_primitives::{Address, map::HashMap};
use core::marker::PhantomData;

/// EVM configuration for Ethereum execution with real Ethereum precompiles.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EthereumEvmVersion<Tx, const SPEC: u8 = { SpecId::OSAKA as u8 }>(
    PhantomData<fn() -> Tx>,
);

impl<Tx: 'static, const SPEC: u8> EvmConfig for EthereumEvmVersion<Tx, SPEC> {
    type Tx = Tx;
    type Host = Evm<Self>;
    type Database = crate::evm::InMemoryDB;
    type Precompiles = EthereumPrecompiles;

    const SPEC_ID: SpecId = match SpecId::try_from_u8(SPEC) {
        Some(spec_id) => spec_id,
        None => panic!("invalid EVM specification ID"),
    };
}

/// Ethereum precompile provider.
#[derive(Clone, Debug, Default)]
pub struct EthereumPrecompiles {
    fun: HashMap<evm2_precompiles::B160, evm2_precompiles::Precompile>,
    addresses: Vec<Address>,
}

impl EthereumPrecompiles {
    /// Creates a precompile provider from precompile address/function pairs.
    pub fn new(precompiles: impl IntoIterator<Item = evm2_precompiles::PrecompileAddress>) -> Self {
        let fun = precompiles.into_iter().map(From::from).collect();
        Self::from_fun(fun)
    }

    /// Adds or replaces precompile address/function pairs.
    pub fn extend(
        &mut self,
        precompiles: impl IntoIterator<Item = evm2_precompiles::PrecompileAddress>,
    ) {
        self.fun.extend(precompiles.into_iter().map(
            |precompile| -> (evm2_precompiles::B160, evm2_precompiles::Precompile) {
                precompile.into()
            },
        ));
        self.refresh_addresses();
    }

    fn from_fun(fun: HashMap<evm2_precompiles::B160, evm2_precompiles::Precompile>) -> Self {
        let mut this = Self { fun, addresses: Vec::new() };
        this.refresh_addresses();
        this
    }

    fn refresh_addresses(&mut self) {
        self.addresses = self.fun.keys().map(|address| Address::from(*address)).collect();
        self.addresses.sort_unstable();
    }

    /// Returns whether `address` is an active precompile.
    pub fn contains(&self, address: &Address) -> bool {
        self.fun.contains_key(&address_key(address))
    }

    /// Returns the precompile function at `address`.
    pub fn get(&self, address: &Address) -> Option<evm2_precompiles::Precompile> {
        self.fun.get(&address_key(address)).copied()
    }

    /// Returns whether no precompiles are active.
    pub fn is_empty(&self) -> bool {
        self.fun.is_empty()
    }

    /// Returns the number of active precompiles.
    pub fn len(&self) -> usize {
        self.fun.len()
    }

    /// Returns active precompile addresses.
    pub fn addresses(&self) -> &[Address] {
        &self.addresses
    }
}

impl PrecompileProvider for EthereumPrecompiles {
    #[inline]
    fn execute(
        &mut self,
        address: Address,
        input: &[u8],
        gas_limit: u64,
    ) -> Option<Result<PrecompileOutput, InstrStop>> {
        let precompile = self.get(&address)?;
        Some(match evm2_precompiles::execute(precompile, input, gas_limit) {
            Ok((gas_used, output)) => Ok(PrecompileOutput { gas_used, output: output.into() }),
            Err(evm2_precompiles::PrecompileError::OutOfGas) => Err(InstrStop::PrecompileOOG),
            Err(_) => Err(InstrStop::PrecompileError),
        })
    }

    #[inline]
    fn warm_addresses(&self) -> &[Address] {
        self.addresses()
    }
}

/// Returns Ethereum precompiles for an EVM spec.
pub fn precompiles_for_spec(spec: SpecId) -> EthereumPrecompiles {
    match spec {
        SpecId::FRONTIER
        | SpecId::FRONTIER_THAWING
        | SpecId::HOMESTEAD
        | SpecId::DAO_FORK
        | SpecId::TANGERINE
        | SpecId::SPURIOUS_DRAGON => homestead_precompiles(),
        SpecId::BYZANTIUM | SpecId::CONSTANTINOPLE | SpecId::PETERSBURG => byzantium_precompiles(),
        SpecId::ISTANBUL | SpecId::MUIR_GLACIER => istanbul_precompiles(),
        SpecId::BERLIN
        | SpecId::LONDON
        | SpecId::ARROW_GLACIER
        | SpecId::GRAY_GLACIER
        | SpecId::MERGE
        | SpecId::SHANGHAI
        | SpecId::CANCUN
        | SpecId::PRAGUE
        | SpecId::OSAKA
        | SpecId::AMSTERDAM => berlin_precompiles(),
    }
}

fn homestead_precompiles() -> EthereumPrecompiles {
    EthereumPrecompiles::new([
        evm2_precompiles::secp256k1::ECRECOVER,
        evm2_precompiles::hash::SHA256,
        evm2_precompiles::hash::RIPEMD160,
        evm2_precompiles::identity::FUN,
    ])
}

fn byzantium_precompiles() -> EthereumPrecompiles {
    let mut precompiles = homestead_precompiles();
    precompiles.extend([
        evm2_precompiles::bn128::add::BYZANTIUM,
        evm2_precompiles::bn128::mul::BYZANTIUM,
        evm2_precompiles::bn128::pair::BYZANTIUM,
        evm2_precompiles::modexp::BYZANTIUM,
    ]);
    precompiles
}

fn istanbul_precompiles() -> EthereumPrecompiles {
    let mut precompiles = byzantium_precompiles();
    precompiles.extend([
        evm2_precompiles::blake2::FUN,
        evm2_precompiles::bn128::add::ISTANBUL,
        evm2_precompiles::bn128::mul::ISTANBUL,
        evm2_precompiles::bn128::pair::ISTANBUL,
    ]);
    precompiles
}

fn berlin_precompiles() -> EthereumPrecompiles {
    let mut precompiles = istanbul_precompiles();
    precompiles.extend([evm2_precompiles::modexp::BERLIN]);
    precompiles
}

fn address_key(address: &Address) -> evm2_precompiles::B160 {
    let mut key = [0; 20];
    key.copy_from_slice(address.as_slice());
    key
}
