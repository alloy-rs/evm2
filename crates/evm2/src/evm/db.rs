//! Database helpers for the EVM state overlay.

use super::state::AccountInfo;
use crate::{bytecode::Bytecode, interpreter::Word};
use alloc::string::ToString;
use alloy_primitives::{Address, B256, keccak256};
use core::any::Any;

mod cache;
pub use cache::{Cache, CacheDB, InMemoryDB};

/// Backing database view used to initialize mutable [`super::State`].
pub trait Database: Any {
    /// Returns this database as [`Any`].
    fn as_any(&self) -> &dyn Any;

    /// Returns this database as mutable [`Any`].
    fn as_any_mut(&mut self) -> &mut dyn Any;

    /// Loads account information.
    fn get_account(&mut self, address: Address) -> Option<AccountInfo>;

    /// Loads bytecode by code hash.
    fn get_code_by_hash(&mut self, code_hash: B256) -> Bytecode;

    /// Loads a persistent storage slot.
    fn get_storage(&mut self, address: Address, key: Word) -> Word;

    /// Loads a historical block hash.
    fn get_block_hash(&mut self, number: Word) -> Option<B256>;
}

impl dyn Database {
    /// Returns `true` if the boxed database has type `T`.
    #[inline]
    pub fn is<T: Database>(&self) -> bool {
        self.as_any().is::<T>()
    }

    /// Returns the concrete database if it has type `T`.
    #[inline]
    pub fn downcast_ref<T: Database>(&self) -> Option<&T> {
        self.as_any().downcast_ref()
    }

    /// Returns the concrete database mutably if it has type `T`.
    #[inline]
    pub fn downcast_mut<T: Database>(&mut self) -> Option<&mut T> {
        self.as_any_mut().downcast_mut()
    }
}

impl Database for alloc::boxed::Box<dyn Database> {
    #[inline]
    fn as_any(&self) -> &dyn Any {
        self.as_ref().as_any()
    }

    #[inline]
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self.as_mut().as_any_mut()
    }

    #[inline]
    fn get_account(&mut self, address: Address) -> Option<AccountInfo> {
        self.as_mut().get_account(address)
    }

    #[inline]
    fn get_code_by_hash(&mut self, code_hash: B256) -> Bytecode {
        self.as_mut().get_code_by_hash(code_hash)
    }

    #[inline]
    fn get_storage(&mut self, address: Address, key: Word) -> Word {
        self.as_mut().get_storage(address, key)
    }

    #[inline]
    fn get_block_hash(&mut self, number: Word) -> Option<B256> {
        self.as_mut().get_block_hash(number)
    }
}

/// Empty backing database.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EmptyDB(());

impl Database for EmptyDB {
    #[inline]
    fn as_any(&self) -> &dyn Any {
        self
    }

    #[inline]
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    #[inline]
    fn get_account(&mut self, _address: Address) -> Option<AccountInfo> {
        None
    }

    #[inline]
    fn get_code_by_hash(&mut self, _code_hash: B256) -> Bytecode {
        Bytecode::default()
    }

    #[inline]
    fn get_storage(&mut self, _address: Address, _key: Word) -> Word {
        Word::ZERO
    }

    #[inline]
    fn get_block_hash(&mut self, number: Word) -> Option<B256> {
        Some(keccak256(number.to_string().as_bytes()))
    }
}
