//! Account models held by the state overlay and emitted in transitions.

use super::{DbResult, DynDatabase, JournalEntry, StateInner};
use crate::{EvmFeatures, bytecode::Bytecode, interpreter::Word};
use alloy_primitives::{Address, B256, KECCAK256_EMPTY, U256};
use derive_where::derive_where;

/// Account information loaded from the backing database or emitted in a state
/// transition.
///
/// Equality compares the persisted account fields, including the optional extension.
/// [`Self::code`] is a cache keyed by the code hash and may or may not be populated.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct AccountInfo {
    /// Account balance.
    pub balance: Word,
    /// Account nonce.
    pub nonce: u64,
    /// Hash of the raw bytes in `code`, or the empty code hash.
    pub code_hash: B256,
    /// Bytecode associated with this account.
    pub code: Option<Bytecode>,
    /// Raw chain-specific data committed to the account leaf by the state provider.
    ///
    /// Empty extensions are omitted by Serde. Binary formats must delimit structs
    /// (e.g. MessagePack); bincode and Postcard account encodings are unsupported.
    #[cfg(feature = "account-ext")]
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "super::AccountExtension::is_empty")
    )]
    pub extension: super::AccountExtension,
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    #[cfg_attr(feature = "serde", serde(skip))]
    pub _non_exhaustive: (),
}

/// Compares [`AccountInfo`] by `balance`, `nonce`, and `code_hash`, skipping the
/// `code` field: `code_hash` already uniquely identifies the bytecode, so
/// comparing the bytecode itself is redundant.
impl PartialEq for AccountInfo {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        let equal = self.balance == other.balance
            && self.nonce == other.nonce
            && self.code_hash == other.code_hash;
        #[cfg(feature = "account-ext")]
        let equal = equal && self.extension == other.extension;
        equal
    }
}

impl Eq for AccountInfo {}

/// Hashes the Ethereum account fields. Extension-only differences may collide,
/// but equal accounts always hash equally.
impl core::hash::Hash for AccountInfo {
    #[inline]
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.balance.hash(state);
        self.nonce.hash(state);
        self.code_hash.hash(state);
    }
}

impl Default for AccountInfo {
    #[inline]
    fn default() -> Self {
        Self {
            balance: U256::ZERO,
            nonce: 0,
            code_hash: KECCAK256_EMPTY,
            code: Some(Bytecode::default()),
            #[cfg(feature = "account-ext")]
            extension: super::AccountExtension::new(),
            _non_exhaustive: (),
        }
    }
}

impl AccountInfo {
    /// Creates an empty account with zero balance and nonce, the empty code hash,
    /// and no cached bytecode.
    #[inline]
    pub const fn empty() -> Self {
        Self::new(U256::ZERO, 0, KECCAK256_EMPTY, None)
    }

    /// Creates a new [`AccountInfo`] with the given fields and optional cached bytecode.
    ///
    /// `None` means the bytecode is not cached; `code_hash` still identifies the account's code.
    /// The caller is responsible for `code_hash` matching the bytecode when it is provided.
    #[inline]
    pub const fn new(balance: Word, nonce: u64, code_hash: B256, code: Option<Bytecode>) -> Self {
        Self {
            balance,
            nonce,
            code_hash,
            code,
            #[cfg(feature = "account-ext")]
            extension: super::AccountExtension::new(),
            _non_exhaustive: (),
        }
    }

    /// Clones this account without bytecode.
    #[inline]
    #[allow(clippy::missing_const_for_fn)] // Cloning a shared extension cannot be const.
    pub(crate) fn clone_no_code(&self) -> Self {
        Self {
            balance: self.balance,
            nonce: self.nonce,
            code_hash: self.code_hash,
            code: None,
            #[cfg(feature = "account-ext")]
            extension: self.extension.clone(),
            _non_exhaustive: (),
        }
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
        let empty = self.balance.is_zero() && self.nonce == 0 && self.code_hash == KECCAK256_EMPTY;
        #[cfg(feature = "account-ext")]
        let empty = empty && self.extension.is_empty();
        empty
    }
}

/// Current-transaction account overlay and EIP-2929/account-lifetime metadata.
///
/// An entry exists in the overlay only because it was loaded, so `original` and `present` are
/// always meaningful. `original` captures the account at the start of the transaction when it is
/// first loaded, while `present` is the live overlay after EVM mutations. Keeping both lets
/// [`State`](super::State) emit the transaction's account transition without re-reading the backing
/// database. `present` is `None` for an account that was loaded as non-existent or deleted.
///
/// `just_created` and `code_changed` track creation and code-modification state of the present
/// overlay account, driving transaction-finalization and change-emission rules. They are meaningful
/// only while `present` is `Some`.
///
/// Detached from the EVM as part of a [`PendingState`](super::PendingState), the entry describes
/// the account's transaction transition: `original` against `present`, with
/// [`Self::is_created`] flagging in-transaction creation.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Account {
    /// Account info at the start of the transaction. `None` means the account did not exist.
    pub(crate) original: Option<AccountInfo>,
    /// Present account overlay after mutations. `None` means the account is absent/deleted.
    pub(crate) present: Option<AccountInfo>,
    /// Whether this account is warm in the current transaction.
    pub(crate) is_warm: bool,
    /// Whether this account is touched for transaction-finalization account-lifetime rules.
    pub(crate) is_touched: bool,
    /// Whether this account has been self-destructed in the current transaction.
    pub(crate) is_destroyed: bool,
    /// Whether the present overlay account was created in the current transaction.
    pub(crate) just_created: bool,
    /// Whether the present overlay account's code has been modified.
    pub(crate) code_changed: bool,
}

impl Account {
    /// Creates an account overlay entry from its transaction-boundary original info and its
    /// present info.
    #[cfg(test)]
    pub(crate) fn new(original: Option<AccountInfo>, present: Option<AccountInfo>) -> Self {
        Self { original, present, ..Self::default() }
    }

    /// Marks the account as created during the transaction, which also flags its code as changed.
    #[inline]
    pub(crate) const fn mark_created(&mut self) {
        self.just_created = true;
        self.code_changed = true;
    }

    /// Returns whether the account was created during the transaction.
    ///
    /// Creation is preserved across selfdestruct finalization, so it also covers accounts that
    /// were created and then destroyed in the same transaction.
    #[inline]
    pub(crate) const fn is_created(&self) -> bool {
        self.just_created
    }

    /// Returns whether the account's info changed during the transaction.
    ///
    /// This compares persisted account fields and existence; it does not consider storage.
    #[inline]
    pub(crate) fn is_changed(&self) -> bool {
        self.original != self.present
    }

    /// Returns the account's new bytecode keyed by code hash when its code changed during the
    /// transaction to non-empty code.
    #[inline]
    pub(crate) fn changed_code(&self) -> Option<(B256, &Bytecode)> {
        let account = self.present.as_ref()?;
        let code = account.code.as_ref()?;
        let code_hash = account.code_hash;
        ((self.code_changed
            || self.original.as_ref().map(|original| original.code_hash) != Some(code_hash))
            && !code.is_empty()
            && !code_hash.is_zero()
            && code_hash != KECCAK256_EMPTY)
            .then_some((code_hash, code))
    }
}

/// A mutable, journaled handle to an account loaded into the transaction overlay.
///
/// Returned by [`State::account`](super::State::account). The account has
/// already been read from the backing database and preserved in the transaction overlay; this
/// handle ties that overlay slot to the revert journal so a mutation and its rollback bookkeeping
/// cannot drift apart, mirroring revm's `AccountHandle`.
///
/// Mutations record account structure and flags separately from balance and nonce operations.
/// Relative operations remain relative during rollback, while setters restore absolute values. A
/// handle used only for reads records nothing.
///
/// The handle also carries the shared [`StateInner`] (backing database, revert journal, and
/// transaction-initial base warm set), so it can journal mutations, load code on demand, and answer
/// warm-access queries without going back through [`State`](super::State), mirroring the database
/// and access-list references revm's `AccountHandle` holds.
#[derive_where(Debug)]
pub struct AccountHandle<'a, 'db> {
    /// Address of the account.
    address: Address,
    /// Transaction overlay entry: account overlay plus warm/touched access metadata.
    tracked: &'a mut Account,
    /// Shared inner state: backing database, revert journal, and base warm set.
    #[derive_where(skip)]
    inner: &'a mut StateInner<'db>,
    /// Whether this handle has recorded its pre-mutation account structure and flags.
    structure_recorded: bool,
}

/// Returns a freshly materialized empty account overlay.
#[inline]
fn empty_account() -> AccountInfo {
    AccountInfo::default()
}

impl<'a, 'db> AccountHandle<'a, 'db> {
    /// Creates a handle over a loaded account overlay slot and the shared inner state (backing
    /// database, revert journal, and transaction-initial base warm set).
    #[inline]
    pub(crate) const fn new(
        address: Address,
        tracked: &'a mut Account,
        inner: &'a mut StateInner<'db>,
    ) -> Self {
        Self { address, tracked, inner, structure_recorded: false }
    }

    /// Records account structure and flags before this handle's first mutation.
    #[inline]
    fn record_change(&mut self) {
        if self.structure_recorded {
            return;
        }
        self.inner.journal.push(JournalEntry::AccountChange {
            address: self.address,
            previous: self.tracked.present.clone(),
            previous_is_warm: self.tracked.is_warm,
            previous_is_touched: self.tracked.is_touched,
            previous_is_destroyed: self.tracked.is_destroyed,
            previous_just_created: self.tracked.just_created,
            previous_code_changed: self.tracked.code_changed,
        });
        self.structure_recorded = true;
    }

    /// Records account structure when a field mutation also materializes or touches the account.
    #[inline]
    fn prepare_field_change(&mut self) {
        if self.tracked.present.is_none() || !self.tracked.is_touched {
            self.record_change();
        }
        self.tracked.is_touched = true;
    }

    /// Returns the account address.
    #[inline]
    pub const fn address(&self) -> Address {
        self.address
    }

    /// Returns the present account overlay, or `None` when the account is absent.
    #[inline]
    pub const fn get(&self) -> Option<&AccountInfo> {
        self.tracked.present.as_ref()
    }

    /// Returns whether the account currently exists in the overlay.
    #[inline]
    pub const fn exists(&self) -> bool {
        self.tracked.present.is_some()
    }

    /// Returns whether the account existed at the transaction boundary.
    #[inline]
    pub const fn originally_exists(&self) -> bool {
        self.tracked.original.is_some()
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
    pub const fn is_created(&self) -> bool {
        self.tracked.present.is_some() && self.tracked.just_created
    }

    /// Returns whether the account is dead by the EIP-161 definition while existing in the overlay.
    ///
    /// An account is existing-dead when it has zero nonce, zero balance, and empty code, or when it
    /// was deleted in this transaction but existed at the transaction boundary. Spurious Dragon
    /// deletes such touched accounts during transaction finalization.
    #[inline]
    pub fn is_existing_dead(&self) -> bool {
        self.tracked.present.as_ref().is_some_and(AccountInfo::is_empty)
            || (self.tracked.present.is_none() && self.tracked.original.is_some())
    }

    /// Returns whether the account is empty for EIP-150 new-account gas checks.
    ///
    /// Under EIP-161 an absent or empty account is empty; before EIP-161 only an absent, untouched
    /// account counts as empty.
    #[inline]
    pub fn is_empty_for_new_account_gas(&self, features: EvmFeatures) -> bool {
        if features.contains(EvmFeatures::EIP161) {
            return self.tracked.present.as_ref().is_none_or(AccountInfo::is_empty);
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
        if let Some(code) = account.code.as_ref()
            && !code.is_empty()
        {
            return Ok(code.clone());
        }
        let code_hash = account.code_hash;
        self.inner.database.get_code_by_hash(&code_hash)
    }

    /// Loads the account's bytecode as it stood at the start of the transaction.
    ///
    /// Used by EIP-7702 to tell whether an authority was already delegated before this transaction
    /// (distinct from [`Self::load_code`], which reflects delegations applied by earlier
    /// authorizations in the same transaction). Returns empty bytecode when the account was absent
    /// or had empty code at the transaction boundary.
    #[inline]
    pub fn original_code(&mut self) -> DbResult<Bytecode> {
        let Some(account) = self.tracked.original.as_ref() else {
            return Ok(Bytecode::default());
        };
        if account.code_hash == KECCAK256_EMPTY {
            return Ok(Bytecode::default());
        }
        if let Some(code) = account.code.as_ref()
            && !code.is_empty()
        {
            return Ok(code.clone());
        }
        let code_hash = account.code_hash;
        self.inner.database.get_code_by_hash(&code_hash)
    }

    /// Touches the account, recording a revert snapshot the first time it is mutated.
    ///
    /// Touched accounts participate in EIP-158/161 empty-account cleanup at transaction
    /// finalization even when no field changes.
    #[inline]
    pub fn touch(&mut self) {
        if !self.tracked.is_touched {
            self.record_change();
            self.tracked.is_touched = true;
        }
    }

    /// Marks the account self-destructed in the current transaction, recording a revert snapshot
    /// the first time and touching the account.
    ///
    /// Touching makes the account participate in EIP-158/161 cleanup, and the self-destruct set
    /// membership is undone by [`State::rollback`](super::State::rollback).
    #[inline]
    pub fn mark_destructed(&mut self) {
        if !self.tracked.is_destroyed {
            self.record_change();
            self.tracked.is_destroyed = true;
            self.inner.selfdestructs.insert(self.address);
            self.tracked.is_touched = true;
        } else if !self.tracked.is_touched {
            self.touch();
        }
    }

    /// Marks the account warm for EIP-2929 gas accounting, recording a revert snapshot when this
    /// access transitions it from cold to warm.
    ///
    /// Returns `true` if the account was cold before this call. Accounts already warm through the
    /// base warm set stay warm across rollback, so warming them again records nothing.
    #[inline]
    pub fn warm(&mut self) -> bool {
        if self.tracked.is_warm || self.inner.prewarm_set.is_warm(&self.address) {
            return false;
        }
        self.record_change();
        self.tracked.is_warm = true;
        true
    }

    /// Sets the account balance, touching the account and recording a revert snapshot.
    #[inline]
    pub fn set_balance(&mut self, balance: Word) {
        self.prepare_field_change();
        self.inner
            .journal
            .push(JournalEntry::BalanceChange { address: self.address, previous: self.balance() });
        self.tracked.present.get_or_insert_with(empty_account).balance = balance;
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
        self.prepare_field_change();
        self.inner.journal.push(JournalEntry::BalanceDelta { address: self.address, delta });
        self.tracked.present.get_or_insert_with(empty_account).balance = balance;
    }

    /// Sets the account nonce, touching the account and recording a revert snapshot.
    #[inline]
    pub fn set_nonce(&mut self, nonce: u64) {
        self.prepare_field_change();
        self.inner
            .journal
            .push(JournalEntry::NonceChange { address: self.address, previous: self.nonce() });
        self.tracked.present.get_or_insert_with(empty_account).nonce = nonce;
    }

    /// Bumps the account nonce by one, touching the account and recording a revert snapshot.
    ///
    /// Returns `false` without changing the nonce when it is already at the maximum value.
    #[inline]
    pub fn bump_nonce(&mut self) -> bool {
        let Some(nonce) = self.nonce().checked_add(1) else {
            self.touch();
            return false;
        };
        self.prepare_field_change();
        self.inner.journal.push(JournalEntry::NonceBump { address: self.address });
        self.tracked.present.get_or_insert_with(empty_account).nonce = nonce;
        true
    }

    /// Sets the account code and its hash, touching the account and recording a revert snapshot.
    ///
    /// The caller is responsible for `code_hash` matching `code`; use [`Self::set_code_slow`] to
    /// have the hash computed.
    #[inline]
    pub fn set_code(&mut self, code_hash: B256, code: Bytecode) {
        self.record_change();
        self.tracked.is_touched = true;
        let account = self.tracked.present.get_or_insert_with(empty_account);
        account.code_hash = code_hash;
        account.code = Some(code);
        self.tracked.code_changed = true;
    }

    /// Sets the account code, computing its hash, touching the account and recording a revert
    /// snapshot.
    #[inline]
    pub fn set_code_slow(&mut self, code: Bytecode) {
        let code_hash = code.hash_slow();
        self.set_code(code_hash, code);
    }

    /// Applies an [EIP-7702](https://eips.ethereum.org/EIPS/eip-7702) delegation to the account.
    ///
    /// Installs an EIP-7702 delegation designator pointing at `delegated_address`, or clears the
    /// account code when `delegated_address` is zero, then bumps the account nonce.
    #[inline]
    pub fn set_delegation(&mut self, delegated_address: Address) {
        let code = if delegated_address.is_zero() {
            Bytecode::default()
        } else {
            Bytecode::new_eip7702(delegated_address)
        };
        self.set_code_slow(code);
        self.bump_nonce();
    }

    /// Records a revert snapshot and returns the live account, materializing an empty one when it
    /// is currently absent.
    #[inline]
    fn present_mut(&mut self) -> &mut AccountInfo {
        self.record_change();
        self.inner
            .journal
            .push(JournalEntry::BalanceChange { address: self.address, previous: self.balance() });
        self.inner
            .journal
            .push(JournalEntry::NonceChange { address: self.address, previous: self.nonce() });
        self.tracked.present.get_or_insert_with(empty_account)
    }

    /// Marks the present overlay account as created in the current transaction, also flagging its
    /// code as changed. The creation/code-change status is reverted together with the rest of the
    /// overlay by the [`JournalEntry::AccountChange`] snapshot recorded for this handle.
    #[inline]
    pub(crate) fn mark_created(&mut self) {
        self.record_change();
        self.tracked.mark_created();
    }

    /// Records a revert snapshot and returns the live account, materializing an empty one when it
    /// is currently absent.
    #[inline]
    pub fn get_or_insert(&mut self) -> &mut AccountInfo {
        self.present_mut()
    }

    /// Updates the account payload, touching the account and recording a revert snapshot.
    #[cfg(feature = "account-ext")]
    pub fn set_extension(&mut self, extension: super::AccountExtension) {
        self.record_change();
        self.tracked.is_touched = true;
        self.tracked.present.get_or_insert_with(empty_account).extension = extension;
    }

    /// Overrides the balance without journaling or touching the account.
    pub fn override_balance(&mut self, balance: Word) {
        if let Some(account) = self.tracked.present.as_mut() {
            account.balance = balance;
        } else if !balance.is_zero() {
            self.tracked.present = Some(AccountInfo::default().with_balance(balance));
        }
    }

    /// Overrides the nonce without journaling or touching the account.
    /// Earlier nonce bumps still revert by decrementing the overridden value.
    pub fn override_nonce(&mut self, nonce: u64) {
        if let Some(account) = self.tracked.present.as_mut() {
            account.nonce = nonce;
        } else if nonce != 0 {
            self.tracked.present = Some(AccountInfo::default().with_nonce(nonce));
        }
    }

    /// Dematerializes an empty account without journaling or changing its access metadata.
    ///
    /// This only checks the account payload. The caller is responsible for deciding that account
    /// absence, including any creation and storage lifetime, should be restored.
    pub fn dematerialize_if_empty(&mut self) {
        if self.tracked.present.as_ref().is_some_and(AccountInfo::is_empty) {
            self.tracked.present = None;
        }
    }

    /// Deletes the account at transaction finalization.
    ///
    /// Used for self-destructed accounts (pre-EIP-8246, and zero-balance accounts under EIP-8246)
    /// and EIP-161 dead-account cleanup. The caller is responsible for wiping the account's
    /// storage. Finalization runs after the last revertible scope, so the mutation is not
    /// journaled: the entry would never be replayed before [`State`](super::State) clears it.
    #[inline]
    pub(crate) fn delete_for_finalization(&mut self) {
        self.tracked.present = None;
    }

    /// Resets a self-destructed account to a balance-only account for EIP-8246 finalization.
    ///
    /// The balance is preserved while the nonce is reset to 0 and the code is cleared. The caller
    /// is responsible for wiping the account's storage. This is only called for accounts that
    /// still hold a balance; zero-balance self-destructed accounts are removed via
    /// [`Self::delete_for_finalization`] instead. Like [`Self::delete_for_finalization`], the
    /// mutation is not journaled because finalization runs after the last revertible scope.
    #[inline]
    pub(crate) fn reset_selfdestructed_for_finalization(&mut self) {
        let balance = self.balance();
        self.tracked.is_destroyed = false;
        self.tracked.present = Some(AccountInfo::default().with_balance(balance));
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
        use crate::{SpecId, Version, bytecode::Bytecode};

        let address = Address::from([0x88; 20]);
        let mut state = State::new(CacheDB::default());

        let checkpoint = state.checkpoint();
        {
            let mut account = state.account(&address).unwrap();
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

        assert!(state.account(&address).unwrap().is_warm());
        let info = state
            .account_info_untracked(&address)
            .unwrap()
            .expect("account materialized by mutation");
        assert_eq!(info.balance, Word::from(100));
        assert_eq!(info.nonce, 8);
        assert_ne!(info.code_hash, KECCAK256_EMPTY);

        state.rollback(checkpoint, Version::base(SpecId::FRONTIER).features);
        assert!(!state.account(&address).unwrap().is_warm());
        assert!(state.account_info_untracked(&address).unwrap().is_none());
        assert!(!state.take_pending_state().is_changed());
    }

    #[test]
    fn account_info_set_code_is_published_on_commit() {
        use alloy_primitives::Bytes;

        let address = Address::from([0x8c; 20]);
        let code = Bytecode::new_raw(Bytes::from_static(&[0x60, 0x01]));
        let mut state = State::new(CacheDB::default());

        state.account(&address).unwrap().get_or_insert().set_code(code.clone());
        state.commit_transaction();

        let loaded = state.account(&address).unwrap().load_code().unwrap();
        assert_eq!(loaded, code);
    }

    #[test]
    fn journaled_existing_account_field_changes_roll_back_granularly() {
        use crate::{SpecId, Version, bytecode::Bytecode};
        use alloy_primitives::Bytes;

        let address = Address::from([0x8b; 20]);
        let original_code = Bytecode::new_raw(Bytes::from_static(&[0x60, 0x01]));
        let original_code_hash = original_code.hash_slow();
        let mut database = CacheDB::default();
        database.insert_account_info(
            &address,
            AccountInfo::default()
                .with_balance(Word::from(10))
                .with_nonce(2)
                .with_code(original_code),
        );
        let mut state = State::new(database);

        let checkpoint = state.checkpoint();
        {
            let mut account = state.account(&address).unwrap();
            account.set_balance(Word::from(999));
            account.set_nonce(5);
            assert!(account.bump_nonce());
            account.set_code_slow(Bytecode::new_raw(Bytes::from_static(&[0x60, 0x02])));
        }

        let info = state.account_info_untracked(&address).unwrap().expect("account exists");
        assert_eq!(info.balance, Word::from(999));
        assert_eq!(info.nonce, 6);
        assert_ne!(info.code_hash, original_code_hash);

        // Each field reverts independently from its own granular journal entry, leaving the loaded
        // account in place with its original values and emitting no state change.
        state.rollback(checkpoint, Version::base(SpecId::FRONTIER).features);
        let info = state.account_info_untracked(&address).unwrap().expect("account still loaded");
        assert_eq!(info.balance, Word::from(10));
        assert_eq!(info.nonce, 2);
        assert_eq!(info.code_hash, original_code_hash);
        assert!(!state.take_pending_state().is_changed());
    }

    #[test]
    fn journaled_account_read_only_handle_journals_nothing() {
        let address = Address::from([0x89; 20]);
        let mut database = CacheDB::default();
        database.insert_account_info(&address, AccountInfo::default().with_balance(Word::from(5)));
        let mut state = State::new(database);

        let checkpoint = state.checkpoint();
        {
            let account = state.account(&address).unwrap();
            assert_eq!(account.balance(), Word::from(5));
            assert_eq!(account.nonce(), 0);
        }
        // Loading preserves the account but a read-only handle records no transition.
        state.rollback(checkpoint, crate::Version::base(crate::SpecId::FRONTIER).features);
        assert!(!state.take_pending_state().is_changed());
    }

    #[test]
    fn journaled_account_skip_cold_load_signals_skip() {
        use crate::LoadError;

        let address = Address::from([0x8a; 20]);
        let mut database = CacheDB::default();
        database.insert_account_info(&address, AccountInfo::default().with_balance(Word::from(5)));
        let mut state = State::new(database);

        // A cold, not-yet-loaded account signals the skip instead of reading the database.
        assert!(matches!(state.account_with_skip(&address, true), Err(LoadError::ColdLoadSkipped)));
        // Skipping leaves the overlay untouched, so a later non-skipped load still works.
        assert_eq!(state.account(&address).unwrap().balance(), Word::from(5));
        // Residency alone does not make a cold access affordable: a loaded-but-cold account still
        // signals the skip, since warmth — not overlay residency — decides the cold surcharge.
        assert!(matches!(state.account_with_skip(&address, true), Err(LoadError::ColdLoadSkipped)));
        // Once warmed, the affordable warm access yields a handle even when skipping is requested.
        state.account(&address).unwrap().warm();
        assert!(state.account_with_skip(&address, true).is_ok());
    }

    #[test]
    fn overrides_preserve_relative_undo() {
        let address = Address::with_last_byte(0x81);
        let sender = Address::with_last_byte(0x82);
        let mut db = CacheDB::default();
        db.insert_account_info(
            &address,
            AccountInfo::default().with_balance(Word::from(100)).with_nonce(7),
        );
        db.insert_account_info(&sender, AccountInfo::default().with_balance(Word::from(100)));
        let mut state = State::new(db);
        let checkpoint = state.checkpoint();

        assert!(state.transfer(&sender, &address, &Word::from(8)).unwrap());
        assert!(state.account(&address).unwrap().bump_nonce());
        state.account(&address).unwrap().override_balance(Word::from(109));
        state.account(&address).unwrap().override_nonce(9);

        state.rollback(checkpoint, crate::Version::base(crate::SpecId::CANCUN).features);
        {
            let account = state.account(&address).unwrap();
            assert_eq!(account.balance(), Word::from(101));
            assert_eq!(account.nonce(), 8);
        }
        assert_eq!(state.account(&sender).unwrap().balance(), Word::from(100));
    }

    #[test]
    fn setters_remain_absolute_across_overrides() {
        let address = Address::with_last_byte(0x83);
        let mut db = CacheDB::default();
        db.insert_account_info(
            &address,
            AccountInfo::default().with_balance(Word::from(100)).with_nonce(7),
        );
        let mut state = State::new(db);
        let checkpoint = state.checkpoint();
        {
            let mut account = state.account(&address).unwrap();
            account.set_balance(Word::from(100));
            account.set_nonce(8);
            account.override_balance(Word::from(200));
            account.override_nonce(9);
        }

        state.rollback(checkpoint, crate::Version::base(crate::SpecId::CANCUN).features);
        let account = state.account(&address).unwrap();
        assert_eq!(account.balance(), Word::from(100));
        assert_eq!(account.nonce(), 7);
    }

    #[test]
    fn creation_endowment_remains_relative_across_overrides() {
        let address = Address::with_last_byte(0x8a);
        let sender = Address::with_last_byte(0x8b);
        let mut db = CacheDB::default();
        db.insert_account_info(&sender, AccountInfo::default().with_balance(Word::from(100)));
        let mut state = State::new(db);
        let features = crate::Version::base(crate::SpecId::CANCUN).features;
        let checkpoint = state.checkpoint();

        state.create_account(&sender, &address, &Word::from(3), features).unwrap().unwrap();
        state.account(&address).unwrap().override_balance(Word::from(109));
        state.account(&address).unwrap().override_nonce(9);
        state.rollback(checkpoint, features);

        let account = state.account(&address).unwrap();
        assert_eq!(account.balance(), Word::from(106));
        assert_eq!(account.nonce(), 0);
        assert!(!account.is_created());
    }

    #[test]
    fn caller_owned_absence_cleanup_handles_rolled_back_materialization() {
        let address = Address::with_last_byte(0x84);
        let sender = Address::with_last_byte(0x85);
        let features = crate::Version::base(crate::SpecId::CANCUN).features;
        for (journaled, overridden) in
            [(Word::ZERO, Word::from(9)), (Word::from(10), Word::from(20))]
        {
            let mut db = CacheDB::default();
            db.insert_account_info(&sender, AccountInfo::default().with_balance(Word::from(100)));
            let mut state = State::new(db);
            let checkpoint = state.checkpoint();
            if journaled.is_zero() {
                assert!(state.account(&address).unwrap().bump_nonce());
            } else {
                assert!(state.transfer(&sender, &address, &journaled).unwrap());
            }
            let (previous, originally_absent) = {
                let mut account = state.account(&address).unwrap();
                let previous = account.balance();
                let originally_absent = !account.originally_exists();
                account.override_balance(overridden);
                (previous, originally_absent)
            };

            state.rollback(checkpoint, features);
            let mut account = state.account(&address).unwrap();
            let restored = account.balance().wrapping_add(previous).wrapping_sub(overridden);
            account.override_balance(restored);
            if originally_absent {
                account.dematerialize_if_empty();
            }
            assert!(!account.exists(), "journaled={journaled}, overridden={overridden}");
        }
    }

    #[test]
    fn caller_controls_empty_account_dematerialization() {
        let historical = Address::with_last_byte(0x8c);
        let surviving = Address::with_last_byte(0x8d);
        let mut state = State::new(CacheDB::default());
        {
            let mut account = state.account(&historical).unwrap();
            account.tracked.mark_created();
            account.tracked.present = None;
            account.override_balance(Word::from(9));
            account.override_balance(Word::ZERO);
            account.dematerialize_if_empty();
            assert!(!account.exists());
        }
        {
            let mut account = state.account(&surviving).unwrap();
            account.override_balance(Word::from(9));
            account.mark_created();
            account.override_balance(Word::ZERO);
            assert!(account.exists());
            assert!(account.is_created());
            assert!(account.get().unwrap().is_empty());
        }
    }

    #[test]
    fn zero_nonce_override_saturates_bump_undo() {
        let address = Address::with_last_byte(0x87);
        let mut state = State::new(CacheDB::default());
        let checkpoint = state.checkpoint();
        {
            let mut account = state.account(&address).unwrap();
            assert!(account.bump_nonce());
            assert!(account.bump_nonce());
            account.override_nonce(0);
        }

        state.rollback(checkpoint, crate::Version::base(crate::SpecId::CANCUN).features);
        let account = state.account(&address).unwrap();
        assert_eq!(account.nonce(), 0);
        assert!(!account.exists());
    }

    #[test]
    fn override_between_nested_rollbacks_preserves_parent_undo() {
        let address = Address::with_last_byte(0x88);
        let sender = Address::with_last_byte(0x89);
        let mut db = CacheDB::default();
        db.insert_account_info(
            &address,
            AccountInfo::default().with_balance(Word::from(100)).with_nonce(7),
        );
        db.insert_account_info(&sender, AccountInfo::default().with_balance(Word::from(100)));
        let mut state = State::new(db);
        let features = crate::Version::base(crate::SpecId::CANCUN).features;
        let parent = state.checkpoint();
        assert!(state.transfer(&sender, &address, &Word::from(3)).unwrap());
        assert!(state.account(&address).unwrap().bump_nonce());
        let child = state.checkpoint();
        assert!(state.transfer(&sender, &address, &Word::from(5)).unwrap());
        assert!(state.account(&address).unwrap().bump_nonce());
        state.rollback(child, features);

        {
            let mut account = state.account(&address).unwrap();
            assert_eq!((account.balance(), account.nonce()), (Word::from(103), 8));
            account.override_balance(Word::from(109));
            account.override_nonce(9);
        }
        state.rollback(parent, features);
        let account = state.account(&address).unwrap();
        assert_eq!((account.balance(), account.nonce()), (Word::from(106), 8));
    }
}
