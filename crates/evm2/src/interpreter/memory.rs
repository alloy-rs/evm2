use super::{Gas, InstrStop, MessageInput, Result, Word};
use crate::{utils::num_words, version::GasParams};
use alloc::{sync::Arc, vec::Vec};
use core::{
    cell::UnsafeCell,
    cmp::min,
    fmt,
    hint::cold_path,
    mem,
    ops::Range,
    sync::atomic::{Ordering, fence},
};

/// Linear EVM memory.
pub struct Memory {
    data: Vec<u8>,
    memory_limit: u64,
    /// The cell shared with the inputs lent out of `data`. Allocated by the first lend, and reused
    /// until memory retires its bytes into it.
    cell: Option<Arc<BorrowCell>>,
    /// Whether an input lent out of `data` may still be alive.
    lent: bool,
}

impl Drop for Memory {
    #[inline]
    fn drop(&mut self) {
        self.reclaim_with(false);
    }
}

impl Default for Memory {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for Memory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Memory").field("len", &self.len()).field("data", &self.data).finish()
    }
}

impl Memory {
    /// Creates memory with the default capacity.
    #[inline]
    pub fn new() -> Self {
        Self::with_capacity(4 * 1024)
    }

    /// Creates memory with the requested capacity.
    #[inline]
    pub fn with_capacity(capacity: usize) -> Self {
        Self { data: Vec::with_capacity(capacity), memory_limit: u64::MAX, cell: None, lent: false }
    }

    /// Returns the memory byte limit.
    #[inline]
    pub const fn memory_limit(&self) -> u64 {
        self.memory_limit
    }

    /// Sets the memory byte limit.
    #[inline]
    pub const fn set_memory_limit(&mut self, limit: u64) {
        self.memory_limit = limit;
    }

    /// Returns the memory length in bytes.
    #[inline]
    pub const fn len(&self) -> usize {
        self.data.len()
    }

    /// Returns all initialized memory bytes.
    #[inline]
    pub const fn as_slice(&self) -> &[u8] {
        self.data.as_slice()
    }

    /// Returns a raw pointer to memory bytes.
    #[inline]
    pub const fn as_mut_ptr(&mut self) -> *mut u8 {
        self.data.as_mut_ptr()
    }

    /// Returns whether memory is empty.
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Clears memory while retaining the allocation.
    #[inline]
    pub fn clear(&mut self) {
        self.reclaim_with(false);
        self.data.clear();
    }

    #[inline]
    fn resize_to(&mut self, new_size: usize) {
        self.data.resize(new_size, 0);
    }

    /// Resizes memory to cover `offset..offset + len`.
    ///
    /// Does not account for gas.
    #[inline]
    pub fn resize(&mut self, offset: usize, len: usize) -> Result {
        let Some(end) = offset.checked_add(len) else {
            return Err(InstrStop::MemoryOOG);
        };
        if end > self.data.len() {
            self.resize_to(end);
        }
        Ok(())
    }

    /// Resizes memory to cover `offset..offset + len` and charges EVM expansion gas.
    #[inline]
    pub fn resize_evm(
        &mut self,
        gas: &mut Gas,
        gas_params: &GasParams,
        offset: usize,
        len: usize,
    ) -> Result {
        let new_num_words = num_words(offset.saturating_add(len));
        if new_num_words > gas.memory().words_num {
            return self.resize_evm_cold(gas, gas_params, new_num_words);
        }

        Ok(())
    }

    #[cold]
    #[inline(never)]
    fn resize_evm_cold(
        &mut self,
        gas: &mut Gas,
        gas_params: &GasParams,
        new_num_words: usize,
    ) -> Result {
        let Some(new_size) = new_num_words.checked_mul(32) else {
            cold_path();
            return Err(InstrStop::MemoryOOG);
        };

        if self.limit_reached(new_num_words) {
            cold_path();
            return Err(InstrStop::MemoryLimitOOG);
        }

        let cost = gas_params.memory_cost(new_num_words);
        let cost =
            unsafe { gas.memory_mut().set_words_num(new_num_words, cost).unwrap_unchecked() };

        gas.spend(cost).map_err(|_| InstrStop::MemoryOOG)?;
        self.resize_to(new_size);
        Ok(())
    }

    /// Returns whether `new_words` exceeds the memory limit.
    #[inline]
    pub const fn limit_reached(&self, new_words: usize) -> bool {
        new_words.saturating_mul(32) as u64 > self.memory_limit
    }

    #[inline]
    #[cfg_attr(debug_assertions, track_caller)]
    fn slice_range(&self, range: Range<usize>) -> &[u8] {
        match self.data.get(range.clone()) {
            Some(slice) => slice,
            None => debug_unreachable!("slice OOB: {range:?}; len: {}", self.len()),
        }
    }

    #[inline]
    #[cfg_attr(debug_assertions, track_caller)]
    fn slice_range_mut(&mut self, range: Range<usize>) -> &mut [u8] {
        let len = self.len();
        match self.data.get_mut(range.clone()) {
            Some(slice) => slice,
            None => debug_unreachable!("slice OOB: {range:?}; len: {len}"),
        }
    }

    /// Reads a word from memory.
    ///
    /// # Panics
    ///
    /// Panics if the range is out of bounds.
    #[inline]
    #[cfg_attr(debug_assertions, track_caller)]
    pub fn get_word(&self, offset: usize) -> Word {
        Word::from_be_slice(self.slice_range(offset..offset + 32))
    }

    /// Writes bytes into memory.
    ///
    /// # Panics
    ///
    /// Panics if the range is out of bounds.
    #[inline]
    #[cfg_attr(debug_assertions, track_caller)]
    pub fn set(&mut self, offset: usize, value: &[u8]) {
        if !value.is_empty() {
            self.slice_range_mut(offset..offset + value.len()).copy_from_slice(value);
        }
    }

    /// Writes bytes into memory without bounds checks.
    ///
    /// # Safety
    ///
    /// If `value` is non-empty, `offset..offset + value.len()` must be in bounds.
    #[inline]
    pub(crate) unsafe fn set_unchecked(&mut self, offset: usize, value: &[u8]) {
        if !value.is_empty() {
            unsafe {
                self.data.get_unchecked_mut(offset..offset + value.len()).copy_from_slice(value);
            }
        }
    }

    /// Writes a data slice into memory with zero padding.
    ///
    /// # Panics
    ///
    /// Panics if the range is out of bounds.
    #[inline]
    #[cfg_attr(debug_assertions, track_caller)]
    pub fn set_data(&mut self, memory_offset: usize, data_offset: usize, len: usize, data: &[u8]) {
        unsafe { set_data(&mut self.data, data, memory_offset, data_offset, len) };
    }

    /// Copies bytes within memory.
    ///
    /// # Panics
    ///
    /// Panics if the range is out of bounds.
    #[inline]
    #[cfg_attr(debug_assertions, track_caller)]
    pub fn copy(&mut self, dst: usize, src: usize, len: usize) {
        self.data.copy_within(src..src + len, dst);
    }

    /// Returns a memory slice.
    ///
    /// # Panics
    ///
    /// Panics if the range is out of bounds.
    #[inline]
    #[cfg_attr(debug_assertions, track_caller)]
    pub fn slice(&self, offset: usize, len: usize) -> &[u8] {
        if len == 0 {
            return &[];
        }
        self.slice_range(offset..offset + len)
    }

    /// Lends `offset..offset + len` out as a message input without copying it.
    ///
    /// # Safety
    ///
    /// Memory must not be written or resized until [`Self::reclaim`] runs.
    ///
    /// # Panics
    ///
    /// Panics if the range is out of bounds.
    #[inline]
    #[cfg_attr(debug_assertions, track_caller)]
    pub(crate) unsafe fn lend(&mut self, offset: usize, len: usize) -> MessageInput {
        let cell = Arc::clone(self.cell.get_or_insert_with(Default::default));
        self.lent = true;
        let bytes = self.slice(offset, len);
        // SAFETY: The caller reclaims memory before it changes, which retires the bytes into
        // `cell` if the input is still alive. Dropping memory does the same.
        unsafe { MessageInput::lent(bytes, cell) }
    }

    /// Makes memory writable again after [`Self::lend`]. If a lent input is still alive, memory
    /// hands its bytes over to the input and continues on a copy of them.
    #[inline]
    pub(crate) fn reclaim(&mut self) {
        self.reclaim_with(true);
    }

    #[inline]
    fn reclaim_with(&mut self, keep: bool) {
        if !self.lent {
            return;
        }
        self.lent = false;
        let Some(cell) = &self.cell else { return };
        if Arc::strong_count(cell) == 1 {
            // Synchronizes with the release decrements of inputs dropped on other threads, before
            // memory writes the bytes they read.
            fence(Ordering::Acquire);
            return;
        }
        self.retire(keep);
    }

    /// Hands the bytes over to the inputs lent out of them, and continues on a copy of them if
    /// `keep`, or on empty memory otherwise.
    #[cold]
    #[inline(never)]
    fn retire(&mut self, keep: bool) {
        let old = mem::take(&mut self.data);
        if keep {
            let mut data = Vec::with_capacity(old.capacity());
            data.extend_from_slice(&old);
            self.data = data;
        }
        let cell = self.cell.take().unwrap();
        // SAFETY: Only the memory owning a cell writes `retired`, once, right before it lets go of
        // the cell, and inputs never access it. Moving the `Vec` does not move the bytes.
        unsafe { *cell.retired.get() = old };
    }
}

/// Keeps a [`Memory`]'s bytes alive for the inputs lent out of them after memory lets go of them.
///
/// Every lent input holds the cell of the memory it came from, so the cell's reference count tells
/// memory whether one is still alive. Memory that must write, grow, clear or drop its bytes while
/// one is moves them into `retired` instead, where they stay unchanged until the last input drops
/// the cell. Memory then continues with a new cell.
#[derive(Default)]
pub(crate) struct BorrowCell {
    retired: UnsafeCell<Vec<u8>>,
}

// SAFETY: `retired` is written once, by the memory owning the cell while it still holds a reference
// to it, and is otherwise only dropped with the cell. Inputs never access it.
unsafe impl Sync for BorrowCell {}

unsafe fn set_data(dst: &mut [u8], src: &[u8], dst_offset: usize, src_offset: usize, len: usize) {
    if len == 0 {
        return;
    }
    if src_offset >= src.len() {
        dst.get_mut(dst_offset..dst_offset + len).unwrap().fill(0);
        return;
    }
    let src_end = min(src_offset + len, src.len());
    let src_len = src_end - src_offset;
    debug_assert!(src_offset < src.len() && src_end <= src.len());
    let data = unsafe { src.get_unchecked(src_offset..src_end) };
    unsafe { dst.get_unchecked_mut(dst_offset..dst_offset + src_len).copy_from_slice(data) };
    unsafe { dst.get_unchecked_mut(dst_offset + src_len..dst_offset + len).fill(0) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SpecId, Version};
    use core::assert_matches;

    #[cfg(feature = "std")]
    use std::thread;

    #[test]
    fn test_num_words() {
        assert_eq!(num_words(0), 0);
        assert_eq!(num_words(1), 1);
        assert_eq!(num_words(31), 1);
        assert_eq!(num_words(32), 1);
        assert_eq!(num_words(33), 2);
        assert_eq!(num_words(63), 2);
        assert_eq!(num_words(64), 2);
        assert_eq!(num_words(65), 3);
        assert_eq!(num_words(usize::MAX - 31), usize::MAX / 32);
        assert_eq!(num_words(usize::MAX - 30), (usize::MAX / 32) + 1);
        assert_eq!(num_words(usize::MAX), (usize::MAX / 32) + 1);
    }

    #[test]
    fn resize_evm_accounts_expansion_gas() {
        let mut gas = Gas::new(100);
        let mut memory = Memory::new();

        let gas_params = Version::new(SpecId::FRONTIER).gas_params;
        memory.resize_evm(&mut gas, &gas_params, 0, 32).unwrap();
        assert_eq!(gas.remaining(), 97);
        assert_eq!(gas.memory().words_num, 1);
        assert_eq!(memory.len(), 32);

        let gas_params = Version::new(SpecId::FRONTIER).gas_params;
        memory.resize_evm(&mut gas, &gas_params, 0, 1).unwrap();
        assert_eq!(gas.remaining(), 97);
        assert_eq!(memory.len(), 32);

        let gas_params = Version::new(SpecId::FRONTIER).gas_params;
        memory.resize_evm(&mut gas, &gas_params, 0, 64).unwrap();
        assert_eq!(gas.remaining(), 94);
        assert_eq!(gas.memory().words_num, 2);
        assert_eq!(memory.len(), 64);
    }

    #[test]
    fn resize_evm_respects_memory_limit() {
        let mut gas = Gas::new(100_000);
        let mut memory = Memory::new();
        memory.set_memory_limit(64);

        let gas_params = Version::new(SpecId::FRONTIER).gas_params;
        memory.resize_evm(&mut gas, &gas_params, 0, 32).unwrap();
        assert_eq!(memory.len(), 32);

        memory.resize_evm(&mut gas, &gas_params, 0, 64).unwrap();
        assert_eq!(memory.len(), 64);

        assert_matches!(
            memory.resize_evm(&mut gas, &gas_params, 0, 96),
            Err(InstrStop::MemoryLimitOOG)
        );
        assert_eq!(memory.len(), 64);
        assert_eq!(gas.memory().words_num, 2);
    }

    fn memory_with(bytes: &[u8]) -> Memory {
        let mut memory = Memory::new();
        memory.resize(0, bytes.len()).unwrap();
        memory.set(0, bytes);
        memory
    }

    #[test]
    fn dropped_lent_input_leaves_memory_in_place() {
        let mut memory = memory_with(&[1; 64]);
        let ptr = memory.as_slice().as_ptr();
        // SAFETY: Memory is reclaimed before it is written.
        let input = unsafe { memory.lend(0, 64) };
        #[cfg(feature = "std")]
        thread::spawn(move || assert_eq!(&*input, [1; 64])).join().unwrap();
        #[cfg(not(feature = "std"))]
        drop(input);

        memory.reclaim();
        memory.set(0, &[2; 32]);
        assert_eq!(memory.as_slice().as_ptr(), ptr);
        assert_eq!(memory.slice(31, 2), [2, 1]);
    }

    #[test]
    fn kept_lent_input_keeps_its_bytes() {
        let ops: [fn(&mut Memory); 3] = [
            |memory| {
                memory.reclaim();
                memory.set(0, &[2; 32]);
                memory.resize(0, 1 << 20).unwrap();
            },
            |memory| memory.clear(),
            |memory| drop(core::mem::take(memory)),
        ];
        for op in ops {
            let mut memory = memory_with(&[1; 64]);
            // SAFETY: Memory is reclaimed, cleared or dropped before it is written.
            let input = unsafe { memory.lend(16, 32) };
            op(&mut memory);
            assert_eq!(&*input, [1; 32]);
            // Memory lends again from a new cell.
            memory.resize(0, 64).unwrap();
            // SAFETY: Memory is not written before it is dropped.
            let again = unsafe { memory.lend(0, 64) };
            drop(memory);
            assert_eq!(&*input, [1; 32]);
            assert_eq!(again.len(), 64);
        }
    }

    #[test]
    fn kept_copies_of_lent_input_own_their_bytes() {
        let mut memory = memory_with(&[1, 2, 3]);
        // SAFETY: Memory is reclaimed before it is written.
        let input = unsafe { memory.lend(0, 3) };
        let clone = input.clone();
        let bytes = input.to_bytes();
        // SAFETY: As above.
        let mut owned = unsafe { memory.lend(0, 3) };
        owned.make_owned();
        drop(input);

        memory.reclaim();
        memory.set(0, &[0; 3]);
        assert_eq!(&*clone, [1, 2, 3]);
        assert_eq!(bytes[..], [1, 2, 3]);
        assert_eq!(&*owned, [1, 2, 3]);
    }
}
