//! Per-thread cache of resident compiled programs.
//!
//! Every call frame looks up its contract. Reading the shared resident map takes a shard lock and
//! clones an `Arc`, and reporting the hit pushes to a shared queue: each writes a cache line that
//! bounces between last-level caches when threads on several of them execute blocks. Instead,
//! each thread keeps the programs it hit in a cache that only it and the backend thread lock, and
//! reports hits to the backend in deduplicated batches. The backend invalidates a key in every
//! cache when it removes or replaces that program, and every key when it clears them all.

use crate::runtime::{BackendShared, CompiledProgram, LookupEvent, RuntimeCacheKey};
use alloy_primitives::map::HashMap;
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
/// any thread is reported at least once per this many of that thread's hits, and once after each
/// eviction sweep, which keeps threads that hit less often from looking idle.
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
        Self { program: program_ptr(&program), cache: None, _owned: Some(program) }
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
    programs: HashMap<RuntimeCacheKey, CachedProgram>,
    /// Hits not yet added to the backend's stats.
    pending_hits: u64,
    /// Hits since the cached programs were last marked unreported.
    hits_since_clear: u64,
    /// The backend's report epoch when the cached programs were last marked unreported.
    report_epoch: u64,
    /// Number of live [`LocalProgram`]s borrowed from this cache.
    borrows: usize,
    /// Programs dropped from the cache while a [`LocalProgram`] may still run them.
    retired: Vec<Arc<CompiledProgram>>,
}

/// A program in a thread's cache.
#[derive(Debug)]
struct CachedProgram {
    program: Arc<CompiledProgram>,
    /// Whether the backend heard of a hit on this program since the last mark.
    reported: bool,
}

impl LocalCache {
    /// Creates an empty cache for `backend`.
    fn new(backend: Weak<BackendShared>) -> Self {
        Self {
            backend,
            programs: HashMap::default(),
            pending_hits: 0,
            hits_since_clear: 0,
            report_epoch: 0,
            borrows: 0,
            retired: Vec::new(),
        }
    }

    /// Empties the cache, returning the programs the caller may drop.
    ///
    /// While programs are borrowed, they are kept alive until the last borrow is released.
    fn invalidate(&mut self) -> Vec<Arc<CompiledProgram>> {
        let programs = self.programs.drain().map(|(_, cached)| cached.program);
        if self.borrows > 0 {
            self.retired.extend(programs);
            Vec::new()
        } else {
            programs.collect()
        }
    }

    /// Drops `key` from the cache, returning the program if the caller may drop it.
    ///
    /// While programs are borrowed, the program is kept alive until the last borrow is released.
    fn invalidate_key(&mut self, key: &RuntimeCacheKey) -> Option<Arc<CompiledProgram>> {
        let program = self.programs.remove(key)?.program;
        if self.borrows > 0 {
            self.retired.push(program);
            None
        } else {
            Some(program)
        }
    }

    /// Counts a hit on the cached program for `key` and returns it, or returns `None` if the
    /// program is not cached.
    ///
    /// Reports the key to the backend if it was not reported since the cached programs were last
    /// marked unreported, which happens every [`HIT_FLUSH_INTERVAL`] hits and after each eviction
    /// sweep.
    fn hit(
        &mut self,
        shared: &BackendShared,
        key: &RuntimeCacheKey,
    ) -> Option<NonNull<CompiledProgram>> {
        let epoch = shared.report_epoch.load(Ordering::Relaxed);
        if self.hits_since_clear >= HIT_FLUSH_INTERVAL || epoch != self.report_epoch {
            for cached in self.programs.values_mut() {
                cached.reported = false;
            }
            self.hits_since_clear = 0;
            self.report_epoch = epoch;
        }
        let cached = self.programs.get_mut(key)?;
        let program = program_ptr(&cached.program);
        self.pending_hits += 1;
        self.hits_since_clear += 1;
        if !mem::replace(&mut cached.reported, true) {
            if shared.events.push(LookupEvent::Hit(*key)).is_err() {
                shared.stats.events_dropped.fetch_add(1, Ordering::Relaxed);
                // Report the key again on its next hit.
                cached.reported = false;
            }
            self.flush_hits(shared);
        }
        Some(program)
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
        let program = match guard.hit(shared, key) {
            Some(program) => program,
            None => {
                // Read the map under the cache lock, so that a removal either precedes this read
                // or invalidates the cache after the insert below.
                let program = Arc::clone(&*shared.resident.try_get(key).try_unwrap()?);
                guard.programs.insert(*key, CachedProgram { program, reported: false });
                guard.hit(shared, key)?
            }
        };
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
    let cache = Arc::new(Mutex::new(LocalCache::new(Arc::downgrade(shared))));
    let mut caches = lock(&shared.local_caches);
    caches.retain(|cache| cache.strong_count() > 0);
    caches.push(Arc::downgrade(&cache));
    cache
}

/// Invalidates `key` in every thread's cache of `caches`.
pub(crate) fn invalidate_key(caches: &LocalCaches, key: &RuntimeCacheKey) {
    lock(caches).retain(|cache| {
        let Some(cache) = cache.upgrade() else { return false };
        // Free the code outside the lock, which the thread takes on every lookup.
        let program = lock(&cache).invalidate_key(key);
        drop(program);
        true
    });
}

/// Invalidates every key in every thread's cache of `caches`.
pub(crate) fn invalidate_all(caches: &LocalCaches) {
    lock(caches).retain(|cache| {
        let Some(cache) = cache.upgrade() else { return false };
        // Free the code outside the lock, which the thread takes on every lookup.
        let programs = lock(&cache).invalidate();
        drop(programs);
        true
    });
}

/// Returns a pointer to `program` that keeps the provenance of its whole allocation, which
/// [`LocalProgram::to_arc`] needs to reach the reference counts.
fn program_ptr(program: &Arc<CompiledProgram>) -> NonNull<CompiledProgram> {
    // SAFETY: `Arc::as_ptr` is never null.
    unsafe { NonNull::new_unchecked(Arc::as_ptr(program).cast_mut()) }
}

/// Locks `mutex`, ignoring poisoning: the caches stay consistent if a holder panics.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[cfg(feature = "llvm")]
mod tests {
    use super::*;
    use crate::runtime::{
        JitBackend, LookupDecision, ResidentMap, RuntimeConfig, backend::EventQueue,
        stats::RuntimeStats,
    };
    use alloy_primitives::B256;
    use evm2::SpecId;
    use std::sync::atomic::{AtomicU64, AtomicUsize};

    /// PUSH1 0x42 PUSH0 MSTORE PUSH1 0x20 PUSH0 RETURN.
    const BYTECODE_RET42: &[u8] = &[0x60, 0x42, 0x5f, 0x52, 0x60, 0x20, 0x5f, 0xf3];

    fn key(n: u64) -> RuntimeCacheKey {
        RuntimeCacheKey {
            code_hash: B256::left_padding_from(&n.to_be_bytes()),
            spec_id: SpecId::CANCUN,
        }
    }

    /// A backend state that no backend thread drains, and a cache of `keys` that all map to one
    /// compiled program.
    fn cache_of(keys: impl IntoIterator<Item = u64>) -> (Arc<BackendShared>, LocalCache) {
        let backend = JitBackend::new(RuntimeConfig {
            enabled: true,
            blocking: true,
            ..RuntimeConfig::default()
        })
        .unwrap();
        let LookupDecision::Compiled(program) = backend.lookup(crate::runtime::LookupRequest {
            key: key(u64::MAX),
            code: BYTECODE_RET42.into(),
        }) else {
            panic!("blocking lookup did not compile");
        };
        let shared = Arc::new(BackendShared {
            resident: ResidentMap::default(),
            events: EventQueue::new(16 * HIT_FLUSH_INTERVAL as usize),
            local_caches: Default::default(),
            pause_depth: AtomicUsize::new(0),
            report_epoch: AtomicU64::new(0),
            stats: Arc::new(RuntimeStats::default()),
        });
        let mut cache = LocalCache::new(Arc::downgrade(&shared));
        for n in keys {
            cache
                .programs
                .insert(key(n), CachedProgram { program: Arc::clone(&program), reported: false });
        }
        (shared, cache)
    }

    fn reports_of(shared: &BackendShared, key: &RuntimeCacheKey) -> usize {
        let mut reports = 0;
        while let Some(event) = shared.events.pop() {
            reports += usize::from(matches!(event, LookupEvent::Hit(hit) if hit == *key));
        }
        reports
    }

    #[test]
    fn hot_key_is_reported_again_while_new_keys_arrive() {
        let (shared, mut cache) = cache_of(0..=2 * HIT_FLUSH_INTERVAL);

        // Each first report of a new key adds the pending hits to the stats, which must not stop
        // the hot key's reports from restarting after `HIT_FLUSH_INTERVAL` hits.
        let hot = key(0);
        for n in 1..=2 * HIT_FLUSH_INTERVAL {
            cache.hit(&shared, &hot).unwrap();
            cache.hit(&shared, &key(n)).unwrap();
        }

        let hot_reports = reports_of(&shared, &hot);
        assert!(hot_reports >= 2, "hot key reported {hot_reports} times");
    }

    #[test]
    fn key_is_reported_again_after_an_eviction_sweep() {
        let (shared, mut cache) = cache_of([0]);

        // A thread that hits far less than `HIT_FLUSH_INTERVAL` times between sweeps must still
        // refresh the key's last-hit time after each sweep.
        let hot = key(0);
        for _ in 0..3 {
            cache.hit(&shared, &hot).unwrap();
            cache.hit(&shared, &hot).unwrap();
            shared.report_epoch.fetch_add(1, Ordering::Relaxed);
        }

        assert_eq!(reports_of(&shared, &hot), 3);
    }
}
