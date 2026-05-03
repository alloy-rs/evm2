//! EVM version data.

mod gas_params;
pub use gas_params::{GasId, GasParamTable, GasParams, num_words};

mod gas_table;
pub use gas_table::GasTable;

mod instruction_impl_table;
pub use instruction_impl_table::InstructionImplTable;

use crate::{EvmConfig, interpreter::SpecId};

/// EVM version data.
#[derive(Debug)]
pub struct EvmVersion<C: EvmConfig = crate::BaseEvmTypes> {
    /// Active hard fork specification.
    pub spec_id: SpecId,
    /// Static opcode gas table.
    pub gas_table: GasTable,
    /// Dynamic gas parameter table.
    pub gas_params: GasParams,
    /// Instruction implementations.
    pub instruction_impls: InstructionImplTable<C>,
}
