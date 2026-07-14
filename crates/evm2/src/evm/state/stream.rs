//! Borrowed state-change streaming traits and adapters.

use super::AccountInfo;
use crate::{bytecode::Bytecode, interpreter::Word};
use alloy_primitives::{Address, B256};
use core::convert::Infallible;

/// Borrowed account information exposed to change sinks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AccountInfoRef<'a> {
    /// Account balance.
    pub balance: Word,
    /// Account nonce.
    pub nonce: u64,
    /// Account code hash.
    pub code_hash: B256,
    /// Borrowed bytecode when the source has it available.
    pub code: Option<&'a Bytecode>,
}

impl<'a> AccountInfoRef<'a> {
    #[inline]
    pub(crate) const fn from_info(info: &'a AccountInfo) -> Self {
        Self {
            balance: info.balance,
            nonce: info.nonce,
            code_hash: info.code_hash,
            code: info.code.as_ref(),
        }
    }

    /// Materializes this borrowed account into owned account info.
    #[inline]
    pub fn to_account_info(self) -> AccountInfo {
        AccountInfo {
            balance: self.balance,
            nonce: self.nonce,
            code_hash: self.code_hash,
            code: self.code.cloned(),
            _non_exhaustive: (),
        }
    }

    #[inline]
    pub(crate) const fn to_account_info_without_code(self) -> AccountInfo {
        AccountInfo {
            balance: self.balance,
            nonce: self.nonce,
            code_hash: self.code_hash,
            code: None,
            _non_exhaustive: (),
        }
    }
}

/// Borrowed account change passed to [`StateChangeSink`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AccountChangeRef<'a> {
    /// Account address.
    pub address: Address,
    /// Account at the start of the source's aggregation boundary.
    pub original: Option<AccountInfoRef<'a>>,
    /// Account after the change. `None` is an explicit deletion.
    pub current: Option<AccountInfoRef<'a>>,
}

impl AccountChangeRef<'_> {
    /// Returns whether this change creates an account.
    #[inline]
    pub const fn created(&self) -> bool {
        self.original.is_none() && self.current.is_some()
    }

    /// Returns whether this change deletes an account.
    #[inline]
    pub const fn deleted(&self) -> bool {
        self.original.is_some() && self.current.is_none()
    }
}

/// Storage slot change passed to [`StateChangeSink`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StorageChange {
    /// Account address.
    pub address: Address,
    /// Storage slot key.
    pub key: Word,
    /// Slot value at the start of the source's aggregation boundary.
    pub original: Word,
    /// Slot value after the change.
    pub current: Word,
}

/// Consumer of borrowed transaction or block state changes.
pub trait StateChangeSink {
    /// Error returned by this sink.
    type Error;

    /// Observes bytecode keyed by code hash.
    #[inline]
    fn bytecode(&mut self, _code_hash: B256, _code: &Bytecode) -> Result<(), Self::Error> {
        Ok(())
    }

    /// Observes an account change.
    #[inline]
    fn account(&mut self, _change: AccountChangeRef<'_>) -> Result<(), Self::Error> {
        Ok(())
    }

    /// Observes a storage wipe marker for an account.
    ///
    /// Sources emit this before any storage slot changes for the same account so sinks can apply
    /// the wipe once, then apply subsequent slot writes.
    #[inline]
    fn storage_wipe(&mut self, _address: Address) -> Result<(), Self::Error> {
        Ok(())
    }

    /// Observes a storage slot change.
    #[inline]
    fn storage(&mut self, _change: StorageChange) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<S> StateChangeSink for &mut S
where
    S: StateChangeSink + ?Sized,
{
    type Error = S::Error;

    #[inline]
    fn bytecode(&mut self, code_hash: B256, code: &Bytecode) -> Result<(), Self::Error> {
        (**self).bytecode(code_hash, code)
    }

    #[inline]
    fn account(&mut self, change: AccountChangeRef<'_>) -> Result<(), Self::Error> {
        (**self).account(change)
    }

    #[inline]
    fn storage_wipe(&mut self, address: Address) -> Result<(), Self::Error> {
        (**self).storage_wipe(address)
    }

    #[inline]
    fn storage(&mut self, change: StorageChange) -> Result<(), Self::Error> {
        (**self).storage(change)
    }
}

/// Source of borrowed state changes.
pub trait StateChangeSource {
    /// Visits all changes in deterministic application order.
    fn visit<S: StateChangeSink>(&self, sink: &mut S) -> Result<(), S::Error>;
}

/// Sink that ignores all changes.
#[derive(Clone, Debug, Default)]
#[allow(missing_copy_implementations)]
pub struct NoopChangeSink(());

impl StateChangeSink for NoopChangeSink {
    type Error = Infallible;
}

/// Sink that forwards each change to two sinks.
#[derive(Clone, Copy, Debug, Default)]
pub struct Tee<A, B> {
    a: A,
    b: B,
}

impl<A, B> Tee<A, B> {
    /// Creates a new tee sink.
    #[inline]
    pub const fn new(a: A, b: B) -> Self {
        Self { a, b }
    }
}

impl<A, B> StateChangeSink for Tee<A, B>
where
    A: StateChangeSink,
    B: StateChangeSink<Error = A::Error>,
{
    type Error = A::Error;

    #[inline]
    fn bytecode(&mut self, code_hash: B256, code: &Bytecode) -> Result<(), Self::Error> {
        self.a.bytecode(code_hash, code)?;
        self.b.bytecode(code_hash, code)
    }

    #[inline]
    fn account(&mut self, change: AccountChangeRef<'_>) -> Result<(), Self::Error> {
        self.a.account(change)?;
        self.b.account(change)
    }

    #[inline]
    fn storage_wipe(&mut self, address: Address) -> Result<(), Self::Error> {
        self.a.storage_wipe(address)?;
        self.b.storage_wipe(address)
    }

    #[inline]
    fn storage(&mut self, change: StorageChange) -> Result<(), Self::Error> {
        self.a.storage(change)?;
        self.b.storage(change)
    }
}
