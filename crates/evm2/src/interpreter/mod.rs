//! EVM interpreter.

mod gas;
pub use gas::{
    Gas, GasId, GasParamTable, GasParams, GasRef, GasTracker, MemoryExtensionResult, MemoryGas,
    num_words,
};

#[macro_use]
mod utils;

mod instructions;
pub use instructions::table::{
    DEFAULT_GAS_TABLE, DEFAULT_TABLE, DEFAULT_TAIL_TABLE, GasTable, InstrFn, InstrFnRet,
    InstrTable, InstructionCx, TailInstrFn, TailInstrFnRet, TailInstrTable, make_table,
    make_tail_table, new_gas_table,
};

mod opcode;
pub use opcode::op;

mod ctrl;
pub use ctrl::{Ctrl, CtrlRef};

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
#[repr(u8)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
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
