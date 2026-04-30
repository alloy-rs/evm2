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
    pub fn check_bounds(&self, input: usize, output: usize) -> Result {
        if self.len < input {
            cold_path();
            return Err(InstrErr::StackUnderflow);
        }
        if self.len - input + output > 1024 {
            cold_path();
            return Err(InstrErr::StackOverflow);
        }
        Ok(())
    }

    #[inline]
    pub fn push(&mut self, value: Word) -> Result {
        let len = self.len;
        if len == 1024 {
            cold_path();
            return Err(InstrErr::StackOverflow);
        }
        unsafe {
            let end = self.stack.as_mut_ptr().add(len);
            core::ptr::write(end, value);
            self.len = len + 1;
        }
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

    #[inline]
    pub fn dup(&mut self, n: usize) -> Result {
        debug_assert!(n > 0, "attempted to dup 0");
        let len = self.len;
        if (len < n) | (len + 1 > 1024) {
            cold_path();
            return Err(if len == 1024 {
                InstrErr::StackOverflow
            } else {
                InstrErr::StackUnderflow
            });
        }
        unsafe {
            let ptr = self.stack.as_mut_ptr().add(len);
            *ptr = *ptr.sub(n);
            self.len = len + 1;
        }
        Ok(())
    }

    #[inline(always)]
    pub fn swap(&mut self, n: usize) -> Result {
        self.exchange(0, n)
    }

    #[inline]
    pub fn exchange(&mut self, n: usize, m: usize) -> Result {
        debug_assert!(n != m, "overlapping exchange");
        let len = self.len;
        if n >= len || m >= len {
            cold_path();
            return Err(InstrErr::StackUnderflow);
        }
        unsafe {
            let top = self.stack.as_mut_ptr().add(len - 1);
            core::ptr::swap_nonoverlapping(top.sub(n), top.sub(m), 1);
        }
        Ok(())
    }

    #[inline]
    pub fn push_slice(&mut self, slice: &[u8]) -> Result {
        if slice.is_empty() {
            cold_path();
            return Ok(());
        }

        let n_words = slice.len().div_ceil(32);
        let new_len = self.len + n_words;
        if new_len > 1024 {
            cold_path();
            return Err(InstrErr::StackOverflow);
        }

        unsafe {
            let dst = self.stack.as_mut_ptr().add(self.len).cast::<u64>();
            self.len = new_len;

            let mut i = 0;

            let words = slice.chunks_exact(32);
            let partial_last_word = words.remainder();
            for word in words {
                for l in word.rchunks_exact(8) {
                    dst.add(i).write(u64::from_be_bytes(l.try_into().unwrap()));
                    i += 1;
                }
            }

            if partial_last_word.is_empty() {
                return Ok(());
            }

            let limbs = partial_last_word.rchunks_exact(8);
            let partial_last_limb = limbs.remainder();
            for l in limbs {
                dst.add(i).write(u64::from_be_bytes(l.try_into().unwrap()));
                i += 1;
            }

            if !partial_last_limb.is_empty() {
                let mut tmp = [0u8; 8];
                tmp[8 - partial_last_limb.len()..].copy_from_slice(partial_last_limb);
                dst.add(i).write(u64::from_be_bytes(tmp));
                i += 1;
            }

            debug_assert_eq!(i.div_ceil(4), n_words, "wrote too much");

            let m = i % 4;
            if m != 0 {
                dst.add(i).write_bytes(0, 4 - m);
            }
        }

        Ok(())
    }
}
