//! Account models held by the state overlay and emitted in transitions.

use crate::{bytecode::Bytecode, interpreter::Word};
use alloy_primitives::{B256, KECCAK256_EMPTY, U256};

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

/// Mutable account state cached by [`State`](super::State).
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

/// Current-transaction account overlay and EIP-2929/account-lifetime metadata.
///
/// `original_info` captures the account at the start of the transaction when it is first loaded,
/// while `present_info` is the live overlay after EVM mutations. Keeping both lets
/// [`State`](super::State) emit the transaction's account transition without re-reading the backing
/// database. A warm- or touch-only entry can exist without being loaded; `loaded` distinguishes a
/// not-yet-loaded entry from one that was loaded as non-existent or deleted.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct TrackedAccount {
    /// Account info at the start of the transaction. `None` means the account did not exist. Only
    /// meaningful when `loaded` is true.
    pub(super) original: Option<AccountInfo>,
    /// Present account overlay after mutations. `None` means the account is absent/deleted.
    pub(super) present: Option<Account>,
    /// Whether the account has been loaded from the backing database in this transaction.
    pub(super) is_loaded: bool,
    /// Whether this account is warm in the current transaction.
    pub(super) is_warm: bool,
    /// Whether this account is touched for transaction-finalization account-lifetime rules.
    pub(super) is_touched: bool,
}

impl TrackedAccount {
    #[inline]
    pub(super) const fn is_empty(&self) -> bool {
        !self.is_loaded && !self.is_warm && !self.is_touched
    }

    /// Returns the present account overlay if the account has been loaded this transaction.
    ///
    /// `Some(&None)` means the account was loaded as non-existent or deleted; `None` means the
    /// account has not been loaded in this transaction.
    #[inline]
    pub(super) const fn present_if_loaded(&self) -> Option<&Option<Account>> {
        if self.is_loaded { Some(&self.present) } else { None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evm::{CacheDB, state::State};
    use alloy_primitives::Address;

    #[test]
    fn transfer_to_self_requires_balance() {
        let address = Address::from([0x77; 20]);
        let mut database = CacheDB::default();
        database.insert_account_info(&address, AccountInfo::default().with_balance(Word::from(3)));
        let mut state = State::new(database);

        assert!(!state.transfer(&address, &address, &Word::from(4)).unwrap());
        assert!(state.transfer(&address, &address, &Word::from(3)).unwrap());
    }
}
