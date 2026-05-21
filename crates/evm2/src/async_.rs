//! Async execution support for synchronous EVM hosts.
//!
//! This module runs the synchronous EVM on a native fiber. Synchronous host methods can then use
//! [`block_on_current`] to poll an async operation; if that operation is pending, the fiber is
//! suspended and the outer async task returns `Poll::Pending`.

use crate::{
    IoMode,
    bytecode::Bytecode,
    evm::{AccountInfo, DatabaseCommit, DbErrorCode, DbResult, DynDatabase, StateChanges},
    interpreter::Word,
};
use alloc::boxed::Box;
use alloy_primitives::{Address, B256};
use core::{
    any::Any, fmt, future::Future, marker::PhantomData, pin::Pin, ptr::NonNull, task::Poll,
};
use corosensei::{Coroutine, CoroutineResult, Yielder, stack::DefaultStack};
use std::{cell::Cell, error::Error, io, task::Context};
use tokio::{
    runtime::{Handle, Runtime},
    task,
};

type Resume = AsyncResult<NonNull<Context<'static>>>;
type Yield = ();
type Complete<R> = AsyncResult<R>;
type EvmFiber<R> = Coroutine<Resume, Yield, Complete<R>, DefaultStack>;

thread_local! {
    static CURRENT: Cell<Option<NonNull<CurrentFiber>>> = const { Cell::new(None) };
}

/// Result type used by async EVM execution helpers.
pub type AsyncResult<T, E = core::convert::Infallible> = core::result::Result<T, AsyncError<E>>;

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
pub(crate) fn on_fiber_result<'a, R, E>(
    stack_size: usize,
    func: impl FnOnce() -> core::result::Result<R, E> + 'a,
) -> impl Future<Output = AsyncResult<R, E>> + Send + 'a
where
    R: Send + 'a,
    E: Send + 'a,
{
    OnFiber::new(stack_size, func)
}

pub(crate) fn on_fiber<'a, R>(
    stack_size: usize,
    func: impl FnOnce() -> R + 'a,
) -> impl Future<Output = AsyncResult<R>> + Send + 'a
where
    R: Send + 'a,
{
    on_fiber_result(stack_size, move || Ok::<_, core::convert::Infallible>(func()))
}

enum OnFiber<'a, R, E> {
    Running(FiberFuture<'a, core::result::Result<R, E>>),
    Error(Option<AsyncError>),
    Done,
}

impl<'a, R, E> OnFiber<'a, R, E> {
    fn new(stack_size: usize, func: impl FnOnce() -> core::result::Result<R, E> + 'a) -> Self {
        match FiberFuture::new(stack_size, func) {
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
    fiber: EvmFiber<R>,
    _marker: PhantomData<&'a ()>,
}

// SAFETY: The future may move between polls, but the coroutine stack itself is heap allocated and
// is only resumed through `poll` with a fresh task context. Values that can remain on the coroutine
// stack across suspension are required to be `Send` by the blocking boundary.
unsafe impl<R: Send> Send for FiberFuture<'_, R> {}

impl<'a, R> FiberFuture<'a, R> {
    fn new(stack_size: usize, func: impl FnOnce() -> R + 'a) -> AsyncResult<Self> {
        let stack = DefaultStack::new(stack_size).map_err(AsyncError::Io)?;
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
        let fiber = unsafe { Coroutine::with_stack_unchecked(stack, body) };
        Ok(Self { fiber, _marker: PhantomData })
    }
}

impl<R> Future for FiberFuture<'_, R> {
    type Output = AsyncResult<R>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let cx = NonNull::from(unsafe { change_context_lifetime(cx) });
        match this.fiber.resume(Ok(cx)) {
            CoroutineResult::Return(result) => Poll::Ready(result),
            CoroutineResult::Yield(()) => Poll::Pending,
        }
    }
}

impl<R> Drop for FiberFuture<'_, R> {
    fn drop(&mut self) {
        if self.fiber.done() {
            return;
        }
        if matches!(self.fiber.resume(Err(AsyncError::Cancelled)), CoroutineResult::Yield(())) {
            // SAFETY: Cancellation already gave the coroutine a chance to return normally. If it
            // yields again, the stack is no longer useful to this future.
            unsafe { self.fiber.force_reset() };
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

#[derive(Debug)]
enum HandleOrRuntime {
    Handle(Handle),
    Runtime(Runtime),
}

impl HandleOrRuntime {
    #[inline]
    fn current() -> Option<Self> {
        match Handle::try_current() {
            Ok(handle) => match handle.runtime_flavor() {
                tokio::runtime::RuntimeFlavor::CurrentThread => None,
                _ => Some(Self::Handle(handle)),
            },
            Err(_) => None,
        }
    }

    #[inline]
    fn block_on<F>(&self, future: F) -> F::Output
    where
        F: Future + Send,
        F::Output: Send,
    {
        match self {
            Self::Handle(handle) => {
                let should_use_block_in_place = Handle::try_current()
                    .ok()
                    .map(|current| {
                        !matches!(
                            current.runtime_flavor(),
                            tokio::runtime::RuntimeFlavor::CurrentThread
                        )
                    })
                    .unwrap_or(false);

                if should_use_block_in_place {
                    task::block_in_place(move || handle.block_on(future))
                } else {
                    handle.block_on(future)
                }
            }
            Self::Runtime(runtime) => runtime.block_on(future),
        }
    }
}

fn block_on_runtime<F>(
    mode: IoMode,
    runtime: Option<&HandleOrRuntime>,
    future: F,
) -> AsyncResult<F::Output>
where
    F: Future + Send,
    F::Output: Send,
{
    match mode {
        IoMode::Blocking => {
            let Some(runtime) = runtime else {
                return Err(AsyncError::Runtime);
            };
            Ok(runtime.block_on(future))
        }
        IoMode::Async => block_on_current(future),
    }
}

fn block_on_runtime_result<F, T, E>(
    mode: IoMode,
    runtime: Option<&HandleOrRuntime>,
    future: F,
) -> AsyncResult<T, E>
where
    F: Future<Output = core::result::Result<T, E>> + Send,
    T: Send,
    E: Send,
{
    match block_on_runtime(mode, runtime, future).map_err(AsyncError::with_inner_error)? {
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
pub trait AsyncDatabase: Any + Send {
    /// Database error type.
    type Error: Error + Send + 'static;

    /// Loads account information.
    fn get_account(
        &mut self,
        address: Address,
    ) -> impl Future<Output = core::result::Result<Option<AccountInfo>, Self::Error>> + Send + '_;

    /// Loads bytecode by code hash.
    fn get_code_by_hash(
        &mut self,
        code_hash: B256,
    ) -> impl Future<Output = core::result::Result<Bytecode, Self::Error>> + Send + '_;

    /// Loads a persistent storage slot.
    fn get_storage(
        &mut self,
        address: Address,
        key: Word,
    ) -> impl Future<Output = core::result::Result<Word, Self::Error>> + Send + '_;

    /// Loads a historical block hash.
    fn get_block_hash(
        &mut self,
        number: Word,
    ) -> impl Future<Output = core::result::Result<Option<B256>, Self::Error>> + Send + '_;
}

/// Adapter that exposes an [`AsyncDatabase`] through the synchronous [`DynDatabase`] interface.
pub struct AsyncDb<D: AsyncDatabase> {
    db: D,
    error: Option<Box<dyn Error + Send>>,
    io_mode: IoMode,
    runtime: Option<HandleOrRuntime>,
}

impl<D: AsyncDatabase> AsyncDb<D> {
    /// Creates a new async database adapter.
    #[inline]
    pub const fn new(db: D) -> Self {
        Self { db, error: None, io_mode: IoMode::Async, runtime: None }
    }

    /// Creates a new blocking async database adapter using the current Tokio runtime handle.
    ///
    /// Returns `None` if no Tokio runtime is available or the current runtime is current-threaded.
    #[inline]
    pub fn blocking(db: D) -> Option<Self> {
        Some(Self {
            db,
            error: None,
            io_mode: IoMode::Blocking,
            runtime: Some(HandleOrRuntime::current()?),
        })
    }

    /// Creates a new blocking async database adapter with a Tokio runtime.
    #[inline]
    pub const fn with_runtime(db: D, runtime: Runtime) -> Self {
        Self {
            db,
            error: None,
            io_mode: IoMode::Blocking,
            runtime: Some(HandleOrRuntime::Runtime(runtime)),
        }
    }

    /// Creates a new blocking async database adapter with a Tokio runtime handle.
    #[inline]
    pub const fn with_handle(db: D, handle: Handle) -> Self {
        Self {
            db,
            error: None,
            io_mode: IoMode::Blocking,
            runtime: Some(HandleOrRuntime::Handle(handle)),
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
            let Self { db, io_mode, runtime, .. } = self;
            block_on_runtime_result(*io_mode, runtime.as_ref(), db.get_account(*address))
        };
        self.database_result(result)
    }

    #[inline]
    fn get_code_by_hash(&mut self, code_hash: &B256) -> DbResult<Bytecode> {
        let result = {
            let Self { db, io_mode, runtime, .. } = self;
            block_on_runtime_result(*io_mode, runtime.as_ref(), db.get_code_by_hash(*code_hash))
        };
        self.database_result(result)
    }

    #[inline]
    fn get_storage(&mut self, address: &Address, key: &Word) -> DbResult<Word> {
        let result = {
            let Self { db, io_mode, runtime, .. } = self;
            block_on_runtime_result(*io_mode, runtime.as_ref(), db.get_storage(*address, *key))
        };
        self.database_result(result)
    }

    #[inline]
    fn get_block_hash(&mut self, number: &Word) -> DbResult<Option<B256>> {
        let result = {
            let Self { db, io_mode, runtime, .. } = self;
            block_on_runtime_result(*io_mode, runtime.as_ref(), db.get_block_hash(*number))
        };
        self.database_result(result)
    }

    #[inline]
    fn error(&mut self, code: DbErrorCode) -> Box<dyn Error + Send> {
        if code == stored_error_code()
            && let Some(error) = self.error.take()
        {
            return error;
        }
        Box::new(AsyncDbErrorUnavailable(code))
    }

    #[inline]
    fn set_io_mode(&mut self, io_mode: IoMode) -> bool {
        if matches!(io_mode, IoMode::Blocking)
            && self.runtime.is_none()
            && let Some(runtime) = HandleOrRuntime::current()
        {
            self.runtime = Some(runtime);
        }
        if matches!(io_mode, IoMode::Blocking) && self.runtime.is_none() {
            return false;
        }
        self.io_mode = io_mode;
        true
    }
}

impl<D: AsyncDatabase + fmt::Debug> fmt::Debug for AsyncDb<D> {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AsyncDb").field("db", &self.db).finish_non_exhaustive()
    }
}

#[inline]
fn stored_error_code() -> DbErrorCode {
    match DbErrorCode::new(1) {
        Some(code) => code,
        None => unreachable!("stored database error code is non-zero"),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AsyncDbErrorUnavailable(DbErrorCode);

impl fmt::Display for AsyncDbErrorUnavailable {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "async database error {:?} is unavailable", self.0)
    }
}

impl Error for AsyncDbErrorUnavailable {}

#[cfg(test)]
mod tests {
    use super::{AsyncDatabase, AsyncDb, AsyncError, block_on_current, on_fiber};
    use crate::{
        BaseEvmTypes, Evm, ExecutionConfig, IoMode, Precompiles, SpecId, TxResult, Version,
        bytecode::Bytecode,
        env::BlockEnv,
        evm::{DynDatabase, InMemoryDB},
        interpreter::Word,
        registry::{HandlerError, HandlerResult, TxRegistry, TxRequest},
    };
    use alloy_consensus::{TxLegacy, transaction::Recovered};
    use alloy_primitives::{Address, B256, Bytes};
    use core::{convert::Infallible, fmt, future::Future, pin::Pin, task::Poll};
    use std::{
        error::Error,
        task::{Context, Waker},
    };

    #[test]
    fn block_on_requires_fiber() {
        assert!(matches!(block_on_current(core::future::ready(())), Err(AsyncError::NotOnFiber)));
    }

    #[test]
    fn fiber_suspends_and_resumes_pending_future() {
        let mut state = 1;
        let mut future = core::pin::pin!(on_fiber(stack_size(), || {
            state += block_on_current(PendingOnce { pending: true }).unwrap();
            state
        }));
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);

        assert!(matches!(future.as_mut().poll(&mut cx), Poll::Pending));
        assert!(matches!(future.as_mut().poll(&mut cx), Poll::Ready(Ok(3))));
    }

    #[test]
    fn async_database_adapts_to_dyn_database() {
        let mut db = AsyncDb::new(TestDb);
        let address = Address::ZERO;
        let key = Word::from(7);
        let mut future = core::pin::pin!(on_fiber(stack_size(), || {
            DynDatabase::get_storage(&mut db, &address, &key).unwrap()
        }));
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);

        assert!(
            matches!(future.as_mut().poll(&mut cx), Poll::Ready(Ok(value)) if value == Word::from(9))
        );
    }

    #[test]
    fn async_database_suspends_until_ready() {
        let mut db = AsyncDb::new(PendingDb { pending: true });
        let address = Address::ZERO;
        let key = Word::from(7);
        let mut future = core::pin::pin!(on_fiber(stack_size(), || {
            DynDatabase::get_storage(&mut db, &address, &key).unwrap()
        }));
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);

        assert!(matches!(future.as_mut().poll(&mut cx), Poll::Pending));
        assert!(
            matches!(future.as_mut().poll(&mut cx), Poll::Ready(Ok(value)) if value == Word::from(9))
        );
    }

    #[test]
    fn async_database_stores_database_error() {
        let mut db = AsyncDb::new(FailingDb);
        let address = Address::ZERO;
        let key = Word::from(7);
        let code = on_fiber(stack_size(), || {
            DynDatabase::get_storage(&mut db, &address, &key).unwrap_err()
        });
        let code = poll_ready(code).unwrap();

        assert_eq!(db.error(code).to_string(), "storage read failed");
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
        let tx = test_tx(41);

        let result = poll_ready(evm.transact_async(&tx)).unwrap();

        assert_eq!(result.gas_used, 42);
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
        let tx = test_tx(41);

        let result = poll_ready(evm.transact_async(&tx));

        assert!(matches!(
            result,
            Err(AsyncError::Inner(HandlerError::UnsupportedTransactionType(TEST_TX_TYPE)))
        ));
    }

    #[test]
    fn blocking_io_mode_blocks_with_tokio_when_not_on_fiber() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _guard = runtime.enter();
        let mut db = AsyncDb::new(TokioDb);
        let address = Address::ZERO;
        let key = Word::from(7);

        assert!(DynDatabase::set_io_mode(&mut db, IoMode::Blocking));
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
    fn blocking_io_mode_blocks_with_stored_tokio_runtime() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let mut db = AsyncDb::with_runtime(TokioDb, runtime);
        let address = Address::ZERO;
        let key = Word::from(7);

        let value = DynDatabase::get_storage(&mut db, &address, &key).unwrap();

        assert_eq!(value, Word::from(9));
    }

    #[test]
    fn blocking_io_mode_requires_runtime_handle() {
        let mut db = AsyncDb::new(TestDb);

        assert!(!DynDatabase::set_io_mode(&mut db, IoMode::Blocking));
    }

    #[test]
    fn evm_sets_async_database_io_mode() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _guard = runtime.enter();
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnv::default(),
            TxRegistry::new(),
            AsyncDb::new(TokioDb),
            Precompiles::base(SpecId::OSAKA),
        );
        let address = Address::ZERO;
        let key = Word::from(7);

        assert!(evm.set_io_mode(IoMode::Blocking));
        let value = evm.database_mut().get_storage(&address, &key).unwrap();

        assert_eq!(value, Word::from(9));
    }

    #[test]
    fn evm_applies_version_io_mode_to_async_database() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _guard = runtime.enter();
        let mut version = Version::new(SpecId::OSAKA);
        version.io_mode = IoMode::Blocking;
        let mut evm = Evm::<BaseEvmTypes>::new_with_execution_config(
            ExecutionConfig::for_spec_and_version(SpecId::OSAKA, version),
            SpecId::OSAKA,
            BlockEnv::default(),
            TxRegistry::new(),
            AsyncDb::new(TokioDb),
            Precompiles::base(SpecId::OSAKA),
        );
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

        let result = poll_ready(evm.system_call_async(contract, Bytes::new())).unwrap();

        assert!(result.status);
        assert_eq!(result.gas_used, 0);
        assert!(result.state_changes.is_empty());
    }

    #[test]
    fn async_io_mode_requires_fiber() {
        let mut db = AsyncDb::new(TestDb);
        let address = Address::ZERO;
        let key = Word::from(7);
        let code = DynDatabase::get_storage(&mut db, &address, &key).unwrap_err();

        assert_eq!(
            db.error(code).to_string(),
            "async host operation requires EVM async fiber execution"
        );
    }

    #[test]
    fn dropping_fiber_cancels_blocked_future() {
        let mut saw_cancel = false;
        {
            let mut future = core::pin::pin!(on_fiber(stack_size(), || {
                saw_cancel = matches!(block_on_current(PendingForever), Err(AsyncError::Cancelled));
            }));
            let waker = Waker::noop();
            let mut cx = Context::from_waker(waker);

            assert!(matches!(future.as_mut().poll(&mut cx), Poll::Pending));
        }
        assert!(saw_cancel);
    }

    #[test]
    fn version_defaults_async_io_mode_and_stack_size() {
        let version = Version::new(SpecId::OSAKA);

        assert_eq!(version.io_mode, IoMode::Async);
        assert_eq!(version.min_stack_size, 1024 * 1024);
    }

    fn stack_size() -> usize {
        Version::new(SpecId::OSAKA).min_stack_size
    }

    const TEST_TX_TYPE: u8 = 0x00;

    fn test_tx(value: u64) -> crate::ethereum::RecoveredTxEnvelope {
        crate::ethereum::RecoveredTxEnvelope::Legacy(Recovered::new_unchecked(
            TxLegacy { nonce: value, ..TxLegacy::default() },
            Address::ZERO,
        ))
    }

    fn handle_test_tx(
        req: TxRequest<'_, Recovered<TxLegacy>, Evm<BaseEvmTypes>>,
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
