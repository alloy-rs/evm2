//! EVM interpreter.

mod gas;
pub use gas::{
    Gas, GasId, GasParamTable, GasParams, GasTracker, MemoryExtensionResult, MemoryGas, num_words,
};

#[macro_use]
mod utils;

mod instructions;
pub use instructions::table;

mod opcode;
pub use opcode::op;

mod ctrl;
pub use ctrl::{BytecodeRef, Pc, PcMut};

mod stack;
pub use stack::{Stack, Word};

mod memory;
pub use memory::Memory;

mod state;
pub use state::{Host, State};

mod runtime;
pub use runtime::{Interpreter, Table};

pub(crate) type Result<T = (), E = InstrErr> = core::result::Result<T, E>;

/// Specification IDs and their activation block.
///
/// Information was obtained from the [Ethereum Execution Specifications](https://github.com/ethereum/execution-specs).
#[repr(u8)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
#[allow(non_camel_case_types)]
pub enum SpecId {
    /// Frontier hard fork
    /// Activated at block 0
    FRONTIER = 0,
    /// Frontier Thawing hard fork
    /// Activated at block 200000
    FRONTIER_THAWING,
    /// Homestead hard fork
    /// Activated at block 1150000
    HOMESTEAD,
    /// DAO Fork hard fork
    /// Activated at block 1920000
    DAO_FORK,
    /// Tangerine Whistle hard fork
    /// Activated at block 2463000
    TANGERINE,
    /// Spurious Dragon hard fork
    /// Activated at block 2675000
    SPURIOUS_DRAGON,
    /// Byzantium hard fork
    /// Activated at block 4370000
    BYZANTIUM,
    /// Constantinople hard fork
    /// Activated at block 7280000 is overwritten with PETERSBURG
    CONSTANTINOPLE,
    /// Petersburg hard fork
    /// Activated at block 7280000
    PETERSBURG,
    /// Istanbul hard fork
    /// Activated at block 9069000
    ISTANBUL,
    /// Muir Glacier hard fork
    /// Activated at block 9200000
    MUIR_GLACIER,
    /// Berlin hard fork
    /// Activated at block 12244000
    BERLIN,
    /// London hard fork
    /// Activated at block 12965000
    LONDON,
    /// Arrow Glacier hard fork
    /// Activated at block 13773000
    ARROW_GLACIER,
    /// Gray Glacier hard fork
    /// Activated at block 15050000
    GRAY_GLACIER,
    /// Paris/Merge hard fork
    /// Activated at block 15537394 (TTD: 58750000000000000000000)
    MERGE,
    /// Shanghai hard fork
    /// Activated at block 17034870 (Timestamp: 1681338455)
    SHANGHAI,
    /// Cancun hard fork
    /// Activated at block 19426587 (Timestamp: 1710338135)
    CANCUN,
    /// Prague hard fork
    /// Activated at block 22431084 (Timestamp: 1746612311)
    PRAGUE,
    /// Osaka hard fork
    /// Activated at slot 13164544 (Timestamp: 1764798551)
    #[default]
    OSAKA,
    /// Amsterdam hard fork
    /// Activated at block TBD
    AMSTERDAM,
}

impl SpecId {
    /// Latest known specification ID.
    #[doc(alias = "MAX")]
    pub const NEXT: Self = Self::AMSTERDAM;

    /// Returns the specification ID for a raw byte.
    #[inline]
    pub const fn try_from_u8(spec_id: u8) -> Option<Self> {
        if spec_id <= Self::NEXT as u8 {
            // SAFETY: `spec_id` is within the valid variant range.
            return Some(unsafe { core::mem::transmute::<u8, Self>(spec_id) });
        }
        None
    }

    /// Returns `true` if this specification enables `other`.
    #[inline]
    pub const fn enables(self, other: Self) -> bool {
        self as u8 >= other as u8
    }

    /// Returns `true` if `other` is enabled in this specification.
    #[deprecated(note = "use SpecId::enables instead")]
    #[inline]
    pub const fn is_enabled_in(self, other: Self) -> bool {
        self.enables(other)
    }
}

impl From<SpecId> for u8 {
    #[inline]
    fn from(spec_id: SpecId) -> Self {
        spec_id as Self
    }
}

impl TryFrom<u8> for SpecId {
    type Error = u8;

    #[inline]
    fn try_from(value: u8) -> core::result::Result<Self, Self::Error> {
        Self::try_from_u8(value).ok_or(value)
    }
}

/// Instruction execution error.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub enum InstrErr {
    /// Execution stopped.
    Stop = 1,
    /// Gas was exhausted.
    OutOfGas,
    /// Stack exceeded the maximum depth.
    StackOverflow,
    /// Stack did not contain enough values.
    StackUnderflow,
    /// Invalid instruction or state.
    Invalid,
    /// Return from execution.
    Return,
    /// Revert execution.
    Revert,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interpreter::table::{DEFAULT_TABLE, DEFAULT_TAIL_TABLE, new_gas_table};
    use alloy_primitives::U256;

    struct DummyHost;

    impl Host for DummyHost {
        fn balance(&self, address: Word) -> Word {
            address
        }
    }

    #[test]
    fn main_smoke() {
        #[rustfmt::skip]
        let bytecode = core::hint::black_box(&[
            op::PUSH1, 0x01,
            op::PUSH1, 0x02,
            op::ADD,
            op::STOP,
        ][..]);
        let spec_id = core::hint::black_box(SpecId::HOMESTEAD);
        let instruction_table = core::hint::black_box(Table::Tail(&DEFAULT_TAIL_TABLE));

        let gas_table = new_gas_table(spec_id);
        let mut interpreter = Interpreter::new(bytecode.into(), spec_id);
        interpreter.run(instruction_table, &gas_table, &mut DummyHost);
    }

    #[test]
    fn basic() {
        const BASIC: &[u8] = &[op::PUSH1, 0x01, op::PUSH1, 0x02, op::ADD, op::STOP];

        for spec in [SpecId::FRONTIER, SpecId::HOMESTEAD] {
            let gas_table = new_gas_table(spec);
            for (_name, table) in [
                ("normal", Table::Normal(&DEFAULT_TABLE)),
                ("tail", Table::Tail(&DEFAULT_TAIL_TABLE)),
            ] {
                let mut interpreter = Interpreter::new(BASIC.into(), spec);
                interpreter.run(table, &gas_table, &mut DummyHost);
                assert!(interpreter.gas.remaining() > 0);
                assert_eq!(interpreter.pc, 6);
                assert_eq!(interpreter.stack_len, 1);
                assert_eq!(interpreter.stack[0], U256::from(3));
            }
        }
    }
}
