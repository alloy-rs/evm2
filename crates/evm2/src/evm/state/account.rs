//! Account models held by the state overlay and emitted in transitions.

use super::{CacheDB, DbResult, DynDatabase, JournalEntry, WarmAddresses};
use crate::{bytecode::Bytecode, interpreter::Word};
use alloc::{boxed::Box, vec::Vec};
use alloy_primitives::{Address, B256, KECCAK256_EMPTY, U256};
use derive_where::derive_where;

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

/// A mutable, journaled handle to an account loaded into the transaction overlay.
///
/// Returned by [`State::journaled_account`](super::State::journaled_account). The account has
/// already been read from the backing database and preserved in the transaction overlay; this
/// handle ties that overlay slot to the revert journal so a mutation and its rollback bookkeeping
/// cannot drift apart, mirroring revm's `JournaledAccount`.
///
/// The first mutating access records a single [`JournalEntry::AccountChange`] snapshot of the
/// account as it was when the handle was created, so every change made through the handle is
/// reverted together by [`State::rollback`](super::State::rollback). A handle used only for reads
/// records nothing. Mutating a currently-absent account materializes an empty one.
///
/// The handle also carries the backing database and the transaction-initial base warm set, so it
/// can load storage and code on demand and answer warm-access queries without going back through
/// [`State`](super::State), mirroring the database and access-list references revm's
/// `JournaledAccount` holds.
#[derive_where(Debug)]
pub struct JournaledAccount<'a> {
    /// Address of the account.
    address: Address,
    /// Transaction overlay entry: account overlay plus warm/touched access metadata.
    tracked: &'a mut TrackedAccount,
    /// Revert journal shared with the owning [`State`](super::State).
    journal: &'a mut Vec<JournalEntry>,
    /// Database plus accepted transaction-boundary overlay, for on-demand storage and code loads.
    #[derive_where(skip)]
    database: &'a mut CacheDB<Box<dyn DynDatabase>>,
    /// Transaction-initial base warm set (precompiles, coinbase, EIP-2930 access list).
    warm_addresses: &'a mut WarmAddresses,
    /// Whether the revert snapshot has already been recorded for this handle.
    snapshotted: bool,
}

/// Returns a freshly materialized empty account.
#[inline]
fn empty_account() -> Account {
    Account { code_hash: KECCAK256_EMPTY, ..Account::default() }
}

impl<'a> JournaledAccount<'a> {
    /// Creates a handle over a loaded account overlay slot, the revert journal, the backing
    /// database, and the transaction-initial base warm set.
    #[inline]
    pub(super) const fn new(
        address: Address,
        tracked: &'a mut TrackedAccount,
        journal: &'a mut Vec<JournalEntry>,
        database: &'a mut CacheDB<Box<dyn DynDatabase>>,
        warm_addresses: &'a mut WarmAddresses,
    ) -> Self {
        Self { address, tracked, journal, database, warm_addresses, snapshotted: false }
    }

    /// Returns the account address.
    #[inline]
    pub const fn address(&self) -> Address {
        self.address
    }

    /// Returns the present account overlay, or `None` when the account is absent.
    #[inline]
    pub const fn get(&self) -> Option<&Account> {
        self.tracked.present.as_ref()
    }

    /// Returns whether the account currently exists in the overlay.
    #[inline]
    pub const fn exists(&self) -> bool {
        self.tracked.present.is_some()
    }

    /// Returns the account balance, or zero when the account is absent.
    #[inline]
    pub fn balance(&self) -> Word {
        self.tracked.present.as_ref().map_or(U256::ZERO, |account| account.balance)
    }

    /// Returns the account nonce, or zero when the account is absent.
    #[inline]
    pub fn nonce(&self) -> u64 {
        self.tracked.present.as_ref().map_or(0, |account| account.nonce)
    }

    /// Returns the account code hash, or the empty code hash when the account is absent.
    #[inline]
    pub fn code_hash(&self) -> B256 {
        self.tracked.present.as_ref().map_or(KECCAK256_EMPTY, |account| account.code_hash)
    }

    /// Returns whether the account is warm for EIP-2929 gas accounting, consulting both the
    /// transaction's base warm set (precompiles, coinbase, EIP-2930 access list) and runtime
    /// warmth recorded during execution.
    #[inline]
    pub fn is_warm(&self) -> bool {
        self.tracked.is_warm || self.warm_addresses.is_warm(&self.address)
    }

    /// Loads the account's bytecode, reading it from the backing database by code hash when it is
    /// not already cached in the overlay.
    ///
    /// Returns empty bytecode when the account is absent or has the empty code hash.
    #[inline]
    pub fn load_code(&mut self) -> DbResult<Bytecode> {
        let Some(account) = self.tracked.present.as_ref() else {
            return Ok(Bytecode::default());
        };
        if account.code_hash == KECCAK256_EMPTY {
            return Ok(Bytecode::default());
        }
        if !account.code.is_empty() {
            return Ok(account.code.clone());
        }
        let code_hash = account.code_hash;
        self.database.get_code_by_hash(&code_hash)
    }

    /// Touches the account, recording a [`JournalEntry::Touch`] the first time it is touched.
    ///
    /// Touched accounts participate in EIP-158/161 empty-account cleanup at transaction
    /// finalization even when no field changes.
    #[inline]
    pub fn touch(&mut self) {
        if !self.tracked.is_touched {
            self.tracked.is_touched = true;
            self.journal.push(JournalEntry::Touch { address: self.address });
        }
    }

    /// Marks the account warm for EIP-2929 gas accounting, recording a
    /// [`JournalEntry::AccountWarmed`] when this access transitions it from cold to warm.
    ///
    /// Returns `true` if the account was cold before this call. Accounts already warm through the
    /// base warm set stay warm across rollback, so warming them again records nothing.
    #[inline]
    pub fn warm(&mut self) -> bool {
        if self.warm_addresses.is_warm(&self.address) {
            return false;
        }
        let was_cold = !self.tracked.is_warm;
        self.tracked.is_warm = true;
        if was_cold {
            self.journal.push(JournalEntry::AccountWarmed { address: self.address });
        }
        was_cold
    }

    /// Sets the account balance, touching the account and recording a revert snapshot.
    #[inline]
    pub fn set_balance(&mut self, balance: Word) {
        self.touch();
        self.get_or_insert().balance = balance;
    }

    /// Sets the account nonce, touching the account and recording a revert snapshot.
    #[inline]
    pub fn set_nonce(&mut self, nonce: u64) {
        self.touch();
        self.get_or_insert().nonce = nonce;
    }

    /// Bumps the account nonce by one, touching the account and recording a revert snapshot.
    ///
    /// Returns `false` without changing the nonce when it is already at the maximum value.
    #[inline]
    pub fn bump_nonce(&mut self) -> bool {
        self.touch();
        let Some(nonce) = self.nonce().checked_add(1) else {
            return false;
        };
        self.get_or_insert().nonce = nonce;
        true
    }

    /// Sets the account code and its hash, touching the account and recording a revert snapshot.
    ///
    /// The caller is responsible for `code_hash` matching `code`; use [`Self::set_code_slow`] to
    /// have the hash computed.
    #[inline]
    pub fn set_code(&mut self, code_hash: B256, code: Bytecode) {
        self.touch();
        let account = self.get_or_insert();
        account.code_hash = code_hash;
        account.code = code;
        account.code_changed = true;
    }

    /// Sets the account code, computing its hash, touching the account and recording a revert
    /// snapshot.
    #[inline]
    pub fn set_code_slow(&mut self, code: Bytecode) {
        let code_hash = code.hash_slow();
        self.set_code(code_hash, code);
    }

    /// Records the revert snapshot on the first mutating access and returns the live account,
    /// materializing an empty one when it is currently absent.
    ///
    /// Hold the handle and call this repeatedly to make several changes under a single snapshot.
    #[inline]
    pub fn get_or_insert(&mut self) -> &mut Account {
        if !self.snapshotted {
            self.journal.push(JournalEntry::AccountChange {
                address: self.address,
                previous: self.tracked.present.clone(),
            });
            self.snapshotted = true;
        }
        self.tracked.present.get_or_insert_with(empty_account)
    }

    /// Records the revert snapshot, consumes the handle, and returns the live account for the
    /// remainder of the overlay borrow, materializing an empty one when it is currently absent.
    #[inline]
    pub fn into_account_mut(self) -> &'a mut Account {
        let Self { address, tracked, journal, snapshotted, .. } = self;
        if !snapshotted {
            journal.push(JournalEntry::AccountChange { address, previous: tracked.present.clone() });
        }
        tracked.present.get_or_insert_with(empty_account)
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

    #[test]
    fn journaled_account_mutations_journal_and_roll_back() {
        use crate::{SpecId, Version};
        use crate::bytecode::Bytecode;

        let address = Address::from([0x88; 20]);
        let mut state = State::new(CacheDB::default());

        let checkpoint = state.checkpoint();
        {
            let mut account = state.journaled_account(&address).unwrap();
            assert!(!account.exists());
            assert!(account.warm(), "first access is cold");
            assert!(!account.warm(), "second access is warm");
            account.set_balance(Word::from(100));
            account.set_nonce(7);
            assert!(account.bump_nonce());
            account.set_code_slow(Bytecode::new_raw(alloy_primitives::Bytes::from_static(&[
                0x60, 0x00,
            ])));
        }

        assert!(state.is_account_warm(&address));
        let info = state.account_info(&address).unwrap().expect("account materialized by mutation");
        assert_eq!(info.balance, Word::from(100));
        assert_eq!(info.nonce, 8);
        assert_ne!(info.code_hash, KECCAK256_EMPTY);

        state.rollback(checkpoint, Version::base(SpecId::FRONTIER).features);
        assert!(!state.is_account_warm(&address));
        assert!(state.account_info(&address).unwrap().is_none());
        assert!(state.build_state_changes().is_empty());
    }

    #[test]
    fn journaled_account_read_only_handle_journals_nothing() {
        let address = Address::from([0x89; 20]);
        let mut database = CacheDB::default();
        database.insert_account_info(&address, AccountInfo::default().with_balance(Word::from(5)));
        let mut state = State::new(database);

        let checkpoint = state.checkpoint();
        {
            let account = state.journaled_account(&address).unwrap();
            assert_eq!(account.balance(), Word::from(5));
            assert_eq!(account.nonce(), 0);
        }
        // Loading preserves the account but a read-only handle records no transition.
        state.rollback(checkpoint, crate::Version::base(crate::SpecId::FRONTIER).features);
        assert!(state.build_state_changes().is_empty());
    }
}
