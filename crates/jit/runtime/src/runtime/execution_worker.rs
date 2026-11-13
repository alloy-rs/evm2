//! Execution-thread-owned references and event publication.
//!
//! One TLS worker follows the most recently used backend on each execution thread. Its code
//! cache retains compiled programs behind a worker-private mutex. The coordinator revokes idle
//! caches and retries active caches without waiting for execution. Reentrant calls
//! use the shared lookup path. Workers publish registrations through an inbox. The coordinator
//! exclusively owns the collection and reclaims retired lanes after their final completion mark.

use super::*;
use std::{
    cell::RefCell,
    sync::{Mutex, Weak, atomic::AtomicU64},
};

const DRAIN_BATCH: usize = 64;

thread_local! {
    static WORKER: RefCell<Option<Worker>> = const { RefCell::new(None) };
}

pub(super) struct Publication<'a>(pub(super) &'a AtomicUsize);

impl Drop for Publication<'_> {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Release);
    }
}

#[repr(align(128))]
#[derive(Default)]
struct CompletionMark(AtomicUsize);

#[repr(align(128))]
#[derive(Default)]
struct Telemetry {
    dropped: AtomicU64,
    cached: AtomicUsize,
    final_mark: AtomicUsize,
    retired: AtomicBool,
    completed: CompletionMark,
    cache: Mutex<ProgramCache>,
    revoke: AtomicBool,
    reclaiming: AtomicUsize,
}

struct Event {
    generation: usize,
    lookup: LookupEvent,
}

/// A notification moved out of its ring, with completion published after handling or discard.
/// Slot reuse is already safe after the move. Lane retirement additionally waits for completion.
pub(super) struct PendingEvent {
    event: Option<LookupEvent>,
    generation: usize,
    mark: usize,
    telemetry: Arc<Telemetry>,
}

impl PendingEvent {
    fn new(event: Event, lane: &Lane) -> Self {
        Self {
            event: Some(event.lookup),
            generation: event.generation,
            mark: lane.consumer.position(),
            telemetry: Arc::clone(&lane.telemetry),
        }
    }

    pub(super) const fn take_lookup(&mut self, generation: usize) -> Option<LookupEvent> {
        if self.generation == generation { self.event.take() } else { None }
    }
}

impl Drop for PendingEvent {
    fn drop(&mut self) {
        // The coordinator drops pending events after moving them out of the ring. An owned byte
        // buffer may run arbitrary user code here. Publish completion only after that code
        // returns.
        drop(self.event.take());
        self.telemetry.completed.0.store(self.mark, Ordering::Release);
    }
}

struct Lane {
    consumer: spsc::Consumer<Event>,
    telemetry: Arc<Telemetry>,
    capacity: usize,
}

#[derive(Default)]
pub(super) struct Lanes {
    lanes: Vec<Lane>,
    next: usize,
    retired_drops: u64,
}

#[derive(Default)]
pub(super) struct Registry {
    registrations: crossbeam_queue::SegQueue<Lane>,
    gauges: QueueStats,
    revoking: AtomicBool,
}

impl Registry {
    fn register(&self, capacity: usize) -> (spsc::Producer<Event>, Arc<Telemetry>) {
        let (producer, consumer) = spsc::channel(capacity);
        let telemetry = Arc::new(Telemetry::default());
        self.registrations.push(Lane { consumer, telemetry: Arc::clone(&telemetry), capacity });
        (producer, telemetry)
    }

    pub(super) fn drain_into(
        &self,
        lanes: &mut Lanes,
        limit: usize,
        events: &mut Vec<PendingEvent>,
    ) {
        self.register_pending(lanes);
        let mut examined = 0;
        let mut empty = 0;
        while examined < limit && empty < lanes.lanes.len() {
            let index = lanes.next % lanes.lanes.len();
            lanes.next = (index + 1) % lanes.lanes.len();
            let lane = &mut lanes.lanes[index];
            let mut taken = 0;
            while taken < DRAIN_BATCH && examined < limit {
                let Some(event) = lane.consumer.pop() else { break };
                taken += 1;
                examined += 1;
                events.push(PendingEvent::new(event, lane));
            }
            if taken == 0 {
                empty += 1;
            } else {
                empty = 0;
            }
        }
    }

    pub(super) fn clear(&self, lanes: &mut Lanes, shared: &BackendShared) {
        let mut discarded_shared = Vec::new();
        let mut discarded_workers = Vec::new();
        {
            self.register_pending(lanes);
            for _ in 0..shared.events.len() {
                if let Some(event) = shared.events.pop() {
                    discarded_shared.push(event);
                }
            }
            for lane in &mut lanes.lanes {
                // Discard the currently published prefix. Late old-generation notifications
                // are moved out and discarded by the next normal drain.
                for _ in 0..lane.consumer.len() {
                    if let Some(event) = lane.consumer.pop() {
                        discarded_workers.push(PendingEvent::new(event, lane));
                    }
                }
            }
        }
        drop(discarded_shared);
        for event in discarded_workers {
            drop(event);
        }
        self.finish_retired(lanes, shared);
    }

    pub(super) fn finish_retired(&self, lanes: &mut Lanes, shared: &BackendShared) {
        let mut retired = Vec::new();
        {
            self.register_pending(lanes);
            let mut index = 0;
            while index < lanes.lanes.len() {
                let lane = &lanes.lanes[index];
                let telemetry = &lane.telemetry;
                // Transfer consumer ownership only after the producer stops and every event
                // already moved to the backend's processing batch has completed.
                let finished = telemetry.retired.load(Ordering::Acquire)
                    && telemetry.completed.0.load(Ordering::Acquire) == lane.consumer.position();
                if finished {
                    let lane = lanes.lanes.swap_remove(index);
                    lanes.retired_drops += lane.telemetry.dropped.load(Ordering::Relaxed);
                    retired.push(lane);
                } else {
                    index += 1;
                }
            }
        }
        let generation = shared.event_generation.load(Ordering::Acquire);
        let mut dropped = 0;
        for mut lane in retired {
            let final_mark = lane.telemetry.final_mark.load(Ordering::Relaxed);
            // Forward the stopped producer's remaining prefix to the bounded shared queue.
            // This releases closed rings independently of the normal event-processing budget.
            while lane.consumer.position() != final_mark {
                let event = lane.consumer.pop().expect("retired producer published its final mark");
                let mut event = PendingEvent::new(event, &lane);
                if let Some(lookup) = event.take_lookup(generation)
                    && let Err(lookup) = shared.events.push(lookup)
                {
                    dropped += 1;
                    drop(lookup);
                }
                // Publish completion after ownership transfer or destruction, on the coordinator.
                drop(event);
            }
            debug_assert_eq!(lane.telemetry.completed.0.load(Ordering::Acquire), final_mark);
            // Every published slot is now completed. The coordinator owns lane destruction.
            drop(lane);
        }
        if dropped > 0 {
            shared.stats.events_dropped.fetch_add(dropped, Ordering::Relaxed);
        }
    }

    // A revocation cycle prevents persistent refills until every old reference is released.
    // Never wait for an executing worker, which may synchronously call into the coordinator.
    pub(super) fn revoke_caches(&self, lanes: &mut Lanes, request: bool) -> bool {
        if !request && !self.revoking.load(Ordering::Acquire) {
            return false;
        }
        let starting = request && !self.revoking.swap(true, Ordering::AcqRel);
        self.register_pending(lanes);
        let handles = lanes.lanes.iter().map(|l| Arc::clone(&l.telemetry)).collect::<Vec<_>>();
        let mut pending = false;
        for telemetry in handles {
            if starting {
                telemetry.revoke.store(true, Ordering::Release);
            }
            if telemetry.reclaiming.load(Ordering::Acquire) != 0 {
                pending = true;
                continue;
            }
            if telemetry.revoke.load(Ordering::Acquire) {
                let guard = match telemetry.cache.try_lock() {
                    Ok(guard) => Some(guard),
                    Err(std::sync::TryLockError::Poisoned(error)) => Some(error.into_inner()),
                    Err(std::sync::TryLockError::WouldBlock) => None,
                };
                if let Some(mut cache) = guard {
                    if telemetry.reclaiming.load(Ordering::Acquire) != 0 {
                        pending = true;
                        continue;
                    }
                    telemetry.reclaiming.fetch_add(1, Ordering::AcqRel);
                    let entries = std::mem::take(&mut cache.entries);
                    telemetry.cached.store(0, Ordering::Relaxed);
                    telemetry.revoke.store(false, Ordering::Release);
                    drop(cache);
                    drop(entries);
                    telemetry.reclaiming.fetch_sub(1, Ordering::Release);
                } else {
                    pending = true;
                }
            }
        }
        pending
    }

    pub(super) fn finish_revocation(&self) {
        self.revoking.store(false, Ordering::Release);
    }

    fn register_pending(&self, lanes: &mut Lanes) {
        while let Some(lane) = self.registrations.pop() {
            lane.telemetry.revoke.store(self.revoking.load(Ordering::Acquire), Ordering::Release);
            lanes.lanes.push(lane);
        }
    }

    pub(super) fn publish_stats(&self, lanes: &Lanes) {
        let mut snapshot = RuntimeStatsSnapshot {
            events_dropped: lanes.retired_drops,
            execution_worker_lanes: lanes.lanes.len() as u64,
            ..Default::default()
        };
        for lane in &lanes.lanes {
            snapshot.events_dropped += lane.telemetry.dropped.load(Ordering::Relaxed);
            snapshot.events_queued += lane.consumer.len() as u64;
            snapshot.execution_worker_cached_programs +=
                lane.telemetry.cached.load(Ordering::Relaxed) as u64;
            snapshot.execution_worker_queue_slots += lane.capacity as u64;
            snapshot.execution_worker_queue_bytes +=
                (lane.capacity * std::mem::size_of::<Event>()) as u64;
        }
        self.gauges.dropped.store(snapshot.events_dropped, Ordering::Relaxed);
        self.gauges.lanes.store(snapshot.execution_worker_lanes, Ordering::Relaxed);
        self.gauges.queued.store(snapshot.events_queued, Ordering::Relaxed);
        self.gauges.cached.store(snapshot.execution_worker_cached_programs, Ordering::Relaxed);
        self.gauges.slots.store(snapshot.execution_worker_queue_slots, Ordering::Relaxed);
        self.gauges.bytes.store(snapshot.execution_worker_queue_bytes, Ordering::Relaxed);
    }

    pub(super) fn add_stats(&self, snapshot: &mut RuntimeStatsSnapshot) {
        snapshot.events_dropped += self.gauges.dropped.load(Ordering::Relaxed);
        snapshot.execution_worker_lanes = self.gauges.lanes.load(Ordering::Relaxed);
        snapshot.events_queued += self.gauges.queued.load(Ordering::Relaxed);
        snapshot.execution_worker_cached_programs = self.gauges.cached.load(Ordering::Relaxed);
        snapshot.execution_worker_queue_slots = self.gauges.slots.load(Ordering::Relaxed);
        snapshot.execution_worker_queue_bytes = self.gauges.bytes.load(Ordering::Relaxed);
    }

    pub(super) fn shutdown(&self, lanes: &mut Lanes, shared: &BackendShared) {
        self.revoke_caches(lanes, true);
        self.clear(lanes, shared);
    }
}

impl Drop for Registry {
    fn drop(&mut self) {
        // The final shared-state owner is gone, so coordinator and execution access has
        // stopped. TLS producers may still retain lanes left in the registration inbox
        // when initialization failed before BackendState could own their cleanup.
        let mut lanes = Lanes::default();
        self.revoke_caches(&mut lanes, true);
        for mut lane in lanes.lanes {
            while let Some(event) = lane.consumer.pop() {
                drop(PendingEvent::new(event, &lane));
            }
        }
    }
}

struct CachedProgram {
    key: RuntimeCacheKey,
    program: Arc<CompiledProgram>,
}

#[derive(Default)]
struct ProgramCache {
    epoch: usize,
    entries: Vec<Option<CachedProgram>>,
}

struct Worker {
    backend: Weak<BackendInner>,
    producer: spsc::Producer<Event>,
    telemetry: Arc<Telemetry>,
    dropped: u64,
}

impl Worker {
    fn new(backend: &JitBackend) -> Self {
        let capacity = backend.inner.tuning.channel_capacity.max(1).next_power_of_two();
        let (producer, telemetry) = backend.inner.shared.execution_workers.register(capacity);
        Self { backend: Arc::downgrade(&backend.inner), producer, telemetry, dropped: 0 }
    }

    fn lookup(
        &mut self,
        backend: &JitBackend,
        req: LookupRequest<'_>,
        cache: &mut ProgramCache,
    ) -> bool {
        if !backend.enabled() || !backend.inner.tuning.should_compile(req.code.as_slice()) {
            return false;
        }
        let shared = &backend.inner.shared;
        let epoch = shared.publication_epoch.load(Ordering::Acquire);
        if cache.epoch != epoch || cache.entries.is_empty() {
            cache.entries = (0..64).map(|_| None).collect();
            cache.epoch = epoch;
            self.telemetry.cached.store(0, Ordering::Relaxed);
        }
        if epoch & 1 != 0 {
            return false;
        }
        let index = (u64::from_le_bytes(req.key.code_hash[..8].try_into().unwrap()) as usize
            ^ req.key.spec_id as usize)
            & 63;
        if cache.entries[index].as_ref().is_none_or(|e| e.key != req.key) {
            let program = shared.resident.try_get(&req.key).try_unwrap().map(|p| Arc::clone(&p));
            if shared.publication_epoch.load(Ordering::Acquire) != epoch {
                return false;
            }
            if let Some(program) = program {
                if cache.entries[index].is_none() {
                    self.telemetry.cached.fetch_add(1, Ordering::Relaxed);
                }
                cache.entries[index] = Some(CachedProgram { key: req.key, program });
            } else {
                self.publish(
                    shared.event_generation.load(Ordering::Acquire),
                    LookupEvent { key: req.key, code: req.code.to_bytes() },
                );
                return false;
            }
        }
        self.publish(
            shared.event_generation.load(Ordering::Acquire),
            LookupEvent { key: req.key, code: Bytes::new() },
        );
        true
    }

    #[inline]
    fn publish(&mut self, generation: usize, lookup: LookupEvent) {
        if self.producer.try_push(Event { generation, lookup }).is_err() {
            self.dropped += 1;
            // A single writer publishes its plain counter. There is no shared read-modify-write.
            self.telemetry.dropped.store(self.dropped, Ordering::Relaxed);
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.telemetry.cached.store(0, Ordering::Relaxed);
        self.telemetry.final_mark.store(self.producer.position(), Ordering::Relaxed);
        self.telemetry.retired.store(true, Ordering::Release);
    }
}

impl JitBackend {
    #[inline]
    pub(crate) fn with_compiled<R>(
        &self,
        request: LookupRequest<'_>,
        run: impl FnOnce(Option<&CompiledProgram>) -> R,
    ) -> R {
        let mut run = Some(run);
        let result = if self.inner.blocking || !self.inner.tuning.worker_owned_lookup {
            Ok(None)
        } else {
            WORKER.try_with(|slot| {
                let Ok(mut slot) = slot.try_borrow_mut() else { return None };
                if slot.as_ref().is_none_or(|w| w.backend.as_ptr() != Arc::as_ptr(&self.inner)) {
                    *slot = Some(Worker::new(self));
                }
                let worker = slot.as_mut().unwrap();
                let telemetry = Arc::clone(&worker.telemetry);
                let mut cache = telemetry.cache.lock().unwrap_or_else(|e| e.into_inner());
                if telemetry.revoke.load(Ordering::Acquire) {
                    cache.entries.clear();
                }
                let available =
                    !self.inner.shared.execution_workers.revoking.load(Ordering::Acquire)
                        && worker.lookup(self, request, &mut cache);
                let index = (u64::from_le_bytes(request.key.code_hash[..8].try_into().unwrap())
                    as usize
                    ^ request.key.spec_id as usize)
                    & 63;
                let program = cache
                    .entries
                    .get(index)
                    .and_then(Option::as_ref)
                    .filter(|e| e.key == request.key)
                    .map(|e| &*e.program);
                let result = run.take().unwrap()(if available { program } else { None });
                if self.inner.shared.execution_workers.revoking.load(Ordering::Acquire) {
                    telemetry.reclaiming.fetch_add(1, Ordering::AcqRel);
                    let entries = std::mem::take(&mut cache.entries);
                    telemetry.cached.store(0, Ordering::Relaxed);
                    telemetry.revoke.store(false, Ordering::Release);
                    drop(cache);
                    drop(entries);
                    telemetry.reclaiming.fetch_sub(1, Ordering::Release);
                }
                Some(result)
            })
        };
        if let Ok(Some(result)) = result {
            return result;
        }
        // TLS teardown and nested host callbacks may make the local worker unavailable.
        // The owning Arc returned here keeps the program live for the entire nested call.
        let decision = self.lookup(request);
        let program = match &decision {
            LookupDecision::Compiled(program) => Some(&**program),
            LookupDecision::Unavailable(_) => None,
        };
        run.unwrap()(program)
    }
}

#[derive(Default)]
struct QueueStats {
    dropped: AtomicU64,
    lanes: AtomicU64,
    queued: AtomicU64,
    cached: AtomicU64,
    slots: AtomicU64,
    bytes: AtomicU64,
}
