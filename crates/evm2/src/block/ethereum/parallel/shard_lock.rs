//! A reader-writer lock whose registered readers write only a cache line of their own, so reads
//! of a lock from several threads do not move its cache line between them.

use super::CachePadded;
use core::{
    cell::{Cell, UnsafeCell},
    hint,
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

/// Spins of a waiting thread before it yields.
const SPINS: u32 = 1 << 7;

std::thread_local! {
    /// The [`Readers`] the thread registered with, by id, and its slot there.
    static READER: Cell<(usize, usize)> = const { Cell::new((usize::MAX, 0)) };
}

/// The reader slots of a set of [`ShardLock`]s. A registered thread announces the tag of the lock
/// it reads in its own slot, which a writer of that lock waits to see cleared. Other threads, and
/// a registered thread already reading a lock, count themselves in the lock instead.
pub(super) struct Readers {
    id: usize,
    slots: Box<[CachePadded<AtomicUsize>]>,
}

impl Readers {
    /// Returns `len` reader slots, identified by `id` among every [`Readers`] alive.
    pub(super) fn new(id: usize, len: usize) -> Self {
        Self { id, slots: (0..len).map(|_| CachePadded(AtomicUsize::new(0))).collect() }
    }

    /// Registers the calling thread as the reader of `slot`, when there is one. No other thread
    /// may register in the same slot.
    pub(super) fn register(&self, slot: usize) {
        if slot < self.slots.len() {
            READER.set((self.id, slot));
        }
    }

    /// Returns the calling thread's slot, if it registered and reads no lock.
    #[inline]
    fn free_slot(&self) -> Option<&AtomicUsize> {
        let (id, slot) = READER.get();
        let slot = &**self.slots.get(slot).filter(|_| id == self.id)?;
        (slot.load(Ordering::Relaxed) == 0).then_some(slot)
    }
}

/// A reader-writer lock over a value, read and written through the [`Readers`] of its set.
pub(super) struct ShardLock<T> {
    /// Identifies the lock in reader slots: nonzero and unique among the locks of one
    /// [`Readers`].
    tag: usize,
    /// Whether a writer holds or waits for the lock.
    writer: AtomicBool,
    /// Readers holding the lock outside a reader slot.
    shared: AtomicUsize,
    value: UnsafeCell<T>,
}

// SAFETY: the lock hands out shared references to readers and exclusive ones to one writer.
unsafe impl<T: Send + Sync> Sync for ShardLock<T> {}

impl<T> ShardLock<T> {
    /// Returns a lock over `value`, identified by the nonzero `tag`.
    pub(super) const fn new(tag: usize, value: T) -> Self {
        assert!(tag != 0, "a zero tag marks a free reader slot");
        Self {
            tag,
            writer: AtomicBool::new(false),
            shared: AtomicUsize::new(0),
            value: UnsafeCell::new(value),
        }
    }

    /// Locks the value to read it.
    #[inline]
    pub(super) fn read<'l>(&'l self, readers: &'l Readers) -> ReadGuard<'l, T> {
        // Announcing the read and then checking for a writer, each sequentially consistent,
        // pairs with the writer announcing itself and then checking for readers: one of the two
        // sees the other.
        if let Some(slot) = readers.free_slot() {
            loop {
                slot.swap(self.tag, Ordering::SeqCst);
                if !self.writer.load(Ordering::SeqCst) {
                    return ReadGuard { lock: self, slot: Some(slot) };
                }
                slot.store(0, Ordering::Release);
                wait_while(|| self.writer.load(Ordering::Relaxed));
            }
        }
        loop {
            self.shared.fetch_add(1, Ordering::SeqCst);
            if !self.writer.load(Ordering::SeqCst) {
                return ReadGuard { lock: self, slot: None };
            }
            self.shared.fetch_sub(1, Ordering::Release);
            wait_while(|| self.writer.load(Ordering::Relaxed));
        }
    }

    /// Locks the value to write it, once every reader left.
    pub(super) fn write<'l>(&'l self, readers: &'l Readers) -> WriteGuard<'l, T> {
        let mut spins = 0;
        while self
            .writer
            .compare_exchange_weak(false, true, Ordering::SeqCst, Ordering::Relaxed)
            .is_err()
        {
            backoff(&mut spins);
        }
        for slot in &readers.slots {
            wait_while(|| slot.load(Ordering::SeqCst) == self.tag);
        }
        wait_while(|| self.shared.load(Ordering::SeqCst) != 0);
        WriteGuard { lock: self }
    }
}

/// Shared access to a [`ShardLock`]'s value.
pub(super) struct ReadGuard<'l, T> {
    lock: &'l ShardLock<T>,
    /// The reader slot announcing the read, or `None` when the lock counts it.
    slot: Option<&'l AtomicUsize>,
}

impl<T> Deref for ReadGuard<'_, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        // SAFETY: no writer holds the lock while a reader does.
        unsafe { &*self.lock.value.get() }
    }
}

impl<T> Drop for ReadGuard<'_, T> {
    #[inline]
    fn drop(&mut self) {
        match self.slot {
            Some(slot) => slot.store(0, Ordering::Release),
            None => {
                self.lock.shared.fetch_sub(1, Ordering::Release);
            }
        }
    }
}

/// Exclusive access to a [`ShardLock`]'s value.
pub(super) struct WriteGuard<'l, T> {
    lock: &'l ShardLock<T>,
}

impl<T> Deref for WriteGuard<'_, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        // SAFETY: the writer holds the lock alone.
        unsafe { &*self.lock.value.get() }
    }
}

impl<T> DerefMut for WriteGuard<'_, T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: the writer holds the lock alone.
        unsafe { &mut *self.lock.value.get() }
    }
}

impl<T> Drop for WriteGuard<'_, T> {
    #[inline]
    fn drop(&mut self) {
        self.lock.writer.store(false, Ordering::Release);
    }
}

/// Spins, then yields, while `condition` holds.
#[inline]
fn wait_while(mut condition: impl FnMut() -> bool) {
    let mut spins = 0;
    while condition() {
        backoff(&mut spins);
    }
}

/// Spins on each of the first [`SPINS`] calls, then yields.
#[inline]
fn backoff(spins: &mut u32) {
    if *spins < SPINS {
        *spins += 1;
        hint::spin_loop();
    } else {
        std::thread::yield_now();
    }
}
