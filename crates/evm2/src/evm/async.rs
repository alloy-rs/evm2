//! Async execution support for synchronous EVM hosts.
//!
//! This module runs the synchronous EVM on a native fiber. Synchronous host methods can then use
//! `block_on_current` to poll an async operation; if that operation is pending, the fiber is
//! suspended and the outer async task returns `Poll::Pending`.

use crate::{
    bytecode::Bytecode,
    evm::{
        AccountInfo, DatabaseCommit, DbErrorCode, DbResult, DynDatabase, StateChanges,
        db_error_unavailable, stored_error_code,
    },
    interpreter::Word,
};
use alloc::boxed::Box;
use alloy_primitives::{Address, B256};
use core::{
    any::Any, fmt, future::Future, marker::PhantomData, pin::Pin, ptr::NonNull, task::Poll,
};
use corosensei::{Coroutine, CoroutineResult, Yielder, stack::DefaultStack};
use std::{cell::Cell, error::Error, io, task::Context};
use tokio::{runtime::Handle, task};

type Resume = AsyncResult<NonNull<Context<'static>>>;
type Yield = ();
type Complete<R> = AsyncResult<R>;
type EvmFiber<R> = Coroutine<Resume, Yield, Complete<R>, DefaultStack>;

const DEFAULT_STACK_SIZE: usize = 1024 * 1024;

/// Reusable async EVM fiber stack storage.
#[derive(Default)]
pub(crate) struct FiberStack {
    stack: Option<DefaultStack>,
}

impl FiberStack {
    #[inline]
    fn take_or_new(&mut self) -> AsyncResult<DefaultStack> {
        match self.stack.take() {
            Some(stack) => Ok(stack),
            None => DefaultStack::new(DEFAULT_STACK_SIZE).map_err(AsyncError::Io),
        }
    }

    #[inline]
    fn put(&mut self, stack: DefaultStack) {
        debug_assert!(self.stack.is_none());
        self.stack = Some(stack);
    }
}

thread_local! {
    static CURRENT: Cell<Option<NonNull<CurrentFiber>>> = const { Cell::new(None) };
}

/// Result type used by async EVM execution helpers.
pub type AsyncResult<T, E = core::convert::Infallible> = Result<T, AsyncError<E>>;

/// Error returned by async EVM execution helpers.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AsyncError<E = core::convert::Infallible> {
    /// The async EVM fiber was cancelled before execution completed.
    #[error("async EVM execution was cancelled")]
    Cancelled,
    /// An async host operation was called outside an async EVM fiber.
    #[error("async host operation requires EVM async fiber execution")]
    NotOnFiber,
    /// Blocking async I/O was requested outside a supported Tokio runtime.
    #[error("async host operation requires a Tokio multi-thread runtime")]
    Runtime,
    /// Async fiber stack setup failed.
    #[error(transparent)]
    Io(io::Error),
    /// The wrapped operation returned an error.
    #[error(transparent)]
    Inner(#[from] E),
}

impl AsyncError {
    fn with_inner_error<E>(self) -> AsyncError<E> {
        match self {
            Self::Cancelled => AsyncError::Cancelled,
            Self::NotOnFiber => AsyncError::NotOnFiber,
            Self::Runtime => AsyncError::Runtime,
            Self::Io(error) => AsyncError::Io(error),
            Self::Inner(error) => match error {},
        }
    }
}

struct CurrentFiber {
    suspend: NonNull<Yielder<Resume, Yield>>,
    future_cx: NonNull<Context<'static>>,
    cancelled: bool,
}

impl CurrentFiber {
    #[inline]
    fn context(&mut self) -> &mut Context<'_> {
        unsafe { restore_context_lifetime(self.future_cx.as_mut()) }
    }

    #[inline]
    fn suspend(&mut self) -> AsyncResult<()> {
        match unsafe { self.suspend.as_ref() }.suspend(()) {
            Ok(cx) => {
                self.future_cx = cx;
                Ok(())
            }
            Err(error) => {
                self.cancelled = true;
                Err(error)
            }
        }
    }

    #[inline]
    const fn is_cancelled(&self) -> bool {
        self.cancelled
    }
}

struct ResetCurrentFiber(Option<NonNull<CurrentFiber>>);

impl Drop for ResetCurrentFiber {
    fn drop(&mut self) {
        CURRENT.set(self.0);
    }
}

/// Runs `func` on a native fiber and awaits its completion.
///
/// Synchronous code running inside `func` may call [`block_on_current`] to wait for async host
/// operations without blocking the executor thread.
#[cfg(test)]
pub(crate) fn on_fiber_result<'a, R, E>(
    func: impl FnOnce() -> Result<R, E> + 'a,
) -> impl Future<Output = AsyncResult<R, E>> + Send + 'a
where
    R: Send + 'a,
    E: Send + 'a,
{
    OnFiber::new(func)
}

/// Runs `func` on a native fiber backed by a reusable EVM stack slot.
///
/// # Safety
///
/// `stack` must point to valid stack storage for the lifetime of the returned future. That storage
/// must not be accessed by anything else until the returned future is dropped.
pub(crate) unsafe fn on_fiber_result_with_stack<'a, R, E>(
    stack: NonNull<FiberStack>,
    func: impl FnOnce() -> Result<R, E> + 'a,
) -> impl Future<Output = AsyncResult<R, E>> + Send + 'a
where
    R: Send + 'a,
    E: Send + 'a,
{
    OnFiber::with_stack(stack, func)
}

#[cfg(test)]
pub(crate) fn on_fiber<'a, R>(
    func: impl FnOnce() -> R + 'a,
) -> impl Future<Output = AsyncResult<R>> + Send + 'a
where
    R: Send + 'a,
{
    on_fiber_result(move || Ok::<_, core::convert::Infallible>(func()))
}

/// Runs `func` on a native fiber backed by a reusable EVM stack slot.
///
/// # Safety
///
/// See [`on_fiber_result_with_stack`].
pub(crate) unsafe fn on_fiber_with_stack<'a, R>(
    stack: NonNull<FiberStack>,
    func: impl FnOnce() -> R + 'a,
) -> impl Future<Output = AsyncResult<R>> + Send + 'a
where
    R: Send + 'a,
{
    unsafe { on_fiber_result_with_stack(stack, move || Ok::<_, core::convert::Infallible>(func())) }
}

enum OnFiber<'a, R, E> {
    Running(FiberFuture<'a, Result<R, E>>),
    Error(Option<AsyncError>),
    Done,
}

impl<'a, R, E> OnFiber<'a, R, E> {
    #[cfg(test)]
    fn new(func: impl FnOnce() -> Result<R, E> + 'a) -> Self {
        Self::new_inner(None, func)
    }

    fn with_stack(stack: NonNull<FiberStack>, func: impl FnOnce() -> Result<R, E> + 'a) -> Self {
        Self::new_inner(Some(stack), func)
    }

    fn new_inner(
        stack: Option<NonNull<FiberStack>>,
        func: impl FnOnce() -> Result<R, E> + 'a,
    ) -> Self {
        match FiberFuture::new(stack, func) {
            Ok(fiber) => Self::Running(fiber),
            Err(error) => Self::Error(Some(error)),
        }
    }
}

impl<R, E> Future for OnFiber<'_, R, E> {
    type Output = AsyncResult<R, E>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        match this {
            Self::Running(fiber) => match Pin::new(fiber).poll(cx) {
                Poll::Ready(Ok(Ok(value))) => {
                    *this = Self::Done;
                    Poll::Ready(Ok(value))
                }
                Poll::Ready(Ok(Err(error))) => {
                    *this = Self::Done;
                    Poll::Ready(Err(AsyncError::Inner(error)))
                }
                Poll::Ready(Err(error)) => {
                    *this = Self::Done;
                    Poll::Ready(Err(error.with_inner_error()))
                }
                Poll::Pending => Poll::Pending,
            },
            Self::Error(error) => {
                let error = error.take().expect("async EVM fiber error already returned");
                Poll::Ready(Err(error.with_inner_error()))
            }
            Self::Done => panic!("async EVM fiber polled after completion"),
        }
    }
}

struct FiberFuture<'a, R> {
    fiber: Option<EvmFiber<R>>,
    stack: Option<NonNull<FiberStack>>,
    _marker: PhantomData<&'a ()>,
}

// SAFETY: The future may move between polls, but the coroutine stack itself is heap allocated and
// is only resumed through `poll` with a fresh task context. Values that can remain on the coroutine
// stack across suspension are required to be `Send` by the blocking boundary.
unsafe impl<R: Send> Send for FiberFuture<'_, R> {}

impl<'a, R> FiberFuture<'a, R> {
    fn new(
        mut stack: Option<NonNull<FiberStack>>,
        func: impl FnOnce() -> R + 'a,
    ) -> AsyncResult<Self> {
        let fiber_stack = match &mut stack {
            Some(stack) => unsafe { stack.as_mut() }.take_or_new()?,
            None => DefaultStack::new(DEFAULT_STACK_SIZE).map_err(AsyncError::Io)?,
        };
        let body = move |suspend: &Yielder<Resume, Yield>, resume| {
            let future_cx = resume?;
            let mut current =
                CurrentFiber { suspend: NonNull::from(suspend), future_cx, cancelled: false };
            let current = NonNull::from(&mut current);
            let previous = CURRENT.replace(Some(current));
            let _reset = ResetCurrentFiber(previous);
            Ok(func())
        };
        // SAFETY: The coroutine is stored inside `FiberFuture<'a, R>`, which is tied to the
        // borrowed state lifetime and dropped before those borrows can expire.
        let fiber = unsafe { Coroutine::with_stack_unchecked(fiber_stack, body) };
        Ok(Self { fiber: Some(fiber), stack, _marker: PhantomData })
    }

    fn recycle_stack(&mut self) {
        let Some(fiber) = self.fiber.take() else { return };
        debug_assert!(fiber.done());
        let stack = fiber.into_stack();
        if let Some(mut slot) = self.stack {
            unsafe { slot.as_mut() }.put(stack);
        }
    }
}

impl<R> Future for FiberFuture<'_, R> {
    type Output = AsyncResult<R>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let cx = NonNull::from(unsafe { change_context_lifetime(cx) });
        let fiber = this.fiber.as_mut().expect("async EVM fiber polled after completion");
        match fiber.resume(Ok(cx)) {
            CoroutineResult::Return(result) => {
                this.recycle_stack();
                Poll::Ready(result)
            }
            CoroutineResult::Yield(()) => Poll::Pending,
        }
    }
}

impl<R> Drop for FiberFuture<'_, R> {
    fn drop(&mut self) {
        let Some(fiber) = self.fiber.as_mut() else {
            return;
        };
        if fiber.done() {
            self.recycle_stack();
        } else if matches!(fiber.resume(Err(AsyncError::Cancelled)), CoroutineResult::Yield(())) {
            // SAFETY: Cancellation already gave the coroutine a chance to return normally. If it
            // yields again, the stack is no longer useful to this future.
            unsafe { fiber.force_reset() };
        } else {
            self.recycle_stack();
        }
    }
}

/// Polls `future` to completion from inside an async EVM fiber.
///
/// If `future` returns `Poll::Pending`, the current EVM fiber is suspended and the outer
/// async EVM future returns `Poll::Pending`. When the executor wakes and polls the outer future
/// again, the EVM fiber resumes and continues polling `future`.
///
/// # Errors
///
/// Returns [`AsyncError::NotOnFiber`] if called outside async EVM execution, or
/// [`AsyncError::Cancelled`] if the outer async EVM execution was dropped.
pub(crate) fn block_on_current<F: Future>(future: F) -> AsyncResult<F::Output> {
    let mut future = core::pin::pin!(future);
    loop {
        match with_current(|current| {
            if current.is_cancelled() {
                return Err(AsyncError::Cancelled);
            }
            let poll = future.as_mut().poll(current.context());
            if poll.is_pending() {
                current.suspend()?;
            }
            Ok(poll)
        })? {
            Poll::Ready(value) => return Ok(value),
            Poll::Pending => {}
        }
    }
}

fn current_tokio_handle() -> Option<Handle> {
    match Handle::try_current() {
        Ok(handle) => match handle.runtime_flavor() {
            tokio::runtime::RuntimeFlavor::CurrentThread => None,
            _ => Some(handle),
        },
        Err(_) => None,
    }
}

fn block_on_handle<F>(handle: &Handle, future: F) -> F::Output
where
    F: Future,
{
    let should_use_block_in_place = Handle::try_current()
        .ok()
        .map(|current| {
            !matches!(current.runtime_flavor(), tokio::runtime::RuntimeFlavor::CurrentThread)
        })
        .unwrap_or(false);

    if should_use_block_in_place {
        task::block_in_place(move || handle.block_on(future))
    } else {
        handle.block_on(future)
    }
}

fn block_on_runtime<F>(runtime: Option<&Handle>, future: F) -> AsyncResult<F::Output>
where
    F: Future,
{
    if CURRENT.get().is_some() {
        return block_on_current(future);
    }

    if let Some(runtime) = runtime {
        return Ok(block_on_handle(runtime, future));
    }

    Err(AsyncError::Runtime)
}

fn block_on_runtime_result<F, T, E>(runtime: Option<&Handle>, future: F) -> AsyncResult<T, E>
where
    F: Future<Output = Result<T, E>>,
{
    match block_on_runtime(runtime, future).map_err(AsyncError::with_inner_error)? {
        Ok(value) => Ok(value),
        Err(error) => Err(AsyncError::Inner(error)),
    }
}

fn with_current<R>(f: impl FnOnce(&mut CurrentFiber) -> AsyncResult<R>) -> AsyncResult<R> {
    let mut current = CURRENT.get().ok_or(AsyncError::NotOnFiber)?;
    f(unsafe { current.as_mut() })
}

unsafe fn change_context_lifetime<'a>(cx: &'a mut Context<'_>) -> &'a mut Context<'static> {
    unsafe { core::mem::transmute::<&'a mut Context<'_>, &'a mut Context<'static>>(cx) }
}

unsafe fn restore_context_lifetime<'a>(cx: &'a mut Context<'static>) -> &'a mut Context<'a> {
    unsafe { core::mem::transmute::<&'a mut Context<'static>, &'a mut Context<'a>>(cx) }
}

/// Asynchronous backing database implementation.
///
/// To take advantage of yielding host I/O, this must be wrapped in [`AsyncDb`] and used with
/// async EVM entrypoints such as [`crate::Evm::transact_async`]. Calling synchronous EVM
/// entrypoints with an [`AsyncDb`] can still execute the futures by blocking on Tokio, but it
/// cannot suspend the EVM fiber and yield back to the caller.
pub trait AsyncDatabase: Any {
    /// Database error type.
    type Error: Error + Send + 'static;

    /// Loads account information.
    fn get_account(
        &mut self,
        address: Address,
    ) -> impl Future<Output = Result<Option<AccountInfo>, Self::Error>> + Send + '_;

    /// Loads bytecode by code hash.
    fn get_code_by_hash(
        &mut self,
        code_hash: B256,
    ) -> impl Future<Output = Result<Bytecode, Self::Error>> + Send + '_;

    /// Loads a persistent storage slot.
    fn get_storage(
        &mut self,
        address: Address,
        key: Word,
    ) -> impl Future<Output = Result<Word, Self::Error>> + Send + '_;

    /// Loads a historical block hash.
    fn get_block_hash(
        &mut self,
        number: Word,
    ) -> impl Future<Output = Result<Option<B256>, Self::Error>> + Send + '_;
}

/// Adapter that exposes an [`AsyncDatabase`] through the synchronous [`DynDatabase`] interface.
pub struct AsyncDb<D: AsyncDatabase> {
    db: D,
    error: Option<Box<dyn Error + Send>>,
    runtime: Option<Handle>,
}

impl<D: AsyncDatabase> AsyncDb<D> {
    /// Creates a new async database adapter.
    ///
    /// This captures the current Tokio runtime handle when one is available.
    #[inline]
    pub fn new(db: D) -> Self {
        Self { db, error: None, runtime: current_tokio_handle() }
    }

    /// Creates a new async database adapter using the current Tokio runtime handle.
    ///
    /// Returns `None` if no Tokio runtime is available or the current runtime is current-threaded.
    #[inline]
    pub fn blocking(db: D) -> Option<Self> {
        Some(Self { db, error: None, runtime: Some(current_tokio_handle()?) })
    }

    /// Creates a new async database adapter with a Tokio runtime handle.
    #[inline]
    pub fn with_tokio_handle(db: D, handle: Handle) -> Self {
        Self { db, error: None, runtime: Some(handle) }
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

    /// Consumes the adapter and returns the wrapped database.
    #[inline]
    pub fn into_inner(self) -> D {
        self.db
    }

    /// Takes the stored database or async execution error.
    #[inline]
    pub fn take_error(&mut self) -> Option<Box<dyn Error + Send>> {
        self.error.take()
    }

    #[inline]
    fn store_error(&mut self, error: impl Error + Send + 'static) -> DbErrorCode {
        self.error = Some(Box::new(error));
        stored_error_code()
    }

    #[inline]
    fn database_result<T>(&mut self, result: AsyncResult<T, D::Error>) -> DbResult<T> {
        result.map_err(|error| match error {
            AsyncError::Inner(error) => self.store_error(error),
            error => self.store_error(error),
        })
    }
}

impl<D: AsyncDatabase + DatabaseCommit> DatabaseCommit for AsyncDb<D> {
    #[inline]
    fn commit(&mut self, changes: &StateChanges) {
        self.db.commit(changes);
    }
}

impl<D: AsyncDatabase> DynDatabase for AsyncDb<D> {
    #[inline]
    fn get_account(&mut self, address: &Address) -> DbResult<Option<AccountInfo>> {
        let result = {
            let Self { db, runtime, .. } = self;
            block_on_runtime_result(runtime.as_ref(), db.get_account(*address))
        };
        self.database_result(result)
    }

    #[inline]
    fn get_code_by_hash(&mut self, code_hash: &B256) -> DbResult<Bytecode> {
        let result = {
            let Self { db, runtime, .. } = self;
            block_on_runtime_result(runtime.as_ref(), db.get_code_by_hash(*code_hash))
        };
        self.database_result(result)
    }

    #[inline]
    fn get_storage(&mut self, address: &Address, key: &Word) -> DbResult<Word> {
        let result = {
            let Self { db, runtime, .. } = self;
            block_on_runtime_result(runtime.as_ref(), db.get_storage(*address, *key))
        };
        self.database_result(result)
    }

    #[inline]
    fn get_block_hash(&mut self, number: &Word) -> DbResult<Option<B256>> {
        let result = {
            let Self { db, runtime, .. } = self;
            block_on_runtime_result(runtime.as_ref(), db.get_block_hash(*number))
        };
        self.database_result(result)
    }

    #[inline]
    fn error(&mut self, code: DbErrorCode) -> Box<dyn Error> {
        if code == stored_error_code()
            && let Some(error) = self.error.take()
        {
            return error;
        }
        db_error_unavailable(code)
    }

    #[inline]
    fn into_any(self: Box<Self>) -> Box<dyn core::any::Any> {
        self
    }
}

impl<D: AsyncDatabase + fmt::Debug> fmt::Debug for AsyncDb<D> {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AsyncDb").field("db", &self.db).finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::{AsyncDatabase, AsyncDb, AsyncError, block_on_current, on_fiber};
    use crate::{
        BaseEvmTypes, Evm, PrecompileError, Precompiles, SpecId, TxResult,
        bytecode::Bytecode,
        env::BlockEnv,
        evm::{Database, Db, DynDatabase, InMemoryDB, PrecompileProvider},
        interpreter::{GasTracker, Message, Word},
        precompile::PrecompileOutput,
        registry::{HandlerError, HandlerResult, TxRegistry, TxRequest},
    };
    use alloy_consensus::{TxLegacy, transaction::Recovered};
    use alloy_primitives::{Address, B256, Bytes};
    use core::{
        assert_matches, convert::Infallible, fmt, future::Future, pin::Pin, ptr::NonNull,
        task::Poll,
    };
    use corosensei::stack::Stack;
    use std::{
        error::Error,
        rc::Rc,
        task::{Context, Waker},
    };

    #[test]
    fn block_on_requires_fiber() {
        assert_matches!(block_on_current(core::future::ready(())), Err(AsyncError::NotOnFiber));
    }

    #[test]
    fn fiber_suspends_and_resumes_pending_future() {
        let mut state = 1;
        let mut future = core::pin::pin!(on_fiber(|| {
            state += block_on_current(PendingOnce { pending: true }).unwrap();
            state
        }));
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);

        assert_matches!(future.as_mut().poll(&mut cx), Poll::Pending);
        assert_matches!(future.as_mut().poll(&mut cx), Poll::Ready(Ok(3)));
    }

    #[test]
    fn fiber_reuses_stack_slot() {
        let mut stack = super::FiberStack::default();
        let stack_ptr = NonNull::from(&mut stack);

        let first = poll_ready(unsafe {
            super::on_fiber_result_with_stack(stack_ptr, || Ok::<_, Infallible>(1))
        })
        .unwrap();
        let first_base = stack.stack.as_ref().unwrap().base();
        let second = poll_ready(unsafe {
            super::on_fiber_result_with_stack(stack_ptr, || Ok::<_, Infallible>(2))
        })
        .unwrap();
        let second_base = stack.stack.as_ref().unwrap().base();

        assert_eq!(first, 1);
        assert_eq!(second, 2);
        assert_eq!(first_base, second_base);
    }

    #[test]
    fn async_database_adapts_to_dyn_database() {
        let mut db = AsyncDb::new(TestDb);
        let address = Address::ZERO;
        let key = Word::from(7);
        let mut future = core::pin::pin!(on_fiber(|| {
            DynDatabase::get_storage(&mut db, &address, &key).unwrap()
        }));
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);

        assert_matches!(
            future.as_mut().poll(&mut cx),
            Poll::Ready(Ok(value)) if value == Word::from(9),
        );
    }

    #[test]
    fn async_database_suspends_until_ready() {
        let mut db = AsyncDb::new(PendingDb { pending: true });
        let address = Address::ZERO;
        let key = Word::from(7);
        let mut future = core::pin::pin!(on_fiber(|| {
            DynDatabase::get_storage(&mut db, &address, &key).unwrap()
        }));
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);

        assert_matches!(future.as_mut().poll(&mut cx), Poll::Pending);
        assert_matches!(
            future.as_mut().poll(&mut cx),
            Poll::Ready(Ok(value)) if value == Word::from(9),
        );
    }

    #[test]
    fn async_database_stores_database_error() {
        let mut db = AsyncDb::new(FailingDb);
        let address = Address::ZERO;
        let key = Word::from(7);
        let code = on_fiber(|| DynDatabase::get_storage(&mut db, &address, &key).unwrap_err());
        let code = poll_ready(code).unwrap();

        assert_eq!(db.error(code).to_string(), "storage read failed");
    }

    #[test]
    fn async_database_inner_futures_are_send() {
        let mut db = TestDb;

        drop(assert_send(db.get_account(Address::ZERO)));
        drop(assert_send(db.get_code_by_hash(B256::ZERO)));
        drop(assert_send(db.get_storage(Address::ZERO, Word::ZERO)));
        drop(assert_send(db.get_block_hash(Word::ZERO)));
    }

    #[test]
    fn dispatches_transaction_async_by_typed_2718_type() {
        let registry = TxRegistry::new().with_handler(
            TEST_TX_TYPE,
            crate::ethereum::RecoveredTxEnvelope::as_legacy,
            handle_test_tx,
        );
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnv::default(),
            registry,
            InMemoryDB::default(),
            Precompiles::base(SpecId::OSAKA),
        );
        evm.evm_is_send::<InMemoryDB, Precompiles<BaseEvmTypes>>();
        let tx = test_tx(41);

        let result = poll_ready(evm.transact_async(&tx)).unwrap();

        assert_eq!(result.gas_used, 42);
    }

    #[test]
    fn transaction_async_future_is_send_with_send_erased_fields() {
        let registry = TxRegistry::new().with_handler(
            TEST_TX_TYPE,
            crate::ethereum::RecoveredTxEnvelope::as_legacy,
            handle_test_tx,
        );
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnv::default(),
            registry,
            InMemoryDB::default(),
            Precompiles::base(SpecId::OSAKA),
        );
        evm.evm_is_send::<InMemoryDB, Precompiles<BaseEvmTypes>>();
        let tx = test_tx(41);

        let result = poll_ready(assert_send(evm.transact_async(&tx))).unwrap();

        assert_eq!(result.gas_used, 42);
    }

    #[test]
    fn transaction_async_future_is_send_after_type_check() {
        let registry = TxRegistry::new().with_handler(
            TEST_TX_TYPE,
            crate::ethereum::RecoveredTxEnvelope::as_legacy,
            handle_test_tx,
        );
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnv::default(),
            registry,
            InMemoryDB::default(),
            Precompiles::base(SpecId::OSAKA),
        );
        evm.set_inspector(SendInspector);
        evm.evm_is_send_with_inspector::<InMemoryDB, Precompiles<BaseEvmTypes>, SendInspector>();
        let tx = test_tx(41);

        let result = poll_ready(assert_send(evm.transact_async(&tx))).unwrap();

        assert_eq!(result.gas_used, 42);
    }

    #[test]
    #[should_panic = "async EVM execution requires EVM erased fields to be verified as Send with Evm::evm_is_send"]
    fn transaction_async_panics_with_non_send_erased_fields() {
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnv::default(),
            TxRegistry::new(),
            InMemoryDB::default(),
            Precompiles::base(SpecId::OSAKA),
        );
        let tx = test_tx(41);

        drop(evm.transact_async(&tx));
    }

    #[test]
    #[should_panic = "async EVM execution requires EVM erased fields to be verified as Send with Evm::evm_is_send"]
    fn transaction_async_panics_after_non_send_setter() {
        let marker = Rc::new(());
        let registry = TxRegistry::new().with_handler(
            TEST_TX_TYPE,
            crate::ethereum::RecoveredTxEnvelope::as_legacy,
            handle_test_tx,
        );
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnv::default(),
            registry,
            InMemoryDB::default(),
            Precompiles::base(SpecId::OSAKA),
        );
        evm.evm_is_send::<InMemoryDB, Precompiles<BaseEvmTypes>>();
        evm.set_inspector(NonSendInspector { marker });
        let tx = test_tx(41);

        drop(evm.transact_async(&tx));
    }

    #[test]
    fn evm_accepts_non_send_erased_fields() {
        let marker = Rc::new(());
        let database = Db::new(NonSendDb { marker: Rc::clone(&marker) });
        let precompiles = NonSendPrecompiles { marker: Rc::clone(&marker) };
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnv::default(),
            TxRegistry::new(),
            database,
            precompiles,
        );

        evm.set_inspector(NonSendInspector { marker });

        let address = Address::ZERO;
        let key = Word::from(7);
        let value = evm.database_mut().get_storage(&address, &key).unwrap();

        assert_eq!(value, Word::from(9));
    }

    #[test]
    fn transaction_async_flattens_handler_error() {
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnv::default(),
            TxRegistry::new(),
            InMemoryDB::default(),
            Precompiles::base(SpecId::OSAKA),
        );
        evm.evm_is_send::<InMemoryDB, Precompiles<BaseEvmTypes>>();
        let tx = test_tx(41);

        let result = poll_ready(evm.transact_async(&tx));

        assert_matches!(
            result,
            Err(AsyncError::Inner(HandlerError::UnsupportedTransactionType(TEST_TX_TYPE)))
        );
    }

    #[test]
    fn synchronous_database_blocks_with_current_tokio_runtime() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _guard = runtime.enter();
        let mut db = AsyncDb::new(TokioDb);
        let address = Address::ZERO;
        let key = Word::from(7);

        let value = DynDatabase::get_storage(&mut db, &address, &key).unwrap();

        assert_eq!(value, Word::from(9));
    }

    #[test]
    fn blocking_constructor_uses_current_tokio_runtime() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _guard = runtime.enter();
        let mut db = AsyncDb::blocking(TokioDb).unwrap();
        let address = Address::ZERO;
        let key = Word::from(7);

        let value = DynDatabase::get_storage(&mut db, &address, &key).unwrap();

        assert_eq!(value, Word::from(9));
    }

    #[test]
    fn synchronous_database_blocks_with_stored_tokio_handle() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let mut db = AsyncDb::with_tokio_handle(TokioDb, runtime.handle().clone());
        let address = Address::ZERO;
        let key = Word::from(7);

        let value = DynDatabase::get_storage(&mut db, &address, &key).unwrap();

        assert_eq!(value, Word::from(9));
    }

    #[test]
    fn synchronous_database_requires_runtime_handle() {
        let mut db = AsyncDb::new(TestDb);
        let address = Address::ZERO;
        let key = Word::from(7);
        let code = DynDatabase::get_storage(&mut db, &address, &key).unwrap_err();

        assert_eq!(
            db.error(code).to_string(),
            "async host operation requires a Tokio multi-thread runtime"
        );
    }

    #[test]
    fn synchronous_evm_database_blocks_with_current_tokio_runtime() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _guard = runtime.enter();
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnv::default(),
            TxRegistry::new(),
            AsyncDb::new(TokioDb),
            Precompiles::base(SpecId::OSAKA),
        );
        evm.evm_is_send::<AsyncDb<TokioDb>, Precompiles<BaseEvmTypes>>();
        let address = Address::ZERO;
        let key = Word::from(7);

        let value = evm.database_mut().get_storage(&address, &key).unwrap();

        assert_eq!(value, Word::from(9));
    }

    #[test]
    fn system_call_async_to_missing_code_is_noop() {
        let contract = Address::from([0x42; 20]);
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnv::default(),
            TxRegistry::new(),
            InMemoryDB::default(),
            Precompiles::base(SpecId::OSAKA),
        );
        evm.evm_is_send::<InMemoryDB, Precompiles<BaseEvmTypes>>();

        let result = poll_ready(evm.system_call_async(contract, Bytes::new())).unwrap();

        assert!(result.status);
        assert_eq!(result.gas_used, 0);
        assert!(result.state_changes.is_empty());
    }

    #[test]
    fn dropping_fiber_cancels_blocked_future() {
        let mut saw_cancel = false;
        {
            let mut future = core::pin::pin!(on_fiber(|| {
                saw_cancel = matches!(block_on_current(PendingForever), Err(AsyncError::Cancelled));
            }));
            let waker = Waker::noop();
            let mut cx = Context::from_waker(waker);

            assert_matches!(future.as_mut().poll(&mut cx), Poll::Pending);
        }
        assert!(saw_cancel);
    }

    const TEST_TX_TYPE: u8 = 0x00;

    fn test_tx(value: u64) -> crate::ethereum::RecoveredTxEnvelope {
        crate::ethereum::RecoveredTxEnvelope::Legacy(Recovered::new_unchecked(
            TxLegacy { nonce: value, ..TxLegacy::default() },
            Address::ZERO,
        ))
    }

    fn handle_test_tx(
        req: TxRequest<'_, BaseEvmTypes, Recovered<TxLegacy>>,
    ) -> HandlerResult<TxResult> {
        let _ = req.host.spec_id();
        Ok(TxResult { status: true, gas_used: req.tx.nonce + 1, ..TxResult::default() })
    }

    fn poll_ready<F: Future + Send>(future: F) -> F::Output {
        let mut future = core::pin::pin!(future);
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);

        match future.as_mut().poll(&mut cx) {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("future unexpectedly pending"),
        }
    }

    fn assert_send<F: Future + Send>(future: F) -> F {
        future
    }

    struct NonSendDb {
        marker: Rc<()>,
    }

    impl Database for NonSendDb {
        type Error = Infallible;

        fn get_account(
            &mut self,
            _address: &Address,
        ) -> Result<Option<crate::evm::AccountInfo>, Self::Error> {
            let _ = Rc::strong_count(&self.marker);
            Ok(None)
        }

        fn get_code_by_hash(&mut self, _code_hash: &B256) -> Result<Bytecode, Self::Error> {
            Ok(Bytecode::default())
        }

        fn get_storage(&mut self, _address: &Address, _key: &Word) -> Result<Word, Self::Error> {
            let _ = Rc::strong_count(&self.marker);
            Ok(Word::from(9))
        }

        fn get_block_hash(&mut self, _number: &Word) -> Result<Option<B256>, Self::Error> {
            Ok(None)
        }
    }

    struct NonSendPrecompiles {
        marker: Rc<()>,
    }

    impl PrecompileProvider<BaseEvmTypes> for NonSendPrecompiles {
        fn contains(&self, _address: &Address) -> bool {
            let _ = Rc::strong_count(&self.marker);
            false
        }

        fn execute(
            &mut self,
            _evm: &mut Evm<BaseEvmTypes>,
            _message: &Message<BaseEvmTypes>,
            _gas: &mut GasTracker,
        ) -> Option<Result<PrecompileOutput, PrecompileError>> {
            None
        }
    }

    struct NonSendInspector {
        marker: Rc<()>,
    }

    impl crate::evm::Inspector<BaseEvmTypes> for NonSendInspector {
        fn step(&mut self, _interp: &mut crate::interpreter::Interpreter<'_, BaseEvmTypes>) {
            let _ = Rc::strong_count(&self.marker);
        }
    }

    struct SendInspector;

    impl crate::evm::Inspector<BaseEvmTypes> for SendInspector {}

    struct PendingOnce {
        pending: bool,
    }

    impl Future for PendingOnce {
        type Output = i32;

        fn poll(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
            if self.pending {
                self.pending = false;
                Poll::Pending
            } else {
                Poll::Ready(2)
            }
        }
    }

    struct PendingForever;

    impl Future for PendingForever {
        type Output = ();

        fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
            Poll::Pending
        }
    }

    struct TestDb;

    impl AsyncDatabase for TestDb {
        type Error = Infallible;

        async fn get_account(
            &mut self,
            _address: Address,
        ) -> Result<Option<crate::evm::AccountInfo>, Self::Error> {
            Ok(None)
        }

        async fn get_code_by_hash(&mut self, _code_hash: B256) -> Result<Bytecode, Self::Error> {
            Ok(Bytecode::default())
        }

        async fn get_storage(
            &mut self,
            _address: Address,
            _key: Word,
        ) -> Result<Word, Self::Error> {
            Ok(Word::from(9))
        }

        async fn get_block_hash(&mut self, _number: Word) -> Result<Option<B256>, Self::Error> {
            Ok(None)
        }
    }

    struct PendingDb {
        pending: bool,
    }

    impl AsyncDatabase for PendingDb {
        type Error = Infallible;

        async fn get_account(
            &mut self,
            _address: Address,
        ) -> Result<Option<crate::evm::AccountInfo>, Self::Error> {
            Ok(None)
        }

        async fn get_code_by_hash(&mut self, _code_hash: B256) -> Result<Bytecode, Self::Error> {
            Ok(Bytecode::default())
        }

        async fn get_storage(
            &mut self,
            _address: Address,
            _key: Word,
        ) -> Result<Word, Self::Error> {
            PendingStorage { pending: &mut self.pending }.await;
            Ok(Word::from(9))
        }

        async fn get_block_hash(&mut self, _number: Word) -> Result<Option<B256>, Self::Error> {
            Ok(None)
        }
    }

    struct PendingStorage<'a> {
        pending: &'a mut bool,
    }

    impl Future for PendingStorage<'_> {
        type Output = ();

        fn poll(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
            if *self.pending {
                *self.pending = false;
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        }
    }

    struct FailingDb;

    impl AsyncDatabase for FailingDb {
        type Error = TestError;

        async fn get_account(
            &mut self,
            _address: Address,
        ) -> Result<Option<crate::evm::AccountInfo>, Self::Error> {
            Ok(None)
        }

        async fn get_code_by_hash(&mut self, _code_hash: B256) -> Result<Bytecode, Self::Error> {
            Ok(Bytecode::default())
        }

        async fn get_storage(
            &mut self,
            _address: Address,
            _key: Word,
        ) -> Result<Word, Self::Error> {
            Err(TestError)
        }

        async fn get_block_hash(&mut self, _number: Word) -> Result<Option<B256>, Self::Error> {
            Ok(None)
        }
    }

    struct TokioDb;

    impl AsyncDatabase for TokioDb {
        type Error = Infallible;

        async fn get_account(
            &mut self,
            _address: Address,
        ) -> Result<Option<crate::evm::AccountInfo>, Self::Error> {
            tokio::task::yield_now().await;
            Ok(None)
        }

        async fn get_code_by_hash(&mut self, _code_hash: B256) -> Result<Bytecode, Self::Error> {
            tokio::task::yield_now().await;
            Ok(Bytecode::default())
        }

        async fn get_storage(
            &mut self,
            _address: Address,
            _key: Word,
        ) -> Result<Word, Self::Error> {
            tokio::task::yield_now().await;
            Ok(Word::from(9))
        }

        async fn get_block_hash(&mut self, _number: Word) -> Result<Option<B256>, Self::Error> {
            tokio::task::yield_now().await;
            Ok(None)
        }
    }

    #[derive(Clone, Copy, Debug)]
    struct TestError;

    impl fmt::Display for TestError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("storage read failed")
        }
    }

    impl Error for TestError {}
}
