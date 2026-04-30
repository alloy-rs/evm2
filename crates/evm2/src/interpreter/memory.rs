use super::{Gas, InstrErr, Result, Word};
use alloc::vec::Vec;
use core::{cmp::min, fmt, hint::cold_path, ops::Range};

/// Linear EVM memory.
pub struct Memory {
    data: Vec<u8>,
    memory_limit: u64,
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
        Self { data: Vec::with_capacity(capacity), memory_limit: u64::MAX }
    }

    /// Creates memory with a byte limit.
    #[inline]
    pub fn new_with_memory_limit(memory_limit: u64) -> Self {
        Self { memory_limit, ..Self::new() }
    }

    /// Sets the memory byte limit.
    #[inline]
    pub const fn set_memory_limit(&mut self, limit: u64) {
        self.memory_limit = limit;
    }

    /// Returns the memory length in bytes.
    #[inline]
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Returns whether memory is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    #[inline]
    fn resize_to(&mut self, new_size: usize) {
        self.data.resize(new_size, 0);
    }

    /// Resizes memory to cover `offset..offset + len`.
    #[inline]
    pub fn resize(&mut self, offset: usize, len: usize) -> Result {
        let Some(end) = offset.checked_add(len) else {
            return Err(InstrErr::OutOfGas);
        };
        if end > self.data.len() {
            self.resize_to(end);
        }
        Ok(())
    }

    /// Returns whether `new_words` exceeds the memory limit.
    #[inline]
    pub fn limit_reached(&self, new_words: usize) -> bool {
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
    #[inline]
    #[cfg_attr(debug_assertions, track_caller)]
    pub fn get_word(&mut self, offset: usize) -> Result<Word> {
        self.resize(offset, 32)?;
        Ok(Word::from_be_slice(self.slice_range(offset..offset + 32)))
    }

    /// Writes bytes into memory.
    #[inline]
    #[cfg_attr(debug_assertions, track_caller)]
    pub fn set(&mut self, offset: usize, value: &[u8]) -> Result {
        if value.is_empty() {
            return Ok(());
        }
        self.resize(offset, value.len())?;
        self.slice_range_mut(offset..offset + value.len()).copy_from_slice(value);
        Ok(())
    }

    /// Writes a data slice into memory with zero padding.
    #[inline]
    #[cfg_attr(debug_assertions, track_caller)]
    pub fn set_data(
        &mut self,
        memory_offset: usize,
        data_offset: usize,
        len: usize,
        data: &[u8],
    ) -> Result {
        if len == 0 {
            return Ok(());
        }
        self.resize(memory_offset, len)?;
        unsafe { set_data(&mut self.data, data, memory_offset, data_offset, len) };
        Ok(())
    }

    /// Copies bytes within memory.
    #[inline]
    #[cfg_attr(debug_assertions, track_caller)]
    pub fn copy(&mut self, dst: usize, src: usize, len: usize) -> Result {
        if len == 0 {
            return Ok(());
        }
        let max = dst.max(src);
        self.resize(max, len)?;
        self.data.copy_within(src..src + len, dst);
        Ok(())
    }

    /// Returns a memory slice.
    #[inline]
    #[cfg_attr(debug_assertions, track_caller)]
    pub fn slice(&mut self, offset: usize, len: usize) -> Result<&[u8]> {
        if len == 0 {
            return Ok(&[]);
        }
        self.resize(offset, len)?;
        Ok(self.slice_range(offset..offset + len))
    }
}

unsafe fn set_data(dst: &mut [u8], src: &[u8], dst_offset: usize, src_offset: usize, len: usize) {
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

#[inline]
pub(super) const fn num_words(len: usize) -> usize {
    len.div_ceil(32)
}

#[inline]
pub(super) fn memory_cost(len: usize) -> u64 {
    let len = len as u64;
    3_u64.saturating_mul(len).saturating_add(len.saturating_mul(len) / 512)
}

#[inline]
pub(super) fn resize_memory(
    gas: &mut Gas,
    memory: &mut Memory,
    offset: usize,
    len: usize,
) -> Result {
    let new_num_words = num_words(offset.saturating_add(len));
    if new_num_words > gas.memory().words_num {
        return resize_memory_cold(gas, memory, new_num_words);
    }

    Ok(())
}

#[cold]
#[inline(never)]
fn resize_memory_cold(gas: &mut Gas, memory: &mut Memory, new_num_words: usize) -> Result {
    let Some(new_size) = new_num_words.checked_mul(32) else {
        cold_path();
        return Err(InstrErr::OutOfGas);
    };

    if memory.limit_reached(new_num_words) {
        cold_path();
        return Err(InstrErr::OutOfGas);
    }

    let cost = memory_cost(new_num_words);
    let cost = unsafe { gas.memory_mut().set_words_num(new_num_words, cost).unwrap_unchecked() };

    if !gas.record_regular_cost(cost) {
        cold_path();
        return Err(InstrErr::OutOfGas);
    }
    memory.resize_to(new_size);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_memory_accounts_expansion_gas() {
        let mut gas = Gas::new(100);
        let mut memory = Memory::new();

        resize_memory(&mut gas, &mut memory, 0, 32).unwrap();
        assert_eq!(gas.remaining(), 97);
        assert_eq!(gas.memory().words_num, 1);
        assert_eq!(memory.len(), 32);

        resize_memory(&mut gas, &mut memory, 0, 1).unwrap();
        assert_eq!(gas.remaining(), 97);
        assert_eq!(memory.len(), 32);

        resize_memory(&mut gas, &mut memory, 0, 64).unwrap();
        assert_eq!(gas.remaining(), 94);
        assert_eq!(gas.memory().words_num, 2);
        assert_eq!(memory.len(), 64);
    }

    #[test]
    fn resize_memory_respects_memory_limit() {
        let mut gas = Gas::new(100);
        let mut memory = Memory::new_with_memory_limit(32);

        resize_memory(&mut gas, &mut memory, 0, 32).unwrap();
        assert!(matches!(resize_memory(&mut gas, &mut memory, 0, 64), Err(InstrErr::OutOfGas)));
        assert_eq!(memory.len(), 32);
        assert_eq!(gas.memory().words_num, 1);
    }
}
