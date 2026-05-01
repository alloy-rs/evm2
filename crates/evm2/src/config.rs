//! EVM configuration.

use crate::interpreter::{GasParams, GasTable, InstructionImplTable, SpecId};
use core::marker::PhantomData;

/// EVM configuration.
pub trait EvmConfig: Sized + 'static {
    /// Transaction type handled by this EVM.
    type Tx;

    /// Host type used by this EVM.
    type Host: crate::interpreter::Host + ?Sized;

    /// Active hard fork specification.
    const SPEC_ID: SpecId;

    /// Static opcode gas table.
    const GAS_TABLE: GasTable = GasTable::new(Self::SPEC_ID);

    /// Dynamic gas parameter table.
    const GAS_PARAMS: GasParams = GasParams::new_spec(Self::SPEC_ID);

    /// Instruction implementations.
    const INSTRUCTION_IMPLS: InstructionImplTable<Self> = InstructionImplTable::new();
}

/// EVM configuration for a specification ID.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EvmVersion<Tx, const SPEC: u8 = { SpecId::OSAKA as u8 }>(PhantomData<fn() -> Tx>);

impl<Tx: 'static, const SPEC: u8> EvmConfig for EvmVersion<Tx, SPEC> {
    type Tx = Tx;
    type Host = crate::evm::Evm<Self>;

    const SPEC_ID: SpecId = match SpecId::try_from_u8(SPEC) {
        Some(spec_id) => spec_id,
        None => panic!("invalid EVM specification ID"),
    };
}
