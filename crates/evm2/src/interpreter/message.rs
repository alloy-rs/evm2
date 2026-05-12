use alloy_primitives::{Address, B256, Bytes, U256};
use derive_where::derive_where;

use crate::{BaseEvmTypes, EvmTypes};

/// EVM message kind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum MessageKind {
    /// Regular `CALL` message.
    #[default]
    Call,
    /// `DELEGATECALL` message.
    DelegateCall,
    /// `CALLCODE` message.
    CallCode,
    /// `CREATE` message.
    Create,
    /// `CREATE2` message.
    Create2,
    /// `STATICCALL` message.
    StaticCall,
}

impl MessageKind {
    /// Returns `true` if the message is CREATE or CREATE2.
    #[inline]
    pub const fn is_create(&self) -> bool {
        matches!(self, Self::Create | Self::Create2)
    }
}

/// Frame-local EVM call/create message executed by the interpreter.
#[derive_where(Clone, Debug, Default, PartialEq, Eq; T::MessageExt)]
pub struct Message<T: EvmTypes = BaseEvmTypes> {
    /// Message kind.
    pub kind: MessageKind,
    /// Current call depth.
    pub depth: u16,
    /// Gas available to this message.
    pub gas_limit: u64,
    /// Account whose context is being executed.
    pub destination: Address,
    /// Immediate caller.
    pub caller: Address,
    /// Call input data, or initcode for create messages.
    pub input: Bytes,
    /// Value transferred with the message.
    pub value: U256,
    /// Address whose code is being executed. This can differ from `destination` for `CALLCODE`,
    /// `DELEGATECALL`, and EIP-7702 delegated-code execution.
    pub code_address: Address,
    /// Whether native precompile dispatch is disabled for this frame because its bytecode was
    /// loaded through an EIP-7702 delegation designation.
    pub disable_precompiles: bool,
    /// CREATE2 salt. Ignored for other message kinds.
    pub salt: B256,
    /// EVM type-specific extension data.
    pub ext: T::MessageExt,
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
}
