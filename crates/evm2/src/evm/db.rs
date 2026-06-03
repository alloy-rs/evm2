//! Database helpers for the EVM state overlay.

use super::state::{AccountInfo, StateChanges};
use crate::{bytecode::Bytecode, interpreter::Word};
use alloc::{boxed::Box, string::ToString};
use alloy_primitives::{Address, B256, keccak256};
use core::{any::Any, error::Error, fmt, num::NonZeroUsize};

mod cache;
pub use cache::{Cache, CacheDB, InMemoryDB};

/// Commits accepted state changes to a database.
pub trait DatabaseCommit {
    /// Commits state changes to the database.
    fn commit(&mut self, changes: &StateChanges);
}

/// Lightweight handle for a database error.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DbErrorCode(NonZeroUsize);

impl DbErrorCode {
    /// Creates a database error code.
    #[inline]
    pub const fn new(code: usize) -> Option<Self> {
        let Some(code) = NonZeroUsize::new(code) else {
            return None;
        };
        Some(Self(code))
    }

    /// Returns the raw database error code.
    #[inline]
    pub const fn get(self) -> usize {
        self.0.get()
    }

    /// Updates the raw database error code.
    #[inline]
    pub fn set(&mut self, code: usize) -> Option<()> {
        let code = NonZeroUsize::new(code)?;
        self.0 = code;
        Some(())
    }
}

/// Result of a database operation.
pub type DbResult<T> = Result<T, DbErrorCode>;

/// Backing database implementation with a concrete error type.
pub trait Database: Any {
    /// Database error type.
    type Error: Error + 'static;

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
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Db<T: Database> {
    db: T,
    result: Option<T::Error>,
}

impl<T: Database> Db<T> {
    /// Creates a new database adapter.
    #[inline]
    pub const fn new(db: T) -> Self {
        Self { db, result: None }
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

    /// Returns the stored database error.
    #[inline]
    pub const fn result(&self) -> Option<&T::Error> {
        self.result.as_ref()
    }

    /// Takes the stored database error.
    #[inline]
    pub const fn take_result(&mut self) -> Option<T::Error> {
        self.result.take()
    }

    #[inline]
    fn store_error(&mut self, err: T::Error) -> DbErrorCode {
        self.result = Some(err);
        stored_error_code()
    }
}

impl<T: Database + DatabaseCommit> DatabaseCommit for Db<T> {
    #[inline]
    fn commit(&mut self, changes: &StateChanges) {
        self.db.commit(changes);
    }
}

#[inline]
pub(crate) fn stored_error_code() -> DbErrorCode {
    match DbErrorCode::new(1) {
        Some(code) => code,
        None => unreachable!("stored database error code is non-zero"),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DbErrorUnavailable(DbErrorCode);

impl fmt::Display for DbErrorUnavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "database error {:?} is unavailable", self.0)
    }
}

impl Error for DbErrorUnavailable {}

#[inline]
pub(crate) fn db_error_unavailable(code: DbErrorCode) -> Box<dyn Error> {
    Box::new(DbErrorUnavailable(code))
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
    fn error(&mut self, code: DbErrorCode) -> Box<dyn Error> {
        if code == stored_error_code()
            && let Some(err) = self.result.take()
        {
            return Box::new(err);
        }
        db_error_unavailable(code)
    }
}

/// Backing database view used to initialize mutable [`super::State`].
pub trait DynDatabase: Any {
    /// Loads account information.
    fn get_account(&mut self, address: &Address) -> DbResult<Option<AccountInfo>>;

    /// Loads bytecode by code hash.
    fn get_code_by_hash(&mut self, code_hash: &B256) -> DbResult<Bytecode>;

    /// Loads a persistent storage slot.
    fn get_storage(&mut self, address: &Address, key: &Word) -> DbResult<Word>;

    /// Loads a historical block hash.
    fn get_block_hash(&mut self, number: &Word) -> DbResult<Option<B256>>;

    /// Retrieves the full error for a previously returned error code.
    fn error(&mut self, code: DbErrorCode) -> Box<dyn Error> {
        db_error_unavailable(code)
    }
}

impl core::ops::Deref for dyn DynDatabase + '_ {
    type Target = dyn Any;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self
    }
}

impl core::ops::DerefMut for dyn DynDatabase + '_ {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self
    }
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
    fn error(&mut self, code: DbErrorCode) -> Box<dyn Error> {
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
