//! EVM interpreter.

pub(crate) mod gas;
pub use crate::version::{GasId, GasParams, num_words};
pub use gas::{Gas, GasTracker, MemoryGas};

#[macro_use]
mod utils;

pub(crate) mod instructions;
#[doc(hidden)]
pub use crate::version::{InstructionImplTable, StaticGasTable};
#[doc(hidden)]
pub use instructions::table;
#[doc(hidden)]
pub use instructions::table::{Instruction, InstructionImplFn};

pub(crate) mod opcode;
pub use opcode::op;

mod ctrl;
pub use ctrl::{BytecodeRef, Pc};

mod stack;
pub use stack::{Stack, StackMut, Word};

mod memory;
pub use memory::Memory;

mod message;
pub use message::{Message, MessageKind};

mod state;
pub use state::{Host, MessageResult, State};

mod runtime;
pub use runtime::Interpreter;

#[doc(hidden)]
pub type Result<T = (), E = InstrStop> = core::result::Result<T, E>;

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
        BaseEvmConfig, EvmRuntimeConfig, SpecId,
        bytecode::Bytecode,
        interpreter::instructions::tests::{TestHost, TestTypes},
    };
    use alloy_primitives::{Bytes, U256};

    #[test]
    fn defaults() {
        assert_eq!(SpecId::DEFAULT, SpecId::default());
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
        type Config = BaseEvmConfig<{ SpecId::HOMESTEAD as u8 }>;

        let bytecode = Bytecode::new_legacy(Bytes::copy_from_slice(bytecode));
        let mut interpreter = Interpreter::<TestTypes>::new(
            bytecode,
            crate::env::TxEnv::default(),
            Message { gas_limit: 10_000, ..Message::default() },
            false,
        );
        let mut host = TestHost::default();
        interpreter.run_with(EvmRuntimeConfig::new::<Config>(), &mut host);
    }

    #[test]
    fn basic() {
        const BASIC: &[u8] = &[op::PUSH1, 0x01, op::PUSH1, 0x02, op::ADD, op::STOP];

        macro_rules! check {
            ($spec_id:ident) => {{
                type Config = BaseEvmConfig<{ SpecId::$spec_id as u8 }>;
                let bytecode = Bytecode::new_legacy(Bytes::from_static(BASIC));
                let mut interpreter = Interpreter::<TestTypes>::new(
                    bytecode,
                    crate::env::TxEnv::default(),
                    Message { gas_limit: 10_000, ..Message::default() },
                    false,
                );
                let mut host = TestHost::default();
                interpreter.run_with(EvmRuntimeConfig::new::<Config>(), &mut host);
                assert!(interpreter.gas.remaining() > 0);
                assert_eq!(interpreter.stack[0], U256::from(3));
            }};
        }

        check!(FRONTIER);
        check!(HOMESTEAD);
    }
}
