//! EVM configuration.

use crate::interpreter::{
    GasParamTable, GasParams, GasTable, InstructionImplTable, InstructionTable, SpecId,
    TailInstructionTable,
    table::{
        make_instruction_table, make_normal_instruction_table, make_tail_instruction_table,
        new_gas_table,
    },
};
use core::marker::PhantomData;

/// EVM configuration.
pub trait EvmConfig: Sized + 'static {
    /// Transaction type handled by this EVM.
    type Tx;

    /// Active hard fork specification.
    const SPEC_ID: SpecId;

    /// Static opcode gas table.
    const GAS_TABLE: GasTable = new_gas_table(Self::SPEC_ID);

    /// Dynamic gas parameter table.
    const GAS_PARAMS: GasParamTable = GasParams::new_spec(Self::SPEC_ID).into_table();

    /// Instruction implementations.
    const INSTRUCTION_IMPLS: InstructionImplTable<Self> = make_instruction_table::<Self>();

    /// Normal instruction dispatch table.
    const INSTRUCTIONS: InstructionTable<Self> =
        make_normal_instruction_table(Self::INSTRUCTION_IMPLS);

    /// Tail-call instruction dispatch table.
    const TAIL_INSTRUCTIONS: TailInstructionTable<Self> =
        make_tail_instruction_table(Self::INSTRUCTION_IMPLS);
}

/// EVM configuration for a specification ID.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EvmVersion<Tx, const SPEC: u8 = { SpecId::OSAKA as u8 }>(PhantomData<fn() -> Tx>);

impl<Tx: 'static, const SPEC: u8> EvmConfig for EvmVersion<Tx, SPEC> {
    type Tx = Tx;

    const SPEC_ID: SpecId = match SpecId::try_from_u8(SPEC) {
        Some(spec_id) => spec_id,
        None => panic!("invalid EVM specification ID"),
    };
}
