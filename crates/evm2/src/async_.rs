//! Async execution support for synchronous EVM hosts.
//!
//! This module runs the synchronous EVM on a native fiber. Synchronous host methods can then use
//! [`block_on_current`] to poll an async operation; if that operation is pending, the fiber is
//! suspended and the outer async task returns `Poll::Pending`.

use crate::{
    bytecode::Bytecode,
    evm::{AccountInfo, DatabaseCommit, DbErrorCode, DbResult, DynDatabase, StateChanges},
    interpreter::Word,
};
use alloc::{
    boxed::Box,
    rc::Rc,
    string::{String, ToString},
};
use alloy_primitives::{Address, B256};
use core::{
    any::Any, fmt, future::Future, marker::PhantomData, pin::Pin, ptr::NonNull, task::Poll,
};
use std::{cell::RefCell, error::Error, task::Context};
use wasmtime_fiber::{Fiber, FiberStack, Suspend};

/// Default stack size for async EVM execution.
const DEFAULT_ASYNC_STACK_SIZE: usize = 2 * 1024 * 1024;

type Resume = AsyncResult<NonNull<Context<'static>>>;
type Yield = ();
type Complete<R> = AsyncResult<R>;
type EvmFiber<'a, R> = Fiber<'a, Resume, Yield, Complete<R>>;
type EvmSuspend<R> = Suspend<Resume, Yield, Complete<R>>;

thread_local! {
    static CURRENT: RefCell<Option<NonNull<dyn CurrentFiber>>> = RefCell::new(None);
}

/// Result type used by async EVM execution helpers.
pub type AsyncResult<T> = core::result::Result<T, AsyncError>;

/// Error returned by async EVM execution helpers.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AsyncError {
    /// The async EVM fiber was cancelled before execution completed.
    #[error("async EVM execution was cancelled")]
    Cancelled,
    /// A fiber stack or fiber could not be created.
    #[error("failed to create EVM async fiber: {0}")]
    Fiber(String),
    /// An async host operation was called outside an async EVM fiber.
    #[error("async host operation requires EVM async fiber execution")]
    NotOnFiber,
}

trait CurrentFiber {
    fn context(&mut self) -> &mut Context<'_>;

    fn suspend(&mut self) -> AsyncResult<()>;

    fn is_cancelled(&self) -> bool;
}

struct FiberContext<'a, R> {
    suspend: &'a mut EvmSuspend<R>,
    future_cx: Option<NonNull<Context<'static>>>,
    cancelled: bool,
}

impl<R> CurrentFiber for FiberContext<'_, R> {
    fn context(&mut self) -> &mut Context<'_> {
        let cx = self.future_cx.as_mut().expect("future context is not available");
        unsafe { restore_context_lifetime(cx.as_mut()) }
    }

    fn suspend(&mut self) -> AsyncResult<()> {
        self.future_cx = None;
        match self.suspend.suspend(()) {
            Ok(cx) => {
                self.future_cx = Some(cx);
                Ok(())
            }
            Err(error) => {
                self.cancelled = true;
                Err(error)
            }
        }
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled
    }
}

struct ResetCurrentFiber(Option<NonNull<dyn CurrentFiber>>);

impl Drop for ResetCurrentFiber {
    fn drop(&mut self) {
        CURRENT.with(|current| *current.borrow_mut() = self.0);
    }
}

/// Runs `func` on a native fiber and awaits its completion.
///
/// Synchronous code running inside `func` may call [`block_on_current`] to wait for async host
/// operations without blocking the executor thread.
pub(crate) async fn on_fiber<S, R>(state: &mut S, func: impl FnOnce(&mut S) -> R) -> AsyncResult<R>
where
    S: ?Sized,
{
    on_fiber_with_stack_size(state, DEFAULT_ASYNC_STACK_SIZE, func).await
}

/// Runs `func` on a native fiber with an explicit stack size.
async fn on_fiber_with_stack_size<S, R>(
    state: &mut S,
    stack_size: usize,
    func: impl FnOnce(&mut S) -> R,
) -> AsyncResult<R>
where
    S: ?Sized,
{
    FiberFuture::new(state, stack_size, func)?.await
}

struct FiberFuture<'a, R> {
    fiber: Option<EvmFiber<'a, R>>,
    _not_send: PhantomData<Rc<()>>,
}

impl<R> Unpin for FiberFuture<'_, R> {}

impl<'a, R> FiberFuture<'a, R> {
    fn new<S>(
        state: &'a mut S,
        stack_size: usize,
        func: impl FnOnce(&mut S) -> R + 'a,
    ) -> AsyncResult<Self>
    where
        S: ?Sized,
    {
        let stack = FiberStack::new(stack_size, false).map_err(fiber_error)?;
        let state = core::ptr::from_mut(state);
        let fiber = Fiber::new(stack, move |resume: Resume, suspend| {
            let future_cx = resume?;
            let mut fiber_context =
                FiberContext { suspend, future_cx: Some(future_cx), cancelled: false };
            let current = NonNull::from(&mut fiber_context as &mut dyn CurrentFiber);
            let current = unsafe { erase_current_fiber_lifetime(current) };
            let previous = CURRENT.with(|slot| slot.borrow_mut().replace(current));
            let _reset = ResetCurrentFiber(previous);
            Ok(func(unsafe { &mut *state }))
        })
        .map_err(fiber_error)?;
        Ok(Self { fiber: Some(fiber), _not_send: PhantomData })
    }
}

impl<R> Future for FiberFuture<'_, R> {
    type Output = AsyncResult<R>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let cx = NonNull::from(unsafe { change_context_lifetime(cx) });
        let fiber = this.fiber.as_ref().expect("async EVM fiber already completed");
        match fiber.resume(Ok(cx)) {
            Ok(result) => {
                this.fiber = None;
                Poll::Ready(result)
            }
            Err(()) => Poll::Pending,
        }
    }
}

impl<R> Drop for FiberFuture<'_, R> {
    fn drop(&mut self) {
        let Some(fiber) = self.fiber.take() else { return };
        if fiber.done() {
            return;
        }
        let _ = fiber.resume(Err(AsyncError::Cancelled));
    }
}

/// Polls `future` to completion from inside an async EVM fiber.
///
/// If `future` returns `Poll::Pending`, the current EVM fiber is suspended and the outer
/// [`on_fiber`] future returns `Poll::Pending`. When the executor wakes and polls the outer future
/// again, the EVM fiber resumes and continues polling `future`.
///
/// # Errors
///
/// Returns [`AsyncError::NotOnFiber`] if called outside [`on_fiber`], or
/// [`AsyncError::Cancelled`] if the outer async EVM execution was dropped.
pub(crate) fn block_on_current<F>(future: F) -> AsyncResult<F::Output>
where
    F: Future,
{
    let mut future = core::pin::pin!(future);
    loop {
        if with_current(|current| current.is_cancelled())? {
            return Err(AsyncError::Cancelled);
        }
        match with_current(|current| future.as_mut().poll(current.context()))? {
            Poll::Ready(value) => return Ok(value),
            Poll::Pending => with_current(|current| current.suspend())??,
        }
    }
}

fn with_current<R>(f: impl FnOnce(&mut dyn CurrentFiber) -> R) -> AsyncResult<R> {
    let mut current =
        CURRENT.with(|slot| slot.borrow().as_ref().copied()).ok_or(AsyncError::NotOnFiber)?;
    Ok(f(unsafe { current.as_mut() }))
}

unsafe fn change_context_lifetime<'a>(cx: &'a mut Context<'_>) -> &'a mut Context<'static> {
    unsafe { core::mem::transmute::<&'a mut Context<'_>, &'a mut Context<'static>>(cx) }
}

unsafe fn restore_context_lifetime<'a>(cx: &'a mut Context<'static>) -> &'a mut Context<'a> {
    unsafe { core::mem::transmute::<&'a mut Context<'static>, &'a mut Context<'a>>(cx) }
}

unsafe fn erase_current_fiber_lifetime<'a>(
    fiber: NonNull<dyn CurrentFiber + 'a>,
) -> NonNull<dyn CurrentFiber + 'static> {
    unsafe {
        core::mem::transmute::<NonNull<dyn CurrentFiber + 'a>, NonNull<dyn CurrentFiber + 'static>>(
            fiber,
        )
    }
}

fn fiber_error(error: impl fmt::Display) -> AsyncError {
    AsyncError::Fiber(error.to_string())
}

/// Asynchronous backing database implementation.
pub trait AsyncDatabase: Any {
    /// Database error type.
    type Error: Error + 'static;

    /// Loads account information.
    fn get_account(
        &mut self,
        address: Address,
    ) -> impl Future<Output = core::result::Result<Option<AccountInfo>, Self::Error>> + '_;

    /// Loads bytecode by code hash.
    fn get_code_by_hash(
        &mut self,
        code_hash: B256,
    ) -> impl Future<Output = core::result::Result<Bytecode, Self::Error>> + '_;

    /// Loads a persistent storage slot.
    fn get_storage(
        &mut self,
        address: Address,
        key: Word,
    ) -> impl Future<Output = core::result::Result<Word, Self::Error>> + '_;

    /// Loads a historical block hash.
    fn get_block_hash(
        &mut self,
        number: Word,
    ) -> impl Future<Output = core::result::Result<Option<B256>, Self::Error>> + '_;
}

/// Adapter that exposes an [`AsyncDatabase`] through the synchronous [`DynDatabase`] interface.
pub struct AsyncDb<D: AsyncDatabase> {
    db: D,
    error: Option<Box<dyn Error>>,
}

impl<D: AsyncDatabase> AsyncDb<D> {
    /// Creates a new async database adapter.
    #[inline]
    pub const fn new(db: D) -> Self {
        Self { db, error: None }
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
    pub fn take_error(&mut self) -> Option<Box<dyn Error>> {
        self.error.take()
    }

    #[inline]
    fn store_error(&mut self, error: impl Error + 'static) -> DbErrorCode {
        self.error = Some(Box::new(error));
        stored_error_code()
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
        match block_on_current(self.db.get_account(*address)) {
            Ok(Ok(account)) => Ok(account),
            Ok(Err(error)) => Err(self.store_error(error)),
            Err(error) => Err(self.store_error(error)),
        }
    }

    #[inline]
    fn get_code_by_hash(&mut self, code_hash: &B256) -> DbResult<Bytecode> {
        match block_on_current(self.db.get_code_by_hash(*code_hash)) {
            Ok(Ok(bytecode)) => Ok(bytecode),
            Ok(Err(error)) => Err(self.store_error(error)),
            Err(error) => Err(self.store_error(error)),
        }
    }

    #[inline]
    fn get_storage(&mut self, address: &Address, key: &Word) -> DbResult<Word> {
        match block_on_current(self.db.get_storage(*address, *key)) {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) => Err(self.store_error(error)),
            Err(error) => Err(self.store_error(error)),
        }
    }

    #[inline]
    fn get_block_hash(&mut self, number: &Word) -> DbResult<Option<B256>> {
        match block_on_current(self.db.get_block_hash(*number)) {
            Ok(Ok(hash)) => Ok(hash),
            Ok(Err(error)) => Err(self.store_error(error)),
            Err(error) => Err(self.store_error(error)),
        }
    }

    #[inline]
    fn error(&mut self, code: DbErrorCode) -> Box<dyn Error> {
        if code == stored_error_code()
            && let Some(error) = self.error.take()
        {
            return error;
        }
        Box::new(AsyncDbErrorUnavailable(code))
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
    use crate::{bytecode::Bytecode, evm::DynDatabase, interpreter::Word};
    use alloc::boxed::Box;
    use alloy_primitives::{Address, B256};
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
        let mut future = Box::pin(on_fiber(&mut state, |state| {
            *state += block_on_current(PendingOnce { pending: true }).unwrap();
            *state
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
        let mut future =
            Box::pin(on_fiber(&mut db, |db| DynDatabase::get_storage(db, &address, &key).unwrap()));
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
        let mut future =
            Box::pin(on_fiber(&mut db, |db| DynDatabase::get_storage(db, &address, &key).unwrap()));
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
        let code =
            on_fiber(&mut db, |db| DynDatabase::get_storage(db, &address, &key).unwrap_err());
        let code = poll_ready(code).unwrap();

        assert_eq!(db.error(code).to_string(), "storage read failed");
    }

    #[test]
    fn dropping_fiber_cancels_blocked_future() {
        let mut saw_cancel = false;
        let mut future = Box::pin(on_fiber(&mut saw_cancel, |saw_cancel| {
            *saw_cancel = matches!(block_on_current(PendingForever), Err(AsyncError::Cancelled));
        }));
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);

        assert!(matches!(future.as_mut().poll(&mut cx), Poll::Pending));
        drop(future);
        assert!(saw_cancel);
    }

    fn poll_ready<F: Future>(future: F) -> F::Output {
        let mut future = Box::pin(future);
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

        fn get_account(
            &mut self,
            _address: Address,
        ) -> impl Future<Output = Result<Option<crate::evm::AccountInfo>, Self::Error>> + '_
        {
            core::future::ready(Ok(None))
        }

        fn get_code_by_hash(
            &mut self,
            _code_hash: B256,
        ) -> impl Future<Output = Result<Bytecode, Self::Error>> + '_ {
            core::future::ready(Ok(Bytecode::default()))
        }

        fn get_storage(
            &mut self,
            _address: Address,
            _key: Word,
        ) -> impl Future<Output = Result<Word, Self::Error>> + '_ {
            core::future::ready(Ok(Word::from(9)))
        }

        fn get_block_hash(
            &mut self,
            _number: Word,
        ) -> impl Future<Output = Result<Option<B256>, Self::Error>> + '_ {
            core::future::ready(Ok(None))
        }
    }

    struct PendingDb {
        pending: bool,
    }

    impl AsyncDatabase for PendingDb {
        type Error = Infallible;

        fn get_account(
            &mut self,
            _address: Address,
        ) -> impl Future<Output = Result<Option<crate::evm::AccountInfo>, Self::Error>> + '_
        {
            core::future::ready(Ok(None))
        }

        fn get_code_by_hash(
            &mut self,
            _code_hash: B256,
        ) -> impl Future<Output = Result<Bytecode, Self::Error>> + '_ {
            core::future::ready(Ok(Bytecode::default()))
        }

        fn get_storage(
            &mut self,
            _address: Address,
            _key: Word,
        ) -> impl Future<Output = Result<Word, Self::Error>> + '_ {
            PendingStorage { db: self }
        }

        fn get_block_hash(
            &mut self,
            _number: Word,
        ) -> impl Future<Output = Result<Option<B256>, Self::Error>> + '_ {
            core::future::ready(Ok(None))
        }
    }

    struct PendingStorage<'a> {
        db: &'a mut PendingDb,
    }

    impl Future for PendingStorage<'_> {
        type Output = Result<Word, Infallible>;

        fn poll(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
            if self.db.pending {
                self.db.pending = false;
                Poll::Pending
            } else {
                Poll::Ready(Ok(Word::from(9)))
            }
        }
    }

    struct FailingDb;

    impl AsyncDatabase for FailingDb {
        type Error = TestError;

        fn get_account(
            &mut self,
            _address: Address,
        ) -> impl Future<Output = Result<Option<crate::evm::AccountInfo>, Self::Error>> + '_
        {
            core::future::ready(Ok(None))
        }

        fn get_code_by_hash(
            &mut self,
            _code_hash: B256,
        ) -> impl Future<Output = Result<Bytecode, Self::Error>> + '_ {
            core::future::ready(Ok(Bytecode::default()))
        }

        fn get_storage(
            &mut self,
            _address: Address,
            _key: Word,
        ) -> impl Future<Output = Result<Word, Self::Error>> + '_ {
            core::future::ready(Err(TestError))
        }

        fn get_block_hash(
            &mut self,
            _number: Word,
        ) -> impl Future<Output = Result<Option<B256>, Self::Error>> + '_ {
            core::future::ready(Ok(None))
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
