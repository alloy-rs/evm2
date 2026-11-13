//! Bounded single-producer/single-consumer ring with cache-line-isolated cursors.

use std::{
    cell::{Cell, UnsafeCell},
    marker::PhantomData,
    mem::MaybeUninit,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

#[repr(align(128))]
struct Cursor(AtomicUsize);

struct Shared<T> {
    published: Cursor,
    released: Cursor,
    slots: Box<[UnsafeCell<MaybeUninit<T>>]>,
}

// SAFETY: The unique producer and consumer transfer exclusive slot ownership through the
// release/acquire cursors. Only an unpublished slot is written, and only a published slot is read.
unsafe impl<T: Send> Sync for Shared<T> {}

// SAFETY: Moving the backing allocation does not change slot ownership. Its contents may move
// between producer and consumer threads only when T is Send.
unsafe impl<T: Send> Send for Shared<T> {}

pub(crate) struct Producer<T> {
    shared: Arc<Shared<T>>,
    sequence: usize,
    released: usize,
    // Endpoints are movable, but sharing an endpoint must not create another producer/consumer.
    _not_sync: PhantomData<Cell<()>>,
}

pub(crate) struct Consumer<T> {
    shared: Arc<Shared<T>>,
    sequence: usize,
    published: usize,
    _not_sync: PhantomData<Cell<()>>,
}

pub(crate) fn channel<T: Send>(capacity: usize) -> (Producer<T>, Consumer<T>) {
    assert!(capacity.is_power_of_two() && capacity <= isize::MAX as usize);
    let shared = Arc::new(Shared {
        published: Cursor(AtomicUsize::new(0)),
        released: Cursor(AtomicUsize::new(0)),
        slots: (0..capacity)
            .map(|_| UnsafeCell::new(MaybeUninit::uninit()))
            .collect::<Vec<_>>()
            .into_boxed_slice(),
    });
    (
        Producer { shared: Arc::clone(&shared), sequence: 0, released: 0, _not_sync: PhantomData },
        Consumer { shared, sequence: 0, published: 0, _not_sync: PhantomData },
    )
}

impl<T> Producer<T> {
    pub(crate) const fn position(&self) -> usize {
        self.sequence
    }

    #[inline]
    pub(crate) fn try_push(&mut self, value: T) -> Result<(), T> {
        let capacity = self.shared.slots.len();
        if self.sequence.wrapping_sub(self.released) == capacity {
            self.released = self.shared.released.0.load(Ordering::Acquire);
            if self.sequence.wrapping_sub(self.released) == capacity {
                return Err(value);
            }
        }
        let index = self.sequence & (capacity - 1);
        // SAFETY: This slot is free according to the acquired consumer cursor. No consumer may
        // access it again until the release-store below publishes its fully initialized payload.
        unsafe { (*self.shared.slots.get_unchecked(index).get()).write(value) };
        self.sequence = self.sequence.wrapping_add(1);
        self.shared.published.0.store(self.sequence, Ordering::Release);
        Ok(())
    }
}

impl<T> Consumer<T> {
    #[inline]
    pub(crate) fn pop(&mut self) -> Option<T> {
        if self.sequence == self.published {
            self.published = self.shared.published.0.load(Ordering::Acquire);
            if self.sequence == self.published {
                return None;
            }
        }
        let index = self.sequence & (self.shared.slots.len() - 1);
        // SAFETY: The acquired producer cursor publishes this initialized slot. Reading moves
        // its value out before the release-store makes that storage available to the producer.
        let value = unsafe { (*self.shared.slots.get_unchecked(index).get()).assume_init_read() };
        self.sequence = self.sequence.wrapping_add(1);
        self.shared.released.0.store(self.sequence, Ordering::Release);
        Some(value)
    }
}

impl<T> Consumer<T> {
    pub(crate) const fn position(&self) -> usize {
        self.sequence
    }

    pub(crate) fn len(&self) -> usize {
        self.shared.published.0.load(Ordering::Acquire).wrapping_sub(self.sequence)
    }
}

impl<T> Drop for Shared<T> {
    fn drop(&mut self) {
        // Both endpoints are gone, so cursors and occupied slots can no longer change.
        let released = *self.released.0.get_mut();
        let published = *self.published.0.get_mut();
        for offset in 0..published.wrapping_sub(released) {
            let index = released.wrapping_add(offset) & (self.slots.len() - 1);
            // SAFETY: Only slots between the released and published cursors contain values.
            unsafe { self.slots[index].get_mut().assume_init_drop() };
        }
    }
}
