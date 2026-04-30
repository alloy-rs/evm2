use super::{InstrErr, Result};
use alloy_primitives::U256;
use core::{fmt, hint::cold_path};

pub type Word = U256;

pub struct Stack<'a> {
    pub(crate) stack: &'a mut [Word; 1024],
    pub(crate) len: usize,
}

impl fmt::Debug for Stack<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_slice().fmt(f)
    }
}

impl<'a> Stack<'a> {
    #[inline]
    pub(crate) fn new(stack: &'a mut [Word; 1024], len: usize) -> Self {
        Self { stack, len }
    }

    #[inline]
    fn as_slice(&self) -> &[Word] {
        unsafe { core::slice::from_raw_parts(self.stack.as_ptr(), self.len) }
    }

    #[inline]
    pub fn push(&mut self, value: Word) -> Result {
        *self.push_slot()? = value;
        Ok(())
    }

    #[inline]
    pub fn push_slot(&mut self) -> Result<&mut Word> {
        if self.len == 1024 {
            cold_path();
            return Err(InstrErr::StackOverflow);
        }
        let index = self.len;
        self.len += 1;
        Ok(unsafe { self.stack.get_unchecked_mut(index) })
    }

    #[inline]
    pub fn pop(&mut self) -> Result<Word> {
        self.popn().map(|[x]| x)
    }

    #[inline]
    pub fn popn<const N: usize>(&mut self) -> Result<[Word; N]> {
        if self.len < N {
            cold_path();
            return Err(InstrErr::StackUnderflow);
        }
        Ok(unsafe { self.popn_unchecked() })
    }

    #[inline]
    pub unsafe fn popn_unchecked<const N: usize>(&mut self) -> [Word; N] {
        core::array::from_fn(|_| unsafe { self.pop_unchecked() })
    }

    #[inline(always)]
    pub fn popn_top<const N: usize>(&mut self) -> Result<([Word; N], &mut Word)> {
        if self.len < (N + 1) {
            cold_path();
            return Err(InstrErr::StackUnderflow);
        }
        let popped = unsafe { self.popn_unchecked() };
        let top = unsafe { self.top_unchecked() };
        Ok((popped, top))
    }

    #[inline]
    pub unsafe fn top_unchecked(&mut self) -> &mut Word {
        unsafe { self.stack.get_unchecked_mut(self.len - 1) }
    }

    #[inline]
    pub unsafe fn pop_unchecked(&mut self) -> Word {
        self.len -= 1;
        unsafe { *self.stack.get_unchecked(self.len) }
    }
}
