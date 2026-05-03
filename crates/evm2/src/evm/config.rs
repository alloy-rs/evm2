//! EVM configuration.

use crate::{
    evm::{InMemoryDB, precompile::PrecompileProvider},
    interpreter::{GasParams, GasTable, InstructionImplTable, SpecId},
};
use core::marker::PhantomData;

/// EVM compile-time type configuration.
pub trait EvmTypes: Sized + 'static {
    /// Transaction type handled by this EVM.
    type Tx;

    /// Host type used by this EVM.
    type Host: crate::interpreter::Host + ?Sized;

    /// Database type used by this EVM.
    type Database: crate::evm::Database;

    /// Precompile provider used by this EVM.
    type Precompiles: PrecompileProvider;
}

/// EVM configuration.
pub trait EvmConfig: EvmTypes {
    /// Active EVM version.
    const VERSION: &'static EvmVersion<Self>;
}

/// EVM version data.
#[derive(Debug)]
pub struct EvmVersion<C: EvmConfig = BaseEvmConfig> {
    /// Active hard fork specification.
    pub spec_id: SpecId,
    /// Static opcode gas table.
    pub gas_table: GasTable,
    /// Dynamic gas parameter table.
    pub gas_params: GasParams,
    /// Instruction implementations.
    pub instruction_impls: InstructionImplTable<C>,
}

/// EVM configuration for a specification ID.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BaseEvmConfig<const SPEC_ID: u8 = { SpecId::OSAKA as u8 }, Tx = ()>(
    PhantomData<fn() -> Tx>,
);

impl<Tx: 'static, const SPEC_ID: u8> EvmTypes for BaseEvmConfig<SPEC_ID, Tx> {
    type Tx = Tx;
    type Host = crate::evm::Evm<Self>;
    type Database = InMemoryDB;
    type Precompiles = crate::evm::precompile::NoPrecompiles;
}

impl<Tx: 'static, const SPEC_ID: u8> EvmConfig for BaseEvmConfig<SPEC_ID, Tx> {
    const VERSION: &'static EvmVersion<Self> =
        &EvmVersion::new_base(match SpecId::try_from_u8(SPEC_ID) {
            Some(spec_id) => spec_id,
            None => panic!("invalid EVM specification ID"),
        });
}
