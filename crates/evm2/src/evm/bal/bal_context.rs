//! EIP-7928 Block Access List state carried alongside the accepted-overlay database.

use super::{AccountBal, Bal, BalError, BlockAccessIndex};
use crate::{
    AnyError, ErrorCode,
    evm::state::{AccountInfo, StateChanges},
    interpreter::Word,
};
use alloc::sync::Arc;
use alloy_eip7928::BlockAccessList;
use alloy_primitives::Address;

/// Result of an EIP-7928 BAL lookup during a read.
type BalResult<T> = Result<T, BalError>;

/// EIP-7928 Block Access List state: an attached BAL consulted on reads plus an optional builder
/// that accumulates one from executed transactions, both keyed at a shared block access index.
///
/// This bundles the read and write BAL machinery so the database wrapper that carries it (evm2's
/// [`CacheDB`](crate::evm::CacheDB)) is not itself BAL-oriented; the state lives on the database
/// wrapper rather than the journaled state.
///
/// The two roles are independent:
///
/// - **Reads** ([`Self::bal`]): when an attached BAL is present, [`Self::get_bal_account`] /
///   [`Self::populate_bal_account`] and [`Self::bal_storage`] serve account info and storage from
///   it at [`Self::bal_index`] (post-state per transaction). A read not covered by the BAL is
///   either an error or falls through to the database, depending on [`Self::allow_db_fallback`].
/// - **Writes** ([`Self::bal_builder`]): when enabled, [`Self::commit_bal`] folds each committed
///   transaction's post-state into the builder at [`Self::bal_index`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BalContext {
    /// Optional attached EIP-7928 BAL consulted on reads.
    ///
    /// `None` (the default) disables BAL-served reads, so reads go straight to the cache/database.
    /// When `Some`, reads resolve account info and storage from the BAL at [`Self::bal_index`].
    /// Shared via [`Arc`] so the same BAL can back multiple executions.
    bal: Option<Arc<Bal>>,
    /// Optional EIP-7928 Block Access List builder.
    ///
    /// `None` (the default) disables BAL construction so normal execution pays nothing. When
    /// `Some`, [`Self::commit_bal`] folds each committed transaction's post-state into it.
    bal_builder: Option<Bal>,
    /// Current EIP-7928 block access index used by both BAL-served reads and [`Self::commit_bal`].
    ///
    /// Callers bump this once per transaction (see [`Self::bump_bal_index`]) so each transaction's
    /// writes are recorded under, and reads served at, a distinct index.
    bal_index: BlockAccessIndex,
    /// Whether reads not covered by the attached [`Self::bal`] fall back to the cache/database
    /// instead of returning [`ErrorCode::BAL_NOT_COVERED`].
    ///
    /// During block validation an access outside the BAL means the BAL is invalid, so this
    /// defaults to `false`. Enabling it allows executing transactions that are not part of the
    /// block (e.g. RPC calls) on top of BAL-positioned state.
    allow_db_fallback: bool,
    /// Last BAL lookup error, surfaced through [`Self::take_error`] after a read returns
    /// [`ErrorCode::BAL_NOT_COVERED`].
    bal_error: Option<BalError>,
}

impl BalContext {
    /// Creates an empty context with no attached BAL and no builder.
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    /// Attaches an EIP-7928 BAL consulted on reads, and returns `self`.
    #[inline]
    pub fn with_bal(mut self, bal: Arc<Bal>) -> Self {
        self.bal = Some(bal);
        self
    }

    /// Attaches an EIP-7928 BAL consulted on reads.
    #[inline]
    pub fn set_bal(&mut self, bal: Arc<Bal>) {
        self.bal = Some(bal);
    }

    /// Returns the attached read BAL, or `None` when no BAL is attached.
    #[inline]
    pub const fn bal(&self) -> Option<&Arc<Bal>> {
        self.bal.as_ref()
    }

    /// Sets whether reads not covered by the attached BAL fall back to the cache/database instead
    /// of returning [`ErrorCode::BAL_NOT_COVERED`], and returns `self`.
    #[inline]
    pub const fn with_allow_db_fallback(mut self, allow: bool) -> Self {
        self.allow_db_fallback = allow;
        self
    }

    /// Sets whether reads not covered by the attached BAL fall back to the cache/database.
    #[inline]
    pub const fn set_allow_db_fallback(&mut self, allow: bool) {
        self.allow_db_fallback = allow;
    }

    /// Enables EIP-7928 BAL construction, installing an empty builder, and returns `self`.
    #[inline]
    pub fn with_bal_builder(mut self) -> Self {
        self.bal_builder = Some(Bal::new());
        self
    }

    /// Enables EIP-7928 BAL construction in place, installing an empty builder.
    #[inline]
    pub fn enable_bal_builder(&mut self) {
        self.bal_builder = Some(Bal::new());
    }

    /// Returns the in-progress BAL builder, or `None` when BAL construction is disabled.
    #[inline]
    pub const fn bal_builder(&self) -> Option<&Bal> {
        self.bal_builder.as_ref()
    }

    /// Returns whether BAL construction is enabled.
    #[inline]
    pub const fn has_builder(&self) -> bool {
        self.bal_builder.is_some()
    }

    /// Returns the current EIP-7928 block access index.
    #[inline]
    pub const fn bal_index(&self) -> BlockAccessIndex {
        self.bal_index
    }

    /// Resets the block access index to the pre-execution slot (index `0`).
    ///
    /// Call this before executing a block's transactions.
    #[inline]
    pub const fn reset_bal_index(&mut self) {
        self.bal_index = BlockAccessIndex::PRE_EXECUTION;
    }

    /// Sets the block access index to the given value.
    #[inline]
    pub const fn set_bal_index(&mut self, index: BlockAccessIndex) {
        self.bal_index = index;
    }

    /// Bumps the block access index by one.
    ///
    /// Call this once per transaction so each transaction's writes are recorded under a distinct
    /// index, matching the EIP-7928 layout where transaction `i` maps to index `i + 1`.
    #[inline]
    pub const fn bump_bal_index(&mut self) {
        self.bal_index.increment();
    }

    /// Folds a committed transaction's post-state into the BAL builder at the current
    /// [`Self::bal_index`].
    ///
    /// No-op when BAL construction is disabled. Loaded-but-unchanged accounts and storage slots in
    /// `changes` are recorded as BAL reads; changed ones as writes.
    #[inline]
    pub fn commit_bal(&mut self, changes: &StateChanges) {
        let index = self.bal_index;
        if let Some(bal) = self.bal_builder.as_mut() {
            for (address, change) in &changes.accounts {
                bal.update_account(index, *address, change);
            }
        }
    }

    /// Records an account-info-only change (no storage) into the BAL builder at the current index.
    ///
    /// Used for post-block balance updates -- block rewards and withdrawals -- that mutate the
    /// accepted overlay directly instead of flowing through a transaction commit, so they are not
    /// captured by [`Self::commit_bal`]. No-op when BAL construction is disabled.
    #[inline]
    pub fn commit_account_change(
        &mut self,
        address: Address,
        original: Option<&AccountInfo>,
        current: Option<&AccountInfo>,
    ) {
        let index = self.bal_index;
        if let Some(bal) = self.bal_builder.as_mut() {
            let account = bal.accounts.entry(address).or_default();
            let original = original.cloned().unwrap_or_default();
            let current = current.cloned().unwrap_or_default();
            account.account_info.update(index, &original, &current);
        }
    }

    /// Takes the built BAL, resetting the block access index. Returns `None` when BAL construction
    /// is disabled.
    #[inline]
    pub const fn take_bal_builder(&mut self) -> Option<Bal> {
        self.reset_bal_index();
        self.bal_builder.take()
    }

    /// Takes the built BAL as a canonical EIP-7928 [`BlockAccessList`], resetting the block access
    /// index. Returns `None` when BAL construction is disabled.
    #[inline]
    pub fn take_alloy_bal(&mut self) -> Option<BlockAccessList> {
        self.take_bal_builder().map(Bal::into_alloy_bal)
    }

    /// Resolves `address` in the attached read BAL.
    ///
    /// Returns `Ok(None)` when no BAL is attached, or when the account is uncovered but
    /// [`Self::allow_db_fallback`] is set. Returns [`BalError::AccountNotFound`] when the account
    /// is uncovered and fallback is disabled.
    #[inline]
    pub fn get_bal_account(&self, address: &Address) -> BalResult<Option<&AccountBal>> {
        let Some(bal) = &self.bal else {
            return Ok(None);
        };
        match bal.accounts.get(address) {
            Some(bal_account) => Ok(Some(bal_account)),
            None if self.allow_db_fallback => Ok(None),
            None => Err(BalError::AccountNotFound { address: *address }),
        }
    }

    /// Applies a resolved BAL account's info writes at the current index to `account`.
    ///
    /// `bal_account` comes from [`Self::get_bal_account`], resolved before the raw account is
    /// read from the cache/database.
    #[inline]
    pub fn populate_bal_account(
        &self,
        bal_account: &AccountBal,
        account: &mut Option<AccountInfo>,
    ) {
        let was_present = account.is_some();
        let mut info = account.take().unwrap_or_default();
        let changed = bal_account.populate_account_info(self.bal_index, &mut info);
        // An account absent from the database with no BAL writes stays absent.
        if changed || was_present {
            *account = Some(info);
        }
    }

    /// Resolves storage slot `key` for `address` from the attached read BAL at the current
    /// index.
    ///
    /// Returns `Ok(Some(value))` when the BAL has a write for the slot at or before the current
    /// index. Returns `Ok(None)` when no BAL is attached, when the slot is covered but has no
    /// applicable write (caller should read the cache/database), or when the account/slot is
    /// uncovered but [`Self::allow_db_fallback`] is set. Returns an error when the account or slot
    /// is uncovered and fallback is disabled.
    #[inline]
    pub fn bal_storage(&self, address: &Address, key: &Word) -> BalResult<Option<Word>> {
        let Some(bal) = &self.bal else {
            return Ok(None);
        };
        let Some(bal_account) = bal.accounts.get(address) else {
            if self.allow_db_fallback {
                return Ok(None);
            }
            return Err(BalError::AccountNotFound { address: *address });
        };

        match bal_account.storage.get_bal_writes(address, *key) {
            Ok(writes) => Ok(writes.get(self.bal_index)),
            Err(BalError::SlotNotFound { .. }) if self.allow_db_fallback => Ok(None),
            Err(err) => Err(err),
        }
    }

    /// Stashes a BAL lookup error for later retrieval through [`Self::take_error`] and returns the
    /// sentinel [`ErrorCode::BAL_NOT_COVERED`].
    #[inline]
    pub const fn store_error(&mut self, err: BalError) -> ErrorCode {
        self.bal_error = Some(err);
        ErrorCode::BAL_NOT_COVERED
    }

    /// Takes the stashed BAL error as an [`AnyError`] when `code` is
    /// [`ErrorCode::BAL_NOT_COVERED`].
    ///
    /// Returns `None` for any other code so the caller can fall back to the wrapped database's
    /// error resolution.
    #[inline]
    pub fn take_error(&mut self, code: ErrorCode) -> Option<AnyError> {
        if code != ErrorCode::BAL_NOT_COVERED {
            return None;
        }
        self.bal_error.take().map(AnyError::new)
    }
}
