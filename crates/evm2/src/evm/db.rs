//! Database helpers for the EVM state overlay.

use super::{NonStaticAny, state::AccountInfo};
use crate::{AnyError, ErrorCode, bytecode::Bytecode, error::error_unavailable, interpreter::Word};
use alloc::{boxed::Box, string::ToString};
use alloy_primitives::{Address, B256, keccak256};
use core::error::Error;

mod cache;
pub use cache::{AccountStorageCache, Cache, CacheDB, InMemoryDB};

/// Result of a database operation.
pub type DbResult<T> = Result<T, ErrorCode>;

/// Backing database implementation with a concrete error type.
pub trait Database: NonStaticAny {
    /// Database error type.
    type Error: Error + Send + Sync + 'static;

    /// Loads account information.
    fn get_account(&mut self, address: &Address) -> Result<Option<AccountInfo>, Self::Error>;

    /// Loads bytecode by code hash.
    fn get_code_by_hash(&mut self, code_hash: &B256) -> Result<Bytecode, Self::Error>;

    /// Loads a persistent storage slot.
    fn get_storage(&mut self, address: &Address, key: &Word) -> Result<Word, Self::Error>;

    /// Loads a historical block hash.
    fn get_block_hash(&mut self, number: &Word) -> Result<Option<B256>, Self::Error>;
}

/// Object-safe database adapter for typed database implementations.
#[derive(Clone, Debug)]
pub struct Db<T: Database> {
    db: T,
    result: Result<(), AnyError>,
}

impl<T: Database + Default> Default for Db<T> {
    #[inline]
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T: Database> Db<T> {
    /// Creates a new database adapter.
    #[inline]
    pub const fn new(db: T) -> Self {
        Self { db, result: Ok(()) }
    }

    /// Returns the wrapped database.
    #[inline]
    pub const fn inner(&self) -> &T {
        &self.db
    }

    /// Returns the wrapped database mutably.
    #[inline]
    pub const fn inner_mut(&mut self) -> &mut T {
        &mut self.db
    }

    /// Consumes the adapter and returns the wrapped database.
    #[inline]
    pub fn into_inner(self) -> T {
        self.db
    }

    /// Returns the stored database result.
    #[inline]
    pub const fn result(&self) -> Result<(), &AnyError> {
        self.result.as_ref().copied()
    }

    /// Takes the stored database result.
    #[inline]
    pub const fn take_result(&mut self) -> Result<(), AnyError> {
        core::mem::replace(&mut self.result, Ok(()))
    }

    #[inline]
    fn store_error(&mut self, err: T::Error) -> ErrorCode {
        self.result = Err(AnyError::new(err));
        ErrorCode::STORED_ERROR
    }
}

impl<T: Database> DynDatabase for Db<T> {
    #[inline]
    fn get_account(&mut self, address: &Address) -> DbResult<Option<AccountInfo>> {
        self.db.get_account(address).map_err(|err| self.store_error(err))
    }

    #[inline]
    fn get_code_by_hash(&mut self, code_hash: &B256) -> DbResult<Bytecode> {
        self.db.get_code_by_hash(code_hash).map_err(|err| self.store_error(err))
    }

    #[inline]
    fn get_storage(&mut self, address: &Address, key: &Word) -> DbResult<Word> {
        self.db.get_storage(address, key).map_err(|err| self.store_error(err))
    }

    #[inline]
    fn get_block_hash(&mut self, number: &Word) -> DbResult<Option<B256>> {
        self.db.get_block_hash(number).map_err(|err| self.store_error(err))
    }

    #[inline]
    fn error(&mut self, code: ErrorCode) -> AnyError {
        if code == ErrorCode::STORED_ERROR
            && let Err(err) = self.result.clone()
        {
            return err;
        }
        error_unavailable(code)
    }
}

/// Backing database view used to initialize mutable [`super::State`].
pub trait DynDatabase: NonStaticAny {
    /// Loads account information.
    fn get_account(&mut self, address: &Address) -> DbResult<Option<AccountInfo>>;

    /// Loads bytecode by code hash.
    fn get_code_by_hash(&mut self, code_hash: &B256) -> DbResult<Bytecode>;

    /// Loads a persistent storage slot.
    fn get_storage(&mut self, address: &Address, key: &Word) -> DbResult<Word>;

    /// Loads a historical block hash.
    fn get_block_hash(&mut self, number: &Word) -> DbResult<Option<B256>>;

    /// Retrieves the full error for a previously returned error code.
    fn error(&mut self, code: ErrorCode) -> AnyError {
        error_unavailable(code)
    }
}

/// Counts calls made through a [`DynDatabase`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DbStatsCounts {
    /// Number of account loads.
    pub get_account: u64,
    /// Number of bytecode loads by hash.
    pub get_code_by_hash: u64,
    /// Number of storage slot loads.
    pub get_storage: u64,
    /// Number of storage loads whose address matched the previous storage load.
    pub get_storage_same_address_repeats: u64,
    /// Longest run of storage loads for the same address.
    pub get_storage_same_address_longest_streak: u64,
    /// Number of block hash loads.
    pub get_block_hash: u64,
    /// Number of error lookups.
    pub error: u64,
}

impl core::ops::AddAssign for DbStatsCounts {
    #[inline]
    fn add_assign(&mut self, rhs: Self) {
        self.get_account += rhs.get_account;
        self.get_code_by_hash += rhs.get_code_by_hash;
        self.get_storage += rhs.get_storage;
        self.get_storage_same_address_repeats += rhs.get_storage_same_address_repeats;
        self.get_storage_same_address_longest_streak = self
            .get_storage_same_address_longest_streak
            .max(rhs.get_storage_same_address_longest_streak);
        self.get_block_hash += rhs.get_block_hash;
        self.error += rhs.error;
    }
}

/// Database wrapper that records method call counts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DbStats<D> {
    db: D,
    counts: DbStatsCounts,
    last_storage_address: Option<Address>,
    storage_address_streak: u64,
}

impl<D> DbStats<D> {
    /// Creates a stats wrapper around `db`.
    #[inline]
    pub const fn new(db: D) -> Self {
        Self {
            db,
            counts: DbStatsCounts {
                get_account: 0,
                get_code_by_hash: 0,
                get_storage: 0,
                get_storage_same_address_repeats: 0,
                get_storage_same_address_longest_streak: 0,
                get_block_hash: 0,
                error: 0,
            },
            last_storage_address: None,
            storage_address_streak: 0,
        }
    }

    /// Returns the wrapped database.
    #[inline]
    pub const fn inner(&self) -> &D {
        &self.db
    }

    /// Returns the wrapped database mutably.
    #[inline]
    pub const fn inner_mut(&mut self) -> &mut D {
        &mut self.db
    }

    /// Consumes the wrapper and returns the wrapped database.
    #[inline]
    pub fn into_inner(self) -> D {
        self.db
    }

    /// Returns recorded method call counts.
    #[inline]
    pub const fn counts(&self) -> DbStatsCounts {
        self.counts
    }

    #[inline]
    fn record_storage_load(&mut self, address: &Address) {
        self.counts.get_storage += 1;
        if self.last_storage_address.as_ref() == Some(address) {
            self.counts.get_storage_same_address_repeats += 1;
            self.storage_address_streak += 1;
        } else {
            self.last_storage_address = Some(*address);
            self.storage_address_streak = 1;
        }
        self.counts.get_storage_same_address_longest_streak =
            self.counts.get_storage_same_address_longest_streak.max(self.storage_address_streak);
    }
}

impl<D: DynDatabase> DynDatabase for DbStats<D> {
    #[inline]
    fn get_account(&mut self, address: &Address) -> DbResult<Option<AccountInfo>> {
        self.counts.get_account += 1;
        self.db.get_account(address)
    }

    #[inline]
    fn get_code_by_hash(&mut self, code_hash: &B256) -> DbResult<Bytecode> {
        self.counts.get_code_by_hash += 1;
        self.db.get_code_by_hash(code_hash)
    }

    #[inline]
    fn get_storage(&mut self, address: &Address, key: &Word) -> DbResult<Word> {
        self.record_storage_load(address);
        self.db.get_storage(address, key)
    }

    #[inline]
    fn get_block_hash(&mut self, number: &Word) -> DbResult<Option<B256>> {
        self.counts.get_block_hash += 1;
        self.db.get_block_hash(number)
    }

    #[inline]
    fn error(&mut self, code: ErrorCode) -> AnyError {
        self.counts.error += 1;
        self.db.error(code)
    }
}

impl<'a> core::ops::Deref for dyn DynDatabase + 'a {
    type Target = dyn NonStaticAny + 'a;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self
    }
}

impl<'a> core::ops::DerefMut for dyn DynDatabase + 'a {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self
    }
}

#[inline]
pub(crate) fn boxed_dyn_database<'a>(database: impl DynDatabase + 'a) -> Box<dyn DynDatabase + 'a> {
    Box::new(database)
}

impl<T: DynDatabase + ?Sized> DynDatabase for Box<T> {
    #[inline]
    fn get_account(&mut self, address: &Address) -> DbResult<Option<AccountInfo>> {
        self.as_mut().get_account(address)
    }

    #[inline]
    fn get_code_by_hash(&mut self, code_hash: &B256) -> DbResult<Bytecode> {
        self.as_mut().get_code_by_hash(code_hash)
    }

    #[inline]
    fn get_storage(&mut self, address: &Address, key: &Word) -> DbResult<Word> {
        self.as_mut().get_storage(address, key)
    }

    #[inline]
    fn get_block_hash(&mut self, number: &Word) -> DbResult<Option<B256>> {
        self.as_mut().get_block_hash(number)
    }

    #[inline]
    fn error(&mut self, code: ErrorCode) -> AnyError {
        self.as_mut().error(code)
    }
}

/// Empty backing database.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EmptyDB(());

impl Database for EmptyDB {
    type Error = core::convert::Infallible;

    #[inline]
    fn get_account(&mut self, _address: &Address) -> Result<Option<AccountInfo>, Self::Error> {
        Ok(None)
    }

    #[inline]
    fn get_code_by_hash(&mut self, _code_hash: &B256) -> Result<Bytecode, Self::Error> {
        Ok(Bytecode::default())
    }

    #[inline]
    fn get_storage(&mut self, _address: &Address, _key: &Word) -> Result<Word, Self::Error> {
        Ok(Word::ZERO)
    }

    #[inline]
    fn get_block_hash(&mut self, number: &Word) -> Result<Option<B256>, Self::Error> {
        Ok(Some(keccak256(number.to_string().as_bytes())))
    }
}

impl DynDatabase for EmptyDB {
    #[inline]
    fn get_account(&mut self, address: &Address) -> DbResult<Option<AccountInfo>> {
        Db::new(*self).get_account(address)
    }

    #[inline]
    fn get_code_by_hash(&mut self, code_hash: &B256) -> DbResult<Bytecode> {
        Db::new(*self).get_code_by_hash(code_hash)
    }

    #[inline]
    fn get_storage(&mut self, address: &Address, key: &Word) -> DbResult<Word> {
        Db::new(*self).get_storage(address, key)
    }

    #[inline]
    fn get_block_hash(&mut self, number: &Word) -> DbResult<Option<B256>> {
        Db::new(*self).get_block_hash(number)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct BorrowingDb<'a> {
        value: &'a u8,
    }

    impl DynDatabase for BorrowingDb<'_> {
        fn get_account(&mut self, _address: &Address) -> DbResult<Option<AccountInfo>> {
            Ok(None)
        }

        fn get_code_by_hash(&mut self, _code_hash: &B256) -> DbResult<Bytecode> {
            Ok(Bytecode::default())
        }

        fn get_storage(&mut self, _address: &Address, _key: &Word) -> DbResult<Word> {
            Ok(Word::ZERO)
        }

        fn get_block_hash(&mut self, _number: &Word) -> DbResult<Option<B256>> {
            Ok(None)
        }
    }

    #[test]
    fn database_downcast_erases_lifetimes() {
        let value = 1;
        let db = BorrowingDb { value: &value };
        let erased = &db as &dyn DynDatabase;

        let downcasted = erased.downcast_ref::<BorrowingDb<'static>>().unwrap();
        let static_value: &'static u8 = downcasted.value;

        assert_eq!(static_value as *const u8, &value as *const u8);
    }
}
