use super::{InstrErr, Result, Word};
use alloc::vec::Vec;

#[derive(Default)]
pub struct Memory {
    data: Vec<u8>,
}

impl Memory {
    #[inline]
    pub fn new() -> Self {
        Self { data: Vec::new() }
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
    pub fn resize(&mut self, offset: usize, len: usize) -> Result {
        let Some(end) = offset.checked_add(len) else {
            return Err(InstrErr::OutOfGas);
        };
        if end > self.data.len() {
            self.data.resize(end, 0);
        }
        Ok(())
    }

    #[inline]
    pub fn get_word(&mut self, offset: usize) -> Result<Word> {
        self.resize(offset, 32)?;
        Ok(Word::from_be_slice(&self.data[offset..offset + 32]))
    }

    #[inline]
    pub fn set(&mut self, offset: usize, value: &[u8]) -> Result {
        self.resize(offset, value.len())?;
        self.data[offset..offset + value.len()].copy_from_slice(value);
        Ok(())
    }

    #[inline]
    pub fn copy(&mut self, dst: usize, src: usize, len: usize) -> Result {
        if len == 0 {
            return Ok(());
        }
        let max = dst.max(src);
        self.resize(max, len)?;
        self.data.copy_within(src..src + len, dst);
        Ok(())
    }

    #[inline]
    pub fn slice(&mut self, offset: usize, len: usize) -> Result<&[u8]> {
        self.resize(offset, len)?;
        Ok(&self.data[offset..offset + len])
    }
}
