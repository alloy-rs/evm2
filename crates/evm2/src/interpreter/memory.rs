use super::{InstrErr, Result, Word};
use alloc::vec::Vec;
use core::{cmp::min, fmt, ops::Range};

#[derive(Default)]
pub struct Memory {
    data: Vec<u8>,
}

impl fmt::Debug for Memory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Memory").field("len", &self.len()).field("data", &self.data).finish()
    }
}

impl Memory {
    #[inline]
    pub fn new() -> Self {
        Self::with_capacity(4 * 1024)
    }

    #[inline]
    pub fn with_capacity(capacity: usize) -> Self {
        Self { data: Vec::with_capacity(capacity) }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.data.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    #[inline]
    fn resize_to(&mut self, new_size: usize) {
        self.data.resize(new_size, 0);
    }

    #[inline]
    fn resize_for(&mut self, offset: usize, len: usize) -> Result {
        let Some(end) = offset.checked_add(len) else {
            return Err(InstrErr::OutOfGas);
        };
        if end > self.data.len() {
            self.resize_to(end);
        }
        Ok(())
    }

    #[inline]
    pub fn resize(&mut self, offset: usize, len: usize) -> Result {
        self.resize_for(offset, len)
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

    #[inline]
    #[cfg_attr(debug_assertions, track_caller)]
    pub fn get_word(&mut self, offset: usize) -> Result<Word> {
        self.resize_for(offset, 32)?;
        Ok(Word::from_be_slice(self.slice_range(offset..offset + 32)))
    }

    #[inline]
    #[cfg_attr(debug_assertions, track_caller)]
    pub fn set(&mut self, offset: usize, value: &[u8]) -> Result {
        if value.is_empty() {
            return Ok(());
        }
        self.resize_for(offset, value.len())?;
        self.slice_range_mut(offset..offset + value.len()).copy_from_slice(value);
        Ok(())
    }

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
        self.resize_for(memory_offset, len)?;
        unsafe { set_data(&mut self.data, data, memory_offset, data_offset, len) };
        Ok(())
    }

    #[inline]
    #[cfg_attr(debug_assertions, track_caller)]
    pub fn copy(&mut self, dst: usize, src: usize, len: usize) -> Result {
        if len == 0 {
            return Ok(());
        }
        let max = dst.max(src);
        self.resize_for(max, len)?;
        self.data.copy_within(src..src + len, dst);
        Ok(())
    }

    #[inline]
    #[cfg_attr(debug_assertions, track_caller)]
    pub fn slice(&mut self, offset: usize, len: usize) -> Result<&[u8]> {
        if len == 0 {
            return Ok(&[]);
        }
        self.resize_for(offset, len)?;
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
