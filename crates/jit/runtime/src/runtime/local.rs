//! Per-thread cache of resident compiled programs.
//!
//! Every call frame looks up its contract. Reading the shared resident map takes a shard lock and
//! clones an `Arc`, and reporting the hit pushes to a shared queue: each writes a cache line that
//! bounces between last-level caches when threads on several of them execute blocks. Instead,
//! each thread keeps the programs it hit in a cache that only it and the backend thread lock, and
//! reports hits to the backend in deduplicated batches. The backend invalidates every cache when it
//! removes or replaces a resident program.

use crate::runtime::{BackendShared, CompiledProgram, LookupRequest, RuntimeCacheKey};
use alloy_primitives::{
    Bytes,
    map::{HashMap, HashSet},
};
use std::{
    cell::RefCell,
    fmt, mem,
    ops::Deref,
    ptr::{self, NonNull},
    sync::{Arc, Mutex, MutexGuard, PoisonError, Weak, atomic::Ordering},
};

/// Hits after which a thread adds its hit count to the stats and reports every key it hits again.
///
/// The backend refreshes a key's last-hit time from these reports, so a resident program used by
/// any thread is reported at least once per this many of that thread's hits.
const HIT_FLUSH_INTERVAL: u64 = 1024;

thread_local! {
    /// The calling thread's cache for the backend it used last.
    static CACHE: RefCell<Option<Slot>> = const { RefCell::new(None) };
}

/// The calling thread's cache and the backend it belongs to.
struct Slot {
    /// The backend, whose allocation the cache's `Weak` keeps from being reused.
    backend: *const BackendShared,
    cache: Arc<Mutex<LocalCache>>,
}

/// The per-thread caches of one backend, which it invalidates.
pub(crate) type LocalCaches = Mutex<Vec<Weak<Mutex<LocalCache>>>>;

/// A compiled program from [`JitBackend::lookup_local`](super::JitBackend::lookup_local).
///
/// Borrows the program from the calling thread's cache without touching its reference count.
/// The cache keeps every program it hands out alive until the thread drops all of these
/// handles, even if the backend evicts it or a nested lookup switches backends meanwhile.
pub struct LocalProgram {
    /// The program, owned by `cache` or `_owned`.
    program: NonNull<CompiledProgram>,
    /// The thread's cache that keeps the program alive while borrowed.
    cache: Option<Arc<Mutex<LocalCache>>>,
    /// Keeps a program that did not come from the thread's cache alive.
    _owned: Option<Arc<CompiledProgram>>,
}

impl LocalProgram {
    /// Wraps a program that no thread's cache holds.
    pub(crate) fn owned(program: Arc<CompiledProgram>) -> Self {
        Self { program: NonNull::from(&*program), cache: None, _owned: Some(program) }
    }

    /// Returns a new reference-counted handle to the program.
    pub fn to_arc(&self) -> Arc<CompiledProgram> {
        // SAFETY: `program` points into a live `Arc` held by `cache` or `_owned`.
        unsafe {
            Arc::increment_strong_count(self.program.as_ptr());
            Arc::from_raw(self.program.as_ptr())
        }
    }
}

impl Deref for LocalProgram {
    type Target = CompiledProgram;

    #[inline]
    fn deref(&self) -> &CompiledProgram {
        // SAFETY: `program` stays alive until this handle is dropped, see `LocalCache::retired`.
        unsafe { self.program.as_ref() }
    }
}

impl fmt::Debug for LocalProgram {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

impl Drop for LocalProgram {
    #[inline]
    fn drop(&mut self) {
        if let Some(cache) = &self.cache {
            let retired = {
                let mut cache = lock(cache);
                cache.borrows -= 1;
                if cache.borrows == 0 { mem::take(&mut cache.retired) } else { Vec::new() }
            };
            drop(retired);
        }
    }
}

/// One thread's cached programs for one backend.
#[derive(Debug)]
pub(crate) struct LocalCache {
    /// Backend the cached programs belong to.
    backend: Weak<BackendShared>,
    /// Programs this thread found resident.
    programs: HashMap<RuntimeCacheKey, Arc<CompiledProgram>>,
    /// Keys reported to the backend since the last flush.
    reported: HashSet<RuntimeCacheKey>,
    /// Hits not yet added to the backend's stats.
    pending_hits: u64,
    /// Number of live [`LocalProgram`]s borrowed from this cache.
    borrows: usize,
    /// Programs dropped from the cache while a [`LocalProgram`] may still run them.
    retired: Vec<Arc<CompiledProgram>>,
}

impl LocalCache {
    /// Empties the cache, returning the programs the caller may drop.
    ///
    /// While programs are borrowed, they are kept alive until the last borrow is released.
    fn invalidate(&mut self) -> Vec<Arc<CompiledProgram>> {
        self.reported.clear();
        let programs = self.programs.drain().map(|(_, program)| program);
        if self.borrows > 0 {
            self.retired.extend(programs);
            Vec::new()
        } else {
            programs.collect()
        }
    }

    /// Counts a hit and reports `key` to the backend if it was not reported since the last flush.
    fn record_hit(&mut self, shared: &BackendShared, key: &RuntimeCacheKey) {
        self.pending_hits += 1;
        if self.reported.insert(*key) {
            // An empty bytecode marks a hit, which refreshes the key's last-hit time.
            if shared.events.push(LookupRequest { key: *key, code: Bytes::new() }).is_err() {
                shared.stats.events_dropped.fetch_add(1, Ordering::Relaxed);
            }
            self.flush_hits(shared);
        } else if self.pending_hits >= HIT_FLUSH_INTERVAL {
            self.flush_hits(shared);
            self.reported.clear();
        }
    }

    /// Adds the pending hits to the backend's stats.
    fn flush_hits(&mut self, shared: &BackendShared) {
        let hits = mem::take(&mut self.pending_hits);
        if hits > 0 {
            shared.stats.lookup_hits.fetch_add(hits, Ordering::Relaxed);
        }
    }
}

impl Drop for LocalCache {
    fn drop(&mut self) {
        if let Some(shared) = self.backend.upgrade() {
            self.flush_hits(&shared);
        }
    }
}

/// Returns the resident program for `key`, reading the shared map only on a thread-local miss.
///
/// Returns `None` if the program is not resident.
pub(super) fn get(shared: &Arc<BackendShared>, key: &RuntimeCacheKey) -> Option<LocalProgram> {
    let found = CACHE.try_with(|slot| {
        let mut slot = slot.borrow_mut();
        let cache = match &*slot {
            Some(slot) if ptr::eq(slot.backend, Arc::as_ptr(shared)) => &slot.cache,
            _ => &slot.insert(Slot { backend: Arc::as_ptr(shared), cache: register(shared) }).cache,
        };
        let mut guard = lock(cache);
        let program = match guard.programs.get(key) {
            Some(program) => NonNull::from(&**program),
            None => {
                // Read the map under the cache lock, so that a removal either precedes this read
                // or invalidates the cache after the insert below.
                let program = Arc::clone(&*shared.resident.try_get(key).try_unwrap()?);
                let ptr = NonNull::from(&*program);
                guard.programs.insert(*key, program);
                ptr
            }
        };
        guard.record_hit(shared, key);
        guard.borrows += 1;
        drop(guard);
        Some(LocalProgram { program, cache: Some(Arc::clone(cache)), _owned: None })
    });
    match found {
        Ok(found) => found,
        // The thread is exiting and its cache is gone: read the shared map.
        Err(_) => {
            let program = Arc::clone(&*shared.resident.try_get(key).try_unwrap()?);
            shared.stats.lookup_hits.fetch_add(1, Ordering::Relaxed);
            Some(LocalProgram::owned(program))
        }
    }
}

/// Creates the calling thread's cache for `shared` and registers it for invalidation.
fn register(shared: &Arc<BackendShared>) -> Arc<Mutex<LocalCache>> {
    let cache = Arc::new(Mutex::new(LocalCache {
        backend: Arc::downgrade(shared),
        programs: HashMap::default(),
        reported: HashSet::default(),
        pending_hits: 0,
        borrows: 0,
        retired: Vec::new(),
    }));
    let mut caches = lock(&shared.local_caches);
    caches.retain(|cache| cache.strong_count() > 0);
    caches.push(Arc::downgrade(&cache));
    cache
}

/// Invalidates every thread's cache of `caches`.
pub(crate) fn invalidate_all(caches: &LocalCaches) {
    lock(caches).retain(|cache| {
        let Some(cache) = cache.upgrade() else { return false };
        // Free the code outside the lock, which the thread takes on every lookup.
        let programs = lock(&cache).invalidate();
        drop(programs);
        true
    });
}

/// Locks `mutex`, ignoring poisoning: the caches stay consistent if a holder panics.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
