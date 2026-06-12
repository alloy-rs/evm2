//! Account models held by the state overlay and emitted in transitions.

use super::{DbResult, DynDatabase, JournalEntry, StateInner};
use crate::{EvmFeatures, bytecode::Bytecode, interpreter::Word};
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
/// Returned by [`State::account_entry`](super::State::account_entry). The account has
/// already been read from the backing database and preserved in the transaction overlay; this
/// handle ties that overlay slot to the revert journal so a mutation and its rollback bookkeeping
/// cannot drift apart, mirroring revm's `AccountEntry`.
///
/// The first mutating access records a single [`JournalEntry::AccountChange`] snapshot of the
/// account as it was when the handle was created, so every change made through the handle is
/// reverted together by [`State::rollback`](super::State::rollback). A handle used only for reads
/// records nothing. Mutating a currently-absent account materializes an empty one.
///
/// The handle also carries the shared [`StateInner`] (backing database, revert journal, and
/// transaction-initial base warm set), so it can journal mutations, load code on demand, and answer
/// warm-access queries without going back through [`State`](super::State), mirroring the database
/// and access-list references revm's `AccountEntry` holds.
#[derive_where(Debug)]
pub struct AccountEntry<'a> {
    /// Address of the account.
    address: Address,
    /// Transaction overlay entry: account overlay plus warm/touched access metadata.
    tracked: &'a mut TrackedAccount,
    /// Shared inner state: backing database, revert journal, and base warm set.
    #[derive_where(skip)]
    inner: &'a mut StateInner,
    /// Whether the revert snapshot has already been recorded for this handle.
    snapshotted: bool,
}

/// Returns a freshly materialized empty account.
#[inline]
fn empty_account() -> Account {
    Account { code_hash: KECCAK256_EMPTY, ..Account::default() }
}

impl<'a> AccountEntry<'a> {
    /// Creates a handle over a loaded account overlay slot and the shared inner state (backing
    /// database, revert journal, and transaction-initial base warm set).
    #[inline]
    pub(super) const fn new(
        address: Address,
        tracked: &'a mut TrackedAccount,
        inner: &'a mut StateInner,
    ) -> Self {
        Self { address, tracked, inner, snapshotted: false }
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
        self.tracked.is_warm || self.inner.prewarm_set.is_warm(&self.address)
    }

    /// Returns whether the account has been marked self-destructed in the current transaction.
    #[inline]
    pub fn is_destructed(&self) -> bool {
        self.inner.selfdestructs.contains(&self.address)
    }

    /// Returns whether the account is touched for transaction-finalization account-lifetime rules.
    #[inline]
    pub const fn is_touched(&self) -> bool {
        self.tracked.is_touched
    }

    /// Returns whether the account was created in the current transaction.
    #[inline]
    pub fn is_created(&self) -> bool {
        self.tracked.present.as_ref().is_some_and(|account| account.just_created)
    }

    /// Returns whether the account is dead by the EIP-161 definition while existing in the overlay.
    ///
    /// An account is existing-dead when it has zero nonce, zero balance, and empty code, or when it
    /// was deleted in this transaction but existed at the transaction boundary. Spurious Dragon
    /// deletes such touched accounts during transaction finalization.
    #[inline]
    pub fn is_existing_dead(&self) -> bool {
        self.tracked.present.as_ref().is_some_and(Account::is_empty)
            || (self.tracked.present.is_none() && self.tracked.original.is_some())
    }

    /// Returns whether the account is empty for EIP-150 new-account gas checks.
    ///
    /// Under EIP-161 an absent or empty account is empty; before EIP-161 only an absent, untouched
    /// account counts as empty.
    #[inline]
    pub fn is_empty_for_new_account_gas(&self, features: EvmFeatures) -> bool {
        if features.contains(EvmFeatures::EIP161) {
            return self.tracked.present.as_ref().is_none_or(Account::is_empty);
        }
        self.tracked.present.is_none() && !self.tracked.is_touched
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
        self.inner.database.get_code_by_hash(&code_hash)
    }

    /// Touches the account, recording a [`JournalEntry::Touch`] the first time it is touched.
    ///
    /// Touched accounts participate in EIP-158/161 empty-account cleanup at transaction
    /// finalization even when no field changes.
    #[inline]
    pub fn touch(&mut self) {
        if !self.tracked.is_touched {
            self.tracked.is_touched = true;
            self.inner.journal.push(JournalEntry::Touch { address: self.address });
        }
    }

    /// Marks the account self-destructed in the current transaction, recording a
    /// [`JournalEntry::SelfDestruct`] the first time and touching the account.
    ///
    /// Touching makes the account participate in EIP-158/161 cleanup, and the self-destruct set
    /// membership is undone by [`State::rollback`](super::State::rollback).
    #[inline]
    pub fn mark_destructed(&mut self) {
        if self.inner.selfdestructs.insert(self.address) {
            self.inner.journal.push(JournalEntry::SelfDestruct { address: self.address });
        }
        self.touch();
    }

    /// Marks the account warm for EIP-2929 gas accounting, recording a
    /// [`JournalEntry::AccountWarmed`] when this access transitions it from cold to warm.
    ///
    /// Returns `true` if the account was cold before this call. Accounts already warm through the
    /// base warm set stay warm across rollback, so warming them again records nothing.
    #[inline]
    pub fn warm(&mut self) -> bool {
        if self.tracked.is_warm || self.inner.prewarm_set.is_warm(&self.address) {
            return false;
        }
        self.tracked.is_warm = true;
        self.inner.journal.push(JournalEntry::AccountWarmed { address: self.address });
        true
    }

    /// Sets the account balance, touching the account and recording a revert snapshot.
    #[inline]
    pub fn set_balance(&mut self, balance: Word) {
        self.touch();
        self.get_or_insert().balance = balance;
    }

    /// Adds a signed balance delta by wrapping two's-complement values, touching the account.
    ///
    /// A zero delta only touches the account, matching the EVM's value-bearing-call semantics.
    #[inline]
    pub fn add_balance(&mut self, delta: Word) {
        if delta.is_zero() {
            self.touch();
            return;
        }
        let balance = self.balance().wrapping_add(delta);
        self.set_balance(balance);
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
            self.inner.journal.push(JournalEntry::AccountChange {
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
        let Self { address, tracked, inner, snapshotted } = self;
        if !snapshotted {
            inner
                .journal
                .push(JournalEntry::AccountChange { address, previous: tracked.present.clone() });
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
        use crate::bytecode::Bytecode;
        use crate::{SpecId, Version};

        let address = Address::from([0x88; 20]);
        let mut state = State::new(CacheDB::default());

        let checkpoint = state.checkpoint();
        {
            let mut account = state.account_entry(&address, false).unwrap();
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

        assert!(state.account_entry(&address, false).unwrap().is_warm());
        let info =
            state.peek_account_info(&address).unwrap().expect("account materialized by mutation");
        assert_eq!(info.balance, Word::from(100));
        assert_eq!(info.nonce, 8);
        assert_ne!(info.code_hash, KECCAK256_EMPTY);

        state.rollback(checkpoint, Version::base(SpecId::FRONTIER).features);
        assert!(!state.account_entry(&address, false).unwrap().is_warm());
        assert!(state.peek_account_info(&address).unwrap().is_none());
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
            let account = state.account_entry(&address, false).unwrap();
            assert_eq!(account.balance(), Word::from(5));
            assert_eq!(account.nonce(), 0);
        }
        // Loading preserves the account but a read-only handle records no transition.
        state.rollback(checkpoint, crate::Version::base(crate::SpecId::FRONTIER).features);
        assert!(state.build_state_changes().is_empty());
    }

    #[test]
    fn journaled_account_skip_cold_load_signals_skip() {
        use crate::evm::DbErrorCode;

        let address = Address::from([0x8a; 20]);
        let mut database = CacheDB::default();
        database.insert_account_info(&address, AccountInfo::default().with_balance(Word::from(5)));
        let mut state = State::new(database);

        // A cold, not-yet-loaded account signals the skip instead of reading the database.
        assert!(matches!(state.account_entry(&address, true), Err(DbErrorCode::COLD_LOAD_SKIPPED)));
        // Skipping leaves the overlay untouched, so a later non-skipped load still works.
        let account = state.account_entry(&address, false).unwrap();
        assert_eq!(account.balance(), Word::from(5));
        // An already-loaded account yields a handle even when skipping is requested.
        assert!(state.account_entry(&address, true).is_ok());
    }
}
