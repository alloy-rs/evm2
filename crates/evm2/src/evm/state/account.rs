//! Account and storage overlay value types.

use crate::{bytecode::Bytecode, interpreter::Word};
use alloy_primitives::{B256, KECCAK256_EMPTY, U256, map::U256Map};

/// A value tracked together with the value it had at an aggregation boundary.
///
/// `Tracked` is used by [`super::State`] to keep a transaction overlay over the backing
/// database, by [`super::StateChanges`] to describe transaction transitions, and by
/// [`super::BlockStateAccumulator`] to describe block transitions. `original` is the value at the
/// start of the current transaction or block, while `current` is the value after all observed
/// mutations in that boundary. When a transaction is accepted, `current` becomes the next
/// transaction's `original` without writing anything to the backing database.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct Tracked<T> {
    /// Value at the start of the aggregation boundary.
    original: T,
    /// Value after observed mutations in the aggregation boundary.
    current: T,
}

impl<T> Tracked<T> {
    /// Creates a tracked value from distinct original and current values.
    #[inline]
    pub const fn from_parts(original: T, current: T) -> Self {
        Self { original, current }
    }

    /// Creates a tracked value whose original and current values are equal.
    #[inline]
    pub fn new(value: T) -> Self
    where
        T: Clone,
    {
        Self { original: value.clone(), current: value }
    }

    /// Returns the original value.
    #[inline]
    pub const fn original(&self) -> &T {
        &self.original
    }

    /// Returns the current value.
    #[inline]
    pub const fn current(&self) -> &T {
        &self.current
    }

    /// Updates the current value.
    #[inline]
    pub fn set_current(&mut self, current: T) {
        self.current = current;
    }

    /// Splits this tracked value into original and current values.
    #[inline]
    pub fn into_parts(self) -> (T, T) {
        (self.original, self.current)
    }
}

impl<T: PartialEq> Tracked<T> {
    /// Returns whether the current value differs from the original value.
    #[inline]
    pub fn is_changed(&self) -> bool {
        self.original != self.current
    }
}

/// Account information loaded from the backing database or emitted in a state
/// transition.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct AccountInfo {
    /// Account balance.
    pub balance: Word,
    /// Account nonce.
    pub nonce: u64,
    /// Hash of the raw bytes in `code`, or the empty code hash.
    pub code_hash: B256,
    /// Bytecode associated with this account.
    pub code: Option<Bytecode>,
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
}

impl Default for AccountInfo {
    #[inline]
    fn default() -> Self {
        Self {
            balance: U256::ZERO,
            nonce: 0,
            code_hash: KECCAK256_EMPTY,
            code: Some(Bytecode::default()),
            _non_exhaustive: (),
        }
    }
}

impl AccountInfo {
    /// Creates a new [`AccountInfo`] with the given fields.
    #[inline]
    pub const fn new(balance: Word, nonce: u64, code_hash: B256, code: Bytecode) -> Self {
        Self { balance, nonce, code_hash, code: Some(code), _non_exhaustive: () }
    }

    /// Creates a new [`AccountInfo`] with the given code.
    #[inline]
    pub fn with_code(self, code: Bytecode) -> Self {
        Self { code_hash: code.hash_slow(), code: Some(code), ..self }
    }

    /// Creates a new [`AccountInfo`] with the given balance.
    #[inline]
    pub const fn with_balance(mut self, balance: Word) -> Self {
        self.balance = balance;
        self
    }

    /// Creates a new [`AccountInfo`] with the given nonce.
    #[inline]
    pub const fn with_nonce(mut self, nonce: u64) -> Self {
        self.nonce = nonce;
        self
    }

    /// Sets account bytecode and updates the code hash.
    #[inline]
    pub fn set_code(&mut self, code: Bytecode) {
        self.code_hash = code.hash_slow();
        self.code = Some(code);
    }

    /// Returns whether this account is empty by the Spurious Dragon definition.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.balance.is_zero() && self.nonce == 0 && self.code_hash == KECCAK256_EMPTY
    }
}

/// Mutable account state cached by [`super::State`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Account {
    /// Account nonce.
    pub nonce: u64,
    /// Account balance.
    pub balance: Word,
    /// Account code hash.
    pub code_hash: B256,
    /// Cached account bytecode.
    pub code: Bytecode,
    /// Whether the account was created in the current transaction.
    pub just_created: bool,
    /// Whether the account code has been modified.
    pub code_changed: bool,
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
}

impl Account {
    /// Creates an account from database account info.
    #[inline]
    pub fn from_info(info: AccountInfo) -> Self {
        Self {
            nonce: info.nonce,
            balance: info.balance,
            code_hash: info.code_hash,
            code: info.code.unwrap_or_default(),
            just_created: false,
            code_changed: false,
            _non_exhaustive: (),
        }
    }

    /// Returns account info.
    #[inline]
    pub fn info(&self) -> AccountInfo {
        AccountInfo {
            balance: self.balance,
            nonce: self.nonce,
            code_hash: self.code_hash,
            code: Some(self.code.clone()),
            _non_exhaustive: (),
        }
    }

    /// Returns whether this account is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.nonce == 0 && self.balance.is_zero() && self.code_hash == KECCAK256_EMPTY
    }
}

/// Persistent storage overlay for one account.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StorageOverlay {
    /// Whether consumers must delete all pre-existing storage for the account
    /// before applying individual slot changes.
    pub wiped: bool,
    /// Loaded or changed storage slots.
    pub slots: U256Map<Tracked<Word>>,
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
}
