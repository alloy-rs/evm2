//! EVM interpreter.

mod gas;
pub use gas::{
    Gas, GasId, GasParamTable, GasParams, GasTracker, MemoryExtensionResult, MemoryGas, num_words,
};

#[macro_use]
mod utils;

mod instructions;
pub(crate) use instructions::table;
#[doc(hidden)]
pub use instructions::table::{GasTable, Instruction, InstructionImplTable};

mod opcode;
pub use opcode::op;

mod ctrl;
pub use ctrl::{BytecodeRef, Pc, PcMut};

mod stack;
pub use stack::{Stack, Word};

mod memory;
pub use memory::Memory;

mod message;
pub use message::{Message, MessageKind};

mod state;
pub use state::{Host, State};

mod runtime;
pub use runtime::Interpreter;

pub(crate) type Result<T = (), E = InstrStop> = core::result::Result<T, E>;

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

/// Result of executing an EVM instruction.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum InstrStop {
    /// Encountered a `STOP` opcode
    #[default]
    Stop = 1, // Start at 1 so that `Result<(), _>::Ok(())` is 0.
    /// Return from the current call.
    Return,
    /// Self-destruct the current contract.
    SelfDestruct,
    /// Temporarily suspended, for CALL/CREATE.
    Suspend,

    // Revert Codes
    /// Revert the transaction.
    Revert = 0x10,
    /// Exceeded maximum call depth.
    CallTooDeep,
    /// Insufficient funds for transfer.
    OutOfFunds,
    /// Revert if `CREATE`/`CREATE2` starts with `0xEF00`.
    CreateInitCodeStartingEF00,
    /// Invalid EVM Object Format (EOF) init code.
    InvalidEOFInitCode,
    /// `ExtDelegateCall` calling a non EOF contract.
    InvalidExtDelegateCallTarget,

    // Error Codes
    /// Out of gas error.
    OutOfGas = 0x20,
    /// Out of gas error encountered during memory expansion.
    MemoryOOG,
    /// The memory limit of the EVM has been exceeded.
    MemoryLimitOOG,
    /// Out of gas error encountered during the execution of a precompiled contract.
    PrecompileOOG,
    /// Out of gas error encountered while calling an invalid operand.
    InvalidOperandOOG,
    /// Out of gas error encountered while checking for reentrancy sentry.
    ReentrancySentryOOG,
    /// Unknown or invalid opcode.
    OpcodeNotFound,
    /// Invalid `CALL` with value transfer in static context.
    CallNotAllowedInsideStatic,
    /// Invalid state modification in static call.
    StateChangeDuringStaticCall,
    /// An undefined bytecode value encountered during execution.
    InvalidFEOpcode,
    /// Invalid jump destination. Dynamic jumps points to invalid not jumpdest opcode.
    InvalidJump,
    /// The feature or opcode is not activated in this version of the EVM.
    NotActivated,
    /// Attempting to pop a value from an empty stack.
    StackUnderflow,
    /// Attempting to push a value onto a full stack.
    StackOverflow,
    /// Invalid memory or storage offset.
    OutOfOffset,
    /// Address collision during contract creation.
    CreateCollision,
    /// Payment amount overflow.
    OverflowPayment,
    /// Error in precompiled contract execution.
    PrecompileError,
    /// Nonce overflow.
    NonceOverflow,
    /// Exceeded contract size limit during creation.
    CreateContractSizeLimit,
    /// Created contract starts with invalid bytes (`0xEF`).
    CreateContractStartingWithEF,
    /// Exceeded init code size limit (EIP-3860:  Limit and meter initcode).
    CreateInitCodeSizeLimit,
    /// Fatal external error. Returned by database.
    FatalExternalError,
    /// Invalid encoding of an instruction's immediate operand.
    InvalidImmediateEncoding,
}

impl InstrStop {
    /// Returns whether execution completed successfully.
    #[doc(alias = "is_ok")]
    #[inline]
    pub const fn is_success(self) -> bool {
        matches!(self, Self::Stop | Self::Return | Self::SelfDestruct)
    }

    /// Returns whether execution reverted without an exceptional halt.
    #[inline]
    pub const fn is_revert(self) -> bool {
        matches!(
            self,
            Self::Revert
                | Self::CallTooDeep
                | Self::OutOfFunds
                | Self::CreateInitCodeStartingEF00
                | Self::InvalidEOFInitCode
                | Self::InvalidExtDelegateCallTarget
        )
    }

    /// Returns whether execution reverted without an exceptional halt.
    #[inline]
    pub const fn is_reverted(self) -> bool {
        self.is_revert()
    }

    /// Returns whether execution completed successfully or reverted.
    #[inline]
    pub const fn is_ok_or_revert(self) -> bool {
        self.is_success() || self.is_revert()
    }

    /// Returns whether execution halted with an exceptional error.
    #[inline]
    pub const fn is_error(self) -> bool {
        !self.is_ok_or_revert() && !matches!(self, Self::Suspend)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        bytecode::Bytecode,
        interpreter::instructions::tests::{TestConfig, TestHost},
    };
    use alloy_primitives::{Bytes, U256};

    #[test]
    fn main_smoke() {
        #[rustfmt::skip]
        let bytecode = core::hint::black_box(&[
            op::PUSH1, 0x01,
            op::PUSH1, 0x02,
            op::ADD,
            op::STOP,
        ][..]);
        type Config = TestConfig<{ SpecId::HOMESTEAD as u8 }>;

        let bytecode = Bytecode::new_legacy(Bytes::copy_from_slice(bytecode));
        let mut interpreter = Interpreter::new(
            bytecode,
            crate::env::TxEnv::default(),
            Message { gas_limit: 10_000, ..Message::default() },
        );
        let mut host = TestHost::default();
        interpreter.run::<Config>(&mut host);
    }

    #[test]
    fn basic() {
        const BASIC: &[u8] = &[op::PUSH1, 0x01, op::PUSH1, 0x02, op::ADD, op::STOP];

        macro_rules! check {
            ($spec_id:ident) => {{
                type Config = TestConfig<{ SpecId::$spec_id as u8 }>;
                let bytecode = Bytecode::new_legacy(Bytes::from_static(BASIC));
                let mut interpreter = Interpreter::new(
                    bytecode,
                    crate::env::TxEnv::default(),
                    Message { gas_limit: 10_000, ..Message::default() },
                );
                let mut host = TestHost::default();
                interpreter.run::<Config>(&mut host);
                assert!(interpreter.gas.remaining() > 0);
                assert_eq!(interpreter.pc, 6);
                assert_eq!(interpreter.stack_len, 1);
                assert_eq!(interpreter.stack[0], U256::from(3));
            }};
        }

        check!(FRONTIER);
        check!(HOMESTEAD);
    }
}
