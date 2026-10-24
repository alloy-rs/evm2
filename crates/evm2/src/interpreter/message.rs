use crate::{BaseEvmTypes, EvmTypesHost, bytecode::Bytecode};
use alloy_primitives::{Address, B256, Bytes, U256, keccak256};

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

/// Frame-local EVM call/create message executed by the interpreter for an EVM type family.
pub type Message<T = BaseEvmTypes> = MessageExt<<T as EvmTypesHost>::MessageExt>;

/// Frame-local EVM call/create message, parameterized by extension data.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MessageExt<E = ()> {
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
    /// Its meaning depends on [`MessageExt::kind`]: for call-family messages it is the account
    /// whose context is executed; for `CREATE`/`CREATE2` it is the address of the
    /// yet-to-be-created contract, derived when the message is constructed (from the creator
    /// and nonce, or from the salt and init-code hash).
    pub destination: Address,
    /// Address requested by the call opcode or transaction, before EIP-7702 code resolution.
    ///
    /// Unlike `destination`, this remains the requested callee for `CALLCODE` and
    /// `DELEGATECALL`. Native precompile dispatch uses this address before delegated bytecode.
    /// For create messages it is the derived contract address.
    pub call_target: Address,
    /// Immediate caller.
    pub caller: Address,
    /// Call input data, or initcode for create messages.
    pub input: Bytes,
    /// Value transferred with the message.
    pub value: U256,
    /// Bytecode resolved for this frame: the code at [`MessageExt::code_address`] (the resolved
    /// delegate's code for an EIP-7702 delegated call), or the initcode for create messages.
    ///
    /// Resolved by the message's producer when it is constructed, so frames never load accounts
    /// for code. Native precompile dispatch at [`MessageExt::call_target`] takes precedence.
    pub code: Bytecode,
    /// Address from which bytecode was resolved. This can differ from `destination` for
    /// `CALLCODE`, `DELEGATECALL`, and EIP-7702 delegated-code execution.
    pub code_address: Address,
    /// Whether the immediate caller frame is executing in a static context. Combined with a
    /// `STATICCALL` kind, this determines whether the new frame is static.
    pub caller_is_static: bool,
    /// CREATE2 salt. Ignored for other message kinds.
    pub salt: B256,
    /// EVM type-specific extension data.
    pub ext: E,
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
}

impl<E> MessageExt<E> {
    /// Replaces the extension, preserving all other message fields.
    #[inline]
    pub fn with_ext<F>(self, ext: F) -> MessageExt<F> {
        self.map_ext(|_| ext)
    }

    /// Transforms the extension, preserving all other message fields.
    #[inline]
    pub fn map_ext<F>(self, f: impl FnOnce(E) -> F) -> MessageExt<F> {
        let Self {
            kind,
            depth,
            gas_limit,
            reservoir,
            destination,
            call_target,
            caller,
            input,
            value,
            code,
            code_address,
            caller_is_static,
            salt,
            ext,
            _non_exhaustive,
        } = self;
        MessageExt {
            kind,
            depth,
            gas_limit,
            reservoir,
            destination,
            call_target,
            caller,
            input,
            value,
            code,
            code_address,
            caller_is_static,
            salt,
            ext: f(ext),
            _non_exhaustive,
        }
    }

    /// Returns `true` if the frame executing this message is static: the caller is static or the
    /// message is a `STATICCALL`.
    #[inline]
    pub fn is_static(&self) -> bool {
        self.caller_is_static || self.kind == MessageKind::StaticCall
    }

    /// Returns `true` if the account whose context the message runs in is the requested
    /// [`call_target`](Self::call_target), i.e. it is not a `DELEGATECALL` or `CALLCODE` into
    /// another account.
    ///
    /// This compares addresses, so a `DELEGATECALL` or `CALLCODE` to the executing account's own
    /// address also counts as direct, and so does an EIP-7702 delegated `CALL`. Create messages
    /// are always direct. Native precompiles can use this to reject delegated calls.
    #[inline]
    pub fn is_direct_call(&self) -> bool {
        self.destination == self.call_target
    }
}

/// Derives the contract address a create message deploys to, i.e. its
/// [`destination`](MessageExt::destination).
///
/// `nonce` is the creator's (pre-bump) nonce and is only used by the `CREATE` scheme; `CREATE2`
/// derives the address from `salt` and the initcode hash instead.
#[inline]
pub fn derive_create_destination(
    kind: MessageKind,
    caller: &Address,
    salt: &B256,
    init_code: &[u8],
    nonce: u64,
) -> Address {
    match kind {
        MessageKind::Create2 => caller.create2(salt, keccak256(init_code)),
        _ => caller.create(nonce),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_static_follows_caller_or_staticcall_kind() {
        let message = |kind, caller_is_static| MessageExt::<()> {
            kind,
            caller_is_static,
            ..MessageExt::default()
        };

        assert!(!message(MessageKind::Call, false).is_static());
        assert!(!message(MessageKind::DelegateCall, false).is_static());
        assert!(message(MessageKind::Call, true).is_static());
        assert!(message(MessageKind::DelegateCall, true).is_static());
        assert!(message(MessageKind::StaticCall, false).is_static());
        assert!(message(MessageKind::StaticCall, true).is_static());
    }

    #[test]
    fn is_direct_call_rejects_delegated_code() {
        let target = Address::with_last_byte(1);
        let executing = Address::with_last_byte(2);
        let caller = Address::with_last_byte(3);
        let call = MessageExt::<()> {
            kind: MessageKind::Call,
            destination: target,
            call_target: target,
            caller,
            ..MessageExt::default()
        };
        let delegated = |kind, destination| MessageExt { kind, destination, ..call.clone() };

        assert!(call.is_direct_call());
        assert!(delegated(MessageKind::StaticCall, target).is_direct_call());
        assert!(delegated(MessageKind::Create, target).is_direct_call());
        assert!(!delegated(MessageKind::DelegateCall, executing).is_direct_call());
        assert!(!delegated(MessageKind::CallCode, executing).is_direct_call());
        // Address-based: a delegate call to the executing account's own address is direct.
        assert!(delegated(MessageKind::DelegateCall, target).is_direct_call());
    }
}
