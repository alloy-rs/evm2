use crate::{
    BaseEvmTypes, EvmTypesHost, Version,
    bytecode::Bytecode,
    interpreter::{InstrStop, Memory, Word},
    trustme,
};
use alloy_primitives::{Address, B256, Bytes, U256, keccak256};
use core::{
    fmt,
    ops::{Deref, Range},
};

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
    /// Inspector-visible call depth, starting at
    /// [`Version::initial_call_depth`](crate::Version::initial_call_depth).
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
    pub input: MessageInput,
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

/// Returns the reason a create message fails before its destination is accessed, if any.
///
/// `balance` and `nonce` are the creator's, and are zero for an absent account. Failures are
/// checked in order: call depth, insufficient balance for `value`, then, for a nested create
/// (`depth` above [`Version::initial_call_depth`]), a creator nonce at `u64::MAX` (EIP-2681). The
/// transaction itself bumps the nonce of a top-level create, so that one skips the nonce check.
#[inline]
pub fn create_preaccess_failure(
    balance: Word,
    nonce: u64,
    value: Word,
    depth: u16,
    version: &Version,
) -> Option<InstrStop> {
    if depth > version.max_call_depth() {
        Some(InstrStop::CallTooDeep)
    } else if balance < value {
        Some(InstrStop::OutOfFunds)
    } else if depth > version.initial_call_depth && nonce == u64::MAX {
        Some(InstrStop::NonceOverflow)
    } else {
        None
    }
}

/// The input of a message: call data, or initcode for create messages.
///
/// When [`Evm`](crate::Evm) runs a call instruction's message, the input is a range of the caller
/// frame's memory instead of a copy, since the caller waits for the call to return. While an
/// inspector's call hooks hold the message, such an input derefs to no bytes: read it with
/// [`as_bytes`](Self::as_bytes) or [`to_bytes`](Self::to_bytes) and the caller's memory (the hook's
/// `interp.memory()`), and to change it, copy it out and assign an owned input, as in
/// `message.input = bytes.into()`. A hook that writes the caller's memory without replacing the
/// input changes what the child reads.
#[derive(Default)]
pub struct MessageInput {
    /// The owned bytes, or the caller's memory with its lifetime erased while attached. Empty for
    /// a detached input in the caller's memory.
    bytes: Bytes,
    /// The input's range in the caller frame's memory, unless the input owns its bytes.
    caller_memory: Option<Range<usize>>,
}

impl MessageInput {
    /// Borrows `bytes`, which are `range` of the caller frame's memory, without copying.
    ///
    /// # Safety
    ///
    /// `bytes` must stay valid and unchanged until the input is detached.
    #[inline]
    pub(crate) unsafe fn borrowed(bytes: &[u8], range: Range<usize>) -> Self {
        debug_assert_eq!(bytes.len(), range.len());
        // SAFETY: The caller keeps the bytes alive and unchanged until the input is detached, and
        // cloning an attached input copies its bytes.
        Self {
            bytes: Bytes::from_static(unsafe { trustme::decouple_lt(bytes) }),
            caller_memory: Some(range),
        }
    }

    /// Returns the length of the input.
    #[inline]
    pub fn len(&self) -> usize {
        match &self.caller_memory {
            Some(range) => range.len(),
            None => self.bytes.len(),
        }
    }

    /// Returns whether the input is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns the input's range in the caller frame's memory, unless it owns its bytes.
    #[inline]
    pub fn caller_memory(&self) -> Option<Range<usize>> {
        self.caller_memory.clone()
    }

    /// Returns the input bytes, reading them from `memory`, the caller frame's memory, unless the
    /// input owns them.
    ///
    /// # Panics
    ///
    /// Panics if the input's range is out of bounds of `memory`.
    #[inline]
    pub fn as_bytes<'a>(&'a self, memory: &'a Memory) -> &'a [u8] {
        match &self.caller_memory {
            Some(range) => memory.slice(range.start, range.len()),
            None => &self.bytes,
        }
    }

    /// Returns the input as owned bytes, copying them from `memory`, the caller frame's memory,
    /// unless the input owns them.
    ///
    /// # Panics
    ///
    /// Panics if the input's range is out of bounds of `memory`.
    #[inline]
    pub fn to_bytes(&self, memory: &Memory) -> Bytes {
        match &self.caller_memory {
            Some(range) => Bytes::copy_from_slice(memory.slice(range.start, range.len())),
            None => self.bytes.clone(),
        }
    }

    /// Points an input in the caller's memory at `memory`, so that it derefs to its bytes.
    ///
    /// # Safety
    ///
    /// `memory` must be the caller frame's memory, and must stay in place and unchanged until
    /// [`detach`](Self::detach).
    ///
    /// # Panics
    ///
    /// Panics if the input's range is out of bounds of `memory`.
    #[inline]
    pub(crate) unsafe fn attach(&mut self, memory: &Memory) {
        if let Some(range) = &self.caller_memory {
            let bytes = memory.slice(range.start, range.len());
            // SAFETY: Upheld by the caller.
            self.bytes = Bytes::from_static(unsafe { trustme::decouple_lt(bytes) });
        }
    }

    /// Returns the range of an input in the caller's memory that has no bytes to deref to.
    #[inline]
    fn detached(&self) -> Option<&Range<usize>> {
        self.caller_memory.as_ref().filter(|range| self.bytes.len() != range.len())
    }

    /// Drops the pointer of an input in the caller's memory, keeping only its range.
    #[inline]
    pub(crate) fn detach(&mut self) {
        if self.caller_memory.is_some() {
            self.bytes = Bytes::new();
        }
    }
}

impl Clone for MessageInput {
    /// Copies the bytes of an attached input, so that a clone never points into memory. A detached
    /// input clones its range.
    #[inline]
    fn clone(&self) -> Self {
        if self.caller_memory.is_some() && self.detached().is_none() {
            return Self::from(Bytes::copy_from_slice(&self.bytes));
        }
        Self { bytes: self.bytes.clone(), caller_memory: self.caller_memory.clone() }
    }
}

impl fmt::Debug for MessageInput {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.caller_memory {
            Some(range) if self.detached().is_some() => {
                f.debug_tuple("CallerMemory").field(range).finish()
            }
            _ => self.bytes.fmt(f),
        }
    }
}

impl PartialEq for MessageInput {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        match (self.detached(), other.detached()) {
            (None, None) => self.bytes == other.bytes,
            (a, b) => a == b,
        }
    }
}

impl Eq for MessageInput {}

impl Deref for MessageInput {
    type Target = [u8];

    #[inline]
    fn deref(&self) -> &[u8] {
        &self.bytes
    }
}

impl From<Bytes> for MessageInput {
    #[inline]
    fn from(bytes: Bytes) -> Self {
        Self { bytes, caller_memory: None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SpecId;

    #[test]
    fn create_preaccess_failure_order_and_nesting() {
        let mut version = Version::new(SpecId::CANCUN);
        version.initial_call_depth = 5;
        let max = version.max_call_depth();
        let check = |balance: u64, nonce: u64, value: u64, depth: u16| {
            create_preaccess_failure(Word::from(balance), nonce, Word::from(value), depth, &version)
        };

        assert_eq!(check(0, u64::MAX, 1, max + 1), Some(InstrStop::CallTooDeep));
        assert_eq!(check(0, u64::MAX, 1, max), Some(InstrStop::OutOfFunds));
        assert_eq!(check(1, u64::MAX, 1, max), Some(InstrStop::NonceOverflow));
        assert_eq!(check(1, u64::MAX, 1, version.initial_call_depth), None);
        assert_eq!(check(0, 0, 0, max), None);
    }

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

    #[test]
    fn caller_memory_input() {
        let mut memory = Memory::new();
        memory.resize(0, 3).unwrap();
        memory.set(0, &[1, 2, 3]);
        // SAFETY: `memory` is unchanged until `input` is detached.
        let mut input = unsafe { MessageInput::borrowed(memory.slice(1, 2), 1..3) };
        assert_eq!(&*input, [2, 3]);
        let clone = input.clone();
        input.detach();
        let range = input.clone();
        memory.set(1, &[7]);

        // An attached input's clone copies its bytes.
        assert_eq!(&*clone, [2, 3]);
        assert_eq!(clone.caller_memory(), None);
        // A detached input reads the caller's memory.
        assert!(input.as_ref().is_empty());
        assert_eq!(input.len(), 2);
        assert_eq!(input.as_bytes(&memory), [7, 3]);
        assert_eq!(input.to_bytes(&memory)[..], [7, 3]);
        assert_eq!(range, input);
        assert_eq!(range.caller_memory(), Some(1..3));
    }
}
