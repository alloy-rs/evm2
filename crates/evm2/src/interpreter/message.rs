use super::CallMemory;
use crate::{BaseEvmTypes, EvmTypesHost, bytecode::Bytecode};
use alloy_primitives::{Address, B256, Bytes, U256, keccak256};
use core::ops::Range;

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
    /// `DELEGATECALL`. For create messages it is the derived contract address.
    pub call_target: Address,
    /// Immediate caller.
    pub caller: Address,
    /// Call input data, or initcode for create messages.
    pub input: CallInput,
    /// Value transferred with the message.
    pub value: U256,
    /// Bytecode this frame executes: the code at [`MessageExt::code_address`] (the resolved
    /// delegate's code for an EIP-7702 delegated call), or the initcode for create messages.
    ///
    /// Resolved by the message's producer when it is constructed, so frames never load accounts
    /// for code.
    pub code: Bytecode,
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
    pub ext: E,
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
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

impl<E> MessageExt<E> {
    /// Copies input out of caller memory so the message can be retained after execution.
    #[inline]
    pub fn into_owned(self, memory: &CallMemory) -> Self {
        Self { input: self.input.to_bytes(memory).into(), ..self }
    }
}

/// Call input stored as owned bytes or a range in live caller memory.
///
/// Cloning preserves memory ranges. Use [`Self::to_bytes`] to retain input after a call returns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallInput {
    inner: InputStorage,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum InputStorage {
    Owned(Bytes),
    Memory { pool: usize, frame: usize, generation: u64, range: Range<usize> },
}

impl CallInput {
    /// Returns the input length without accessing its memory.
    #[inline]
    pub fn len(&self) -> usize {
        match &self.inner {
            InputStorage::Owned(bytes) => bytes.len(),
            InputStorage::Memory { range, .. } => range.len(),
        }
    }

    /// Returns whether the input is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns the input bytes through their live memory context.
    ///
    /// # Panics
    ///
    /// Panics if a memory range belongs to another context or its call has returned.
    #[inline]
    pub fn as_slice<'a>(&'a self, memory: &'a CallMemory) -> &'a [u8] {
        match &self.inner {
            InputStorage::Owned(bytes) => bytes,
            InputStorage::Memory { pool, frame, generation, range } => {
                memory.slice(*pool, *frame, *generation, range.clone())
            }
        }
    }

    /// Returns owned bytes, copying only memory-backed input.
    ///
    /// # Panics
    ///
    /// Panics if a memory range belongs to another context or its call has returned.
    #[inline]
    pub fn to_bytes(&self, memory: &CallMemory) -> Bytes {
        match &self.inner {
            InputStorage::Owned(bytes) => bytes.clone(),
            InputStorage::Memory { .. } => Bytes::copy_from_slice(self.as_slice(memory)),
        }
    }

    pub(super) const fn memory(
        pool: usize,
        frame: usize,
        generation: u64,
        range: Range<usize>,
    ) -> Self {
        Self { inner: InputStorage::Memory { pool, frame, generation, range } }
    }
}

impl Default for CallInput {
    #[inline]
    fn default() -> Self {
        Self { inner: InputStorage::Owned(Default::default()) }
    }
}

impl From<Bytes> for CallInput {
    #[inline]
    fn from(bytes: Bytes) -> Self {
        Self { inner: InputStorage::Owned(bytes) }
    }
}
