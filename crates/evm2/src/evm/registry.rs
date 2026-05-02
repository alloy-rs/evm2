//! Type-erased transaction handler registry with typed handlers.
//!
//! Handlers are written against concrete transaction types. The registry stores
//! them behind an object-safe boundary and dispatches by transaction type byte.
//!
//! The registry is generic over the envelope type and the handler output, so it
//! does not force a particular transaction or receipt representation onto the
//! rest of the crate.
//!
//! # Example
//!
//! ```
//! use evm2::registry::{HandlerError, HandlerResult, TxRegistry, TxRequest};
//!
//! const TRANSFER: u8 = 0x01;
//!
//! struct TransferTx {
//!     amount: u64,
//! }
//!
//! enum Envelope {
//!     Transfer(TransferTx),
//! }
//!
//! impl Envelope {
//!     fn as_transfer(&self) -> Option<&TransferTx> {
//!         match self {
//!             Self::Transfer(tx) => Some(tx),
//!         }
//!     }
//! }
//!
//! #[derive(Debug, PartialEq, Eq)]
//! struct Receipt {
//!     gas_used: u64,
//! }
//!
//! fn handle_transfer(req: TxRequest<'_, TransferTx>) -> HandlerResult<Receipt> {
//!     Ok(Receipt { gas_used: 21_000 + req.tx.amount })
//! }
//!
//! let registry = TxRegistry::<Envelope, Receipt>::new().with_handler(
//!     TRANSFER,
//!     Envelope::as_transfer,
//!     handle_transfer,
//! );
//!
//! let tx = Envelope::Transfer(TransferTx { amount: 7 });
//! let receipt = registry.try_get_by_type(TRANSFER)?.call(&tx)?;
//!
//! assert_eq!(receipt, Receipt { gas_used: 21_007 });
//! # Ok::<(), HandlerError>(())
//! ```

use alloc::boxed::Box;
use core::{array, fmt, marker::PhantomData};
use thiserror::Error;

/// Convenience result type used by the registry and handlers.
pub type HandlerResult<T> = core::result::Result<T, HandlerError>;

/// Registry and handler errors.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum HandlerError {
    /// No handler is registered for the transaction type byte.
    #[error("unsupported transaction type 0x{0:02x}")]
    UnsupportedTransactionType(u8),
    /// A registered handler's extractor did not match the provided envelope.
    #[error("envelope did not contain expected transaction type 0x{expected:02x}")]
    WrongTransactionType {
        /// Expected transaction type byte.
        expected: u8,
    },
}

/// Request passed to a typed transaction handler.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct TxRequest<'a, Tx> {
    /// Concrete transaction extracted from the envelope.
    pub tx: &'a Tx,
}

/// A typed transaction handler.
///
/// `Tx` remains concrete. This is what gives handlers strong type guarantees
/// even though the registry itself is type-erased.
pub trait TxHandler<Tx, Output> {
    /// Executes the handler.
    fn call(&self, req: TxRequest<'_, Tx>) -> HandlerResult<Output>;
}

impl<Tx, Output, F> TxHandler<Tx, Output> for F
where
    F: for<'a> Fn(TxRequest<'a, Tx>) -> HandlerResult<Output>,
{
    fn call(&self, req: TxRequest<'_, Tx>) -> HandlerResult<Output> {
        self(req)
    }
}

/// An erased transaction handler returned by [`TxRegistry`].
#[derive(Clone, Copy)]
pub struct AnyTxHandler<'a, Env, Output> {
    inner: &'a dyn ErasedTxHandler<Env, Output>,
}

impl<Env, Output> fmt::Debug for AnyTxHandler<'_, Env, Output> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AnyTxHandler").finish_non_exhaustive()
    }
}

impl<Env, Output> AnyTxHandler<'_, Env, Output> {
    /// Executes the erased handler against an envelope.
    pub fn call(&self, env: &Env) -> HandlerResult<Output> {
        self.inner.call(env)
    }
}

trait ErasedTxHandler<Env, Output> {
    fn call(&self, env: &Env) -> HandlerResult<Output>;
}

struct HandlerAdapter<Tx, H, F> {
    type_id: u8,
    handler: H,
    extract: F,
    _tx: PhantomData<fn() -> Tx>,
}

impl<Tx, H, F> HandlerAdapter<Tx, H, F> {
    const fn new(type_id: u8, extract: F, handler: H) -> Self {
        Self { type_id, handler, extract, _tx: PhantomData }
    }
}

impl<Env, Tx, Output, H, F> ErasedTxHandler<Env, Output> for HandlerAdapter<Tx, H, F>
where
    H: TxHandler<Tx, Output>,
    F: for<'a> Fn(&'a Env) -> Option<&'a Tx>,
{
    fn call(&self, env: &Env) -> HandlerResult<Output> {
        let tx = (self.extract)(env)
            .ok_or(HandlerError::WrongTransactionType { expected: self.type_id })?;
        self.handler.call(TxRequest { tx })
    }
}

type HandlerTable<Env, Output> = [Option<Box<dyn ErasedTxHandler<Env, Output>>>; 256];

/// A type-erased transaction handler registry keyed by transaction type byte.
pub struct TxRegistry<Env, Output = ()> {
    handlers: Box<HandlerTable<Env, Output>>,
}

impl<Env, Output> fmt::Debug for TxRegistry<Env, Output> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let len = self.handlers.iter().filter(|handler| handler.is_some()).count();
        f.debug_struct("TxRegistry").field("len", &len).finish_non_exhaustive()
    }
}

impl<Env, Output> Default for TxRegistry<Env, Output> {
    fn default() -> Self {
        Self::new()
    }
}

impl<Env, Output> TxRegistry<Env, Output> {
    /// Creates an empty registry.
    pub fn new() -> Self {
        Self { handlers: Box::new(array::from_fn(|_| None)) }
    }

    /// Registers a typed handler for a transaction type byte.
    ///
    /// `extract` projects `Tx` out of the envelope. The handler remains typed
    /// as `TxHandler<Tx, Output>`; only this registry boundary is erased.
    pub fn register<Tx, H, F>(&mut self, type_id: u8, extract: F, handler: H) -> &mut Self
    where
        Tx: 'static,
        H: TxHandler<Tx, Output> + 'static,
        F: for<'a> Fn(&'a Env) -> Option<&'a Tx> + 'static,
    {
        self.handlers[type_id as usize] =
            Some(Box::new(HandlerAdapter::new(type_id, extract, handler)));
        self
    }

    /// Adds a typed handler and returns the registry.
    #[must_use]
    pub fn with_handler<Tx, H, F>(mut self, type_id: u8, extract: F, handler: H) -> Self
    where
        Tx: 'static,
        H: TxHandler<Tx, Output> + 'static,
        F: for<'a> Fn(&'a Env) -> Option<&'a Tx> + 'static,
    {
        self.register(type_id, extract, handler);
        self
    }

    /// Returns true if a handler is registered for `type_id`.
    pub fn contains(&self, type_id: u8) -> bool {
        self.handlers[type_id as usize].is_some()
    }

    /// Returns the erased handler registered for `type_id`, if any.
    pub fn get_by_type(&self, type_id: u8) -> Option<AnyTxHandler<'_, Env, Output>> {
        self.handlers[type_id as usize].as_deref().map(|inner| AnyTxHandler { inner })
    }

    /// Returns the erased handler registered for `type_id`.
    pub fn try_get_by_type(&self, type_id: u8) -> HandlerResult<AnyTxHandler<'_, Env, Output>> {
        self.get_by_type(type_id).ok_or(HandlerError::UnsupportedTransactionType(type_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    #[derive(Debug)]
    struct TransferTx {
        amount: u64,
    }

    #[derive(Debug)]
    struct CreateTx {
        initcode: Vec<u8>,
    }

    #[derive(Debug)]
    enum Envelope {
        Transfer(TransferTx),
        Create(CreateTx),
    }

    #[derive(Debug, PartialEq, Eq)]
    struct Receipt {
        success: bool,
        cumulative_gas_used: u64,
    }

    fn transfer(env: &Envelope) -> Option<&TransferTx> {
        match env {
            Envelope::Transfer(tx) => Some(tx),
            Envelope::Create(_) => None,
        }
    }

    fn create(env: &Envelope) -> Option<&CreateTx> {
        match env {
            Envelope::Create(tx) => Some(tx),
            Envelope::Transfer(_) => None,
        }
    }

    fn receipt(cumulative_gas_used: u64) -> Receipt {
        Receipt { success: true, cumulative_gas_used }
    }

    fn handle_transfer(req: TxRequest<'_, TransferTx>) -> HandlerResult<Receipt> {
        let gas_used = 21_000 + req.tx.amount;
        Ok(receipt(gas_used))
    }

    fn handle_create(req: TxRequest<'_, CreateTx>) -> HandlerResult<Receipt> {
        let gas_used = 53_000 + req.tx.initcode.len() as u64;
        Ok(receipt(gas_used))
    }

    fn call_registered(
        registry: &TxRegistry<Envelope, Receipt>,
        type_id: u8,
        env: &Envelope,
    ) -> HandlerResult<Receipt> {
        registry.try_get_by_type(type_id)?.call(env)
    }

    #[test]
    fn dispatches_to_typed_handlers_from_erased_registry() {
        let mut registry = TxRegistry::<Envelope, Receipt>::new();
        registry.register(0x01, transfer, handle_transfer);
        registry.register(0x02, create, handle_create);

        let transfer_receipt =
            call_registered(&registry, 0x01, &Envelope::Transfer(TransferTx { amount: 7 }))
                .expect("transfer handler is registered");
        assert_eq!(transfer_receipt, receipt(21_007));

        let create_receipt =
            call_registered(&registry, 0x02, &Envelope::Create(CreateTx { initcode: Vec::new() }))
                .expect("create handler is registered");
        assert_eq!(create_receipt, receipt(53_000));
    }

    #[test]
    fn reports_unsupported_and_mismatched_types() {
        let mut registry = TxRegistry::<Envelope, Receipt>::new();
        registry.register(0x01, transfer, handle_transfer);

        assert_eq!(
            call_registered(&registry, 0xff, &Envelope::Transfer(TransferTx { amount: 7 })),
            Err(HandlerError::UnsupportedTransactionType(0xff))
        );
        assert_eq!(
            call_registered(&registry, 0x01, &Envelope::Create(CreateTx { initcode: Vec::new() })),
            Err(HandlerError::WrongTransactionType { expected: 0x01 })
        );
    }
}
