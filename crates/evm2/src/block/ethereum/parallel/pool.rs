//! Long-lived threads that run a parallel block's helper work, so each block hands its work to
//! threads that already exist instead of spawning and joining new ones, and the threads keep their
//! thread-local caches and allocator state warm across blocks.

use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::{
    any::Any,
    marker::PhantomData,
    mem,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};
use std::{
    io,
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
    sync::{Condvar, Mutex},
    thread::{self, Thread},
};

/// How long an idle pool thread waits for work before it exits.
const KEEP_ALIVE: Duration = Duration::from_secs(60);

/// Work a pool thread runs.
type Job = Box<dyn FnOnce() + Send>;

/// A panic payload.
type Panic = Box<dyn Any + Send>;

/// The pool's idle threads, the most recently idle last, which takes the next job first.
static IDLE: Mutex<Vec<Arc<Slot>>> = Mutex::new(Vec::new());

/// Where an idle pool thread waits for its next job.
#[derive(Default)]
struct Slot {
    job: Mutex<Option<Job>>,
    ready: Condvar,
}

/// Runs `job` on an idle pool thread, or on a new one when none is idle. Drops `job` unrun when
/// no thread spawns.
fn submit(job: Job) -> io::Result<()> {
    let idle = IDLE.lock().expect("lock poisoned").pop();
    if let Some(slot) = idle {
        *slot.job.lock().expect("lock poisoned") = Some(job);
        slot.ready.notify_one();
        return Ok(());
    }
    thread::Builder::new().name("evm2-pool".into()).spawn(move || serve(job)).map(drop)
}

/// Runs `job`, then the jobs submitted to this thread while it is idle, until it idles for
/// [`KEEP_ALIVE`].
fn serve(mut job: Job) {
    let slot = Arc::new(Slot::default());
    loop {
        // A job's panic has been reported by the panic hook and, for a scoped job, handed to its
        // scope: the thread stays in the pool.
        let _ = catch_unwind(AssertUnwindSafe(job));
        IDLE.lock().expect("lock poisoned").push(slot.clone());
        let mut next = slot.job.lock().expect("lock poisoned");
        job = loop {
            if let Some(job) = next.take() {
                break job;
            }
            let (guard, wait) = slot.ready.wait_timeout(next, KEEP_ALIVE).expect("lock poisoned");
            next = guard;
            if wait.timed_out() && next.is_none() {
                // Leave the pool, unless a submitter already took this thread: its job is then on
                // its way.
                let mut idle = IDLE.lock().expect("lock poisoned");
                if let Some(index) = idle.iter().position(|idle| Arc::ptr_eq(idle, &slot)) {
                    idle.remove(index);
                    return;
                }
            }
        };
    }
}

/// Runs `f` on a pool thread, or drops it here unrun when no thread spawns.
pub(super) fn spawn_detached(f: impl FnOnce() + Send + 'static) {
    let _ = submit(Box::new(f));
}

/// Works as [`std::thread::scope`], with the scope's jobs on pool threads: returns only once every
/// job spawned in the scope finished, and resumes the panic of the scope's closure, or else of a
/// job whose handle was dropped unjoined.
pub(super) fn scope<'env, F, T>(f: F) -> T
where
    F: for<'scope> FnOnce(&'scope Scope<'scope, 'env>) -> T,
{
    let scope = Scope {
        data: Arc::new(ScopeData {
            running: AtomicUsize::new(0),
            owner: thread::current(),
            panic: Mutex::new(None),
        }),
        scope: PhantomData,
        env: PhantomData,
    };
    let result = catch_unwind(AssertUnwindSafe(|| f(&scope)));
    while scope.data.running.load(Ordering::Acquire) != 0 {
        thread::park();
    }
    let result = result.unwrap_or_else(|panic| resume_unwind(panic));
    if let Some(panic) = scope.data.panic.lock().expect("lock poisoned").take() {
        resume_unwind(panic);
    }
    result
}

/// A scope to spawn jobs borrowing from `'env` on pool threads, see [`scope`].
pub(super) struct Scope<'scope, 'env: 'scope> {
    data: Arc<ScopeData>,
    scope: PhantomData<&'scope mut &'scope ()>,
    env: PhantomData<&'env mut &'env ()>,
}

impl<'scope> Scope<'scope, '_> {
    /// Runs `f` on a pool thread, as [`std::thread::Scope::spawn`].
    ///
    /// # Panics
    ///
    /// Panics when no thread is idle and none spawns.
    pub(super) fn spawn<F, T>(&'scope self, f: F) -> ScopedJoinHandle<'scope, T>
    where
        F: FnOnce() -> T + Send + 'scope,
        T: Send + 'scope,
    {
        let packet = Arc::new(Packet {
            scope: self.data.clone(),
            result: Mutex::new(None),
            done: Condvar::new(),
            lifetime: PhantomData,
        });
        self.data.running.fetch_add(1, Ordering::Relaxed);
        let finish = Finish { packet: Some(packet.clone()), scope: self.data.clone() };
        let job = Box::new(move || {
            let result = catch_unwind(AssertUnwindSafe(f));
            if let Some(packet) = &finish.packet {
                *packet.result.lock().expect("lock poisoned") = Some(result);
                packet.done.notify_all();
            }
            drop(finish);
        }) as Box<dyn FnOnce() + Send + 'scope>;
        // SAFETY: `scope` returns only once the job ran or was dropped unrun, which `Finish`
        // reports last, so nothing the job borrows for `'scope` is freed while it exists.
        let job = unsafe { mem::transmute::<Box<dyn FnOnce() + Send + 'scope>, Job>(job) };
        if let Err(error) = submit(job) {
            panic!("failed to spawn thread: {error}");
        }
        ScopedJoinHandle { packet }
    }
}

/// The state a [`Scope`] shares with its jobs.
struct ScopeData {
    /// How many of the scope's jobs have not finished.
    running: AtomicUsize,
    /// The thread waiting in [`scope`] for the jobs.
    owner: Thread,
    /// The first panic of a job whose handle was dropped unjoined.
    panic: Mutex<Option<Panic>>,
}

/// A job's result, shared by the job and its handle.
struct Packet<'scope, T> {
    scope: Arc<ScopeData>,
    result: Mutex<Option<thread::Result<T>>>,
    done: Condvar,
    lifetime: PhantomData<&'scope ()>,
}

impl<T> Drop for Packet<'_, T> {
    fn drop(&mut self) {
        if let Some(Err(panic)) = self.result.get_mut().expect("lock poisoned").take() {
            self.scope.panic.lock().expect("lock poisoned").get_or_insert(panic);
        }
    }
}

/// Reports a job finished, or dropped unrun, to its scope, after releasing its result.
struct Finish<'scope, T> {
    packet: Option<Arc<Packet<'scope, T>>>,
    scope: Arc<ScopeData>,
}

impl<T> Drop for Finish<'_, T> {
    fn drop(&mut self) {
        drop(self.packet.take());
        if self.scope.running.fetch_sub(1, Ordering::Release) == 1 {
            self.scope.owner.unpark();
        }
    }
}

/// A handle to join a job spawned with [`Scope::spawn`].
pub(super) struct ScopedJoinHandle<'scope, T> {
    packet: Arc<Packet<'scope, T>>,
}

impl<T> ScopedJoinHandle<'_, T> {
    /// Waits for the job to finish and returns its result, or its panic.
    pub(super) fn join(self) -> thread::Result<T> {
        let mut result = self.packet.result.lock().expect("lock poisoned");
        loop {
            if let Some(result) = result.take() {
                return result;
            }
            result = self.packet.done.wait(result).expect("lock poisoned");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::sync::atomic::AtomicBool;

    #[test]
    fn scope_waits_for_every_job() {
        let counter = AtomicUsize::new(0);
        scope(|scope| {
            for _ in 0..16 {
                scope.spawn(|| {
                    thread::sleep(Duration::from_millis(5));
                    counter.fetch_add(1, Ordering::Relaxed);
                });
            }
        });
        assert_eq!(counter.load(Ordering::Relaxed), 16);
    }

    #[test]
    fn join_returns_result_and_panic() {
        let value = 7;
        let (joined, panicked) = scope(|scope| {
            let joined = scope.spawn(|| value * 6);
            let panicked = scope.spawn(|| panic!("job"));
            (joined.join(), panicked.join().is_err())
        });
        assert_eq!(joined.ok(), Some(42));
        assert!(panicked);
    }

    #[test]
    fn unjoined_panic_resumes_after_jobs_finish() {
        let finished = AtomicBool::new(false);
        let panic = catch_unwind(AssertUnwindSafe(|| {
            scope(|scope| {
                scope.spawn(|| panic!("unjoined"));
                scope.spawn(|| {
                    thread::sleep(Duration::from_millis(20));
                    finished.store(true, Ordering::Relaxed);
                });
            });
        }))
        .unwrap_err();
        assert_eq!(panic.downcast_ref::<&str>(), Some(&"unjoined"));
        assert!(finished.load(Ordering::Relaxed));
    }

    #[test]
    fn threads_are_reused() {
        let first = scope(|scope| scope.spawn(|| thread::current().id()).join().unwrap());
        let mut reused = false;
        for _ in 0..64 {
            let next = scope(|scope| scope.spawn(|| thread::current().id()).join().unwrap());
            reused |= next == first;
        }
        assert!(reused);
    }
}
