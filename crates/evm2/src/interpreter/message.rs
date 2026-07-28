use crate::{BaseEvmTypes, EvmTypesHost};
use alloy_primitives::{Address, B256, Bytes, U256, keccak256};
use derive_where::derive_where;

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
pub struct Message<T: EvmTypesHost = BaseEvmTypes> {
    /// Message kind.
    pub kind: MessageKind,
    /// Current call depth.
    pub depth: u16,
    /// Gas available to this message.
    pub gas_limit: u64,
    /// EIP-8037 state-gas reservoir inherited from the parent frame.
    ///
    /// The reservoir is a shared pool threaded down into child frames and
    /// reconciled back on return by
    /// [`GasTracker::merge_child_gas`](crate::interpreter::GasTracker::merge_child_gas).
    /// Zero for non-Amsterdam execution.
    pub reservoir: u64,
    /// The account this message targets.
    ///
    /// Its meaning depends on [`Message::kind`]: for call-family messages it is the account whose
    /// context is executed; for `CREATE`/`CREATE2` it is the address of the yet-to-be-created
    /// contract, derived when the message is constructed (from the creator and nonce, or from the
    /// salt and init-code hash).
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
    /// Whether the immediate caller frame is executing in a static context. Combined with a
    /// `STATICCALL` kind, this determines whether the new frame is static.
    pub caller_is_static: bool,
    /// CREATE2 salt. Ignored for other message kinds.
    pub salt: B256,
    /// EVM type-specific extension data.
    pub ext: T::MessageExt,
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
}

impl<T: EvmTypesHost> Message<T> {
    /// Returns the keccak256 hash of the init code ([`Message::input`]).
    ///
    /// Only meaningful for create messages, where `input` holds the initcode.
    #[inline]
    pub fn init_code_hash(&self) -> B256 {
        keccak256(self.input.as_ref())
    }

    /// Derives the address that this create message deploys to.
    ///
    /// `nonce` is the creator's nonce and is only used by the `CREATE` scheme; `CREATE2` derives
    /// the address from the salt and [`Message::init_code_hash`]. The result is written into
    /// [`Message::destination`] when the message is constructed.
    #[inline]
    pub fn created_address(&self, nonce: u64) -> Address {
        match self.kind {
            MessageKind::Create2 => self.caller.create2(self.salt, self.init_code_hash()),
            _ => self.caller.create(nonce),
        }
    }
}
