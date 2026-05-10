//! Database helpers for the EVM state overlay.

use super::state::{AccountInfo, StateChanges};
use crate::{bytecode::Bytecode, interpreter::Word};
use alloc::{boxed::Box, string::ToString};
use alloy_primitives::{Address, B256, keccak256};
use core::{any::Any, error::Error, fmt};

mod cache;
pub use cache::{Cache, CacheDB, InMemoryDB};

/// Commits accepted state changes to a database.
pub trait DatabaseCommit {
    /// Commits state changes to the database.
    fn commit(&mut self, changes: &StateChanges);
}

/// Lightweight handle for a database error.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct DbErrorCode(pub usize);

/// Result of a database operation.
pub type DbResult<T> = Result<T, DbErrorCode>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DbErrorUnavailable(DbErrorCode);

impl fmt::Display for DbErrorUnavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "database error {:?} is unavailable", self.0)
    }
}

impl Error for DbErrorUnavailable {}

/// Backing database view used to initialize mutable [`super::State`].
pub trait Database: Any {
    /// Loads account information.
    fn get_account(&mut self, address: Address) -> DbResult<Option<AccountInfo>>;

    /// Loads bytecode by code hash.
    fn get_code_by_hash(&mut self, code_hash: B256) -> DbResult<Bytecode>;

    /// Loads a persistent storage slot.
    fn get_storage(&mut self, address: Address, key: Word) -> DbResult<Word>;

    /// Loads a historical block hash.
    fn get_block_hash(&mut self, number: Word) -> DbResult<Option<B256>>;

    /// Retrieves the full error for a previously returned error code.
    fn error(&mut self, code: DbErrorCode) -> Box<dyn Error> {
        Box::new(DbErrorUnavailable(code))
    }
}

impl Database for Box<dyn Database> {
    #[inline]
    fn get_account(&mut self, address: Address) -> DbResult<Option<AccountInfo>> {
        self.as_mut().get_account(address)
    }

    #[inline]
    fn get_code_by_hash(&mut self, code_hash: B256) -> DbResult<Bytecode> {
        self.as_mut().get_code_by_hash(code_hash)
    }

    #[inline]
    fn get_storage(&mut self, address: Address, key: Word) -> DbResult<Word> {
        self.as_mut().get_storage(address, key)
    }

    #[inline]
    fn get_block_hash(&mut self, number: Word) -> DbResult<Option<B256>> {
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
    #[inline]
    fn get_account(&mut self, _address: Address) -> DbResult<Option<AccountInfo>> {
        Ok(None)
    }

    #[inline]
    fn get_code_by_hash(&mut self, _code_hash: B256) -> DbResult<Bytecode> {
        Ok(Bytecode::default())
    }

    #[inline]
    fn get_storage(&mut self, _address: Address, _key: Word) -> DbResult<Word> {
        Ok(Word::ZERO)
    }

    #[inline]
    fn get_block_hash(&mut self, number: Word) -> DbResult<Option<B256>> {
        Ok(Some(keccak256(number.to_string().as_bytes())))
    }
}
