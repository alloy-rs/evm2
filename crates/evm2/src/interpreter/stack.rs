use super::{InstrStop, Result};
use crate::constants::STACK_LIMIT;
use alloy_primitives::U256;
use core::{fmt, hint::cold_path};

/// EVM stack word.
pub type Word = U256;

/// Mutable EVM operand stack.
pub struct Stack<'a> {
    pub(crate) stack: &'a mut [Word; STACK_LIMIT],
    pub(crate) len: usize,
}

/// Borrowed mutable EVM operand stack.
pub struct StackMut<'a> {
    pub(crate) stack: &'a mut [Word; STACK_LIMIT],
    pub(crate) len: &'a mut usize,
}

impl fmt::Debug for Stack<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_slice().fmt(f)
    }
}

impl fmt::Debug for StackMut<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_slice().fmt(f)
    }
}

impl<'a> Stack<'a> {
    pub(crate) const CAPACITY: usize = STACK_LIMIT;

    #[inline]
    pub(crate) const fn new(stack: &'a mut [Word; Stack::CAPACITY], len: usize) -> Self {
        debug_assert!(len <= Self::CAPACITY);
        Self { stack, len }
    }

    #[inline]
    #[cfg(not(feature = "tco"))]
    pub(crate) const fn reborrow(&mut self) -> Stack<'_> {
        Stack { stack: self.stack, len: self.len }
    }

    #[inline]
    pub(crate) const fn as_mut(&mut self) -> StackMut<'_> {
        StackMut { stack: self.stack, len: &mut self.len }
    }

    /// Returns the stack length.
    #[inline]
    pub const fn len(&self) -> usize {
        let len = self.len;
        // SAFETY: Type invariant. Can be broken on overflow/underflow, so it's on the caller's to
        // not call this after.
        unsafe { core::hint::assert_unchecked(len <= Self::CAPACITY) };
        len
    }

    /// Returns whether the stack is empty.
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns the stack contents as a slice.
    #[inline]
    pub const fn as_slice(&self) -> &[Word] {
        unsafe { core::slice::from_raw_parts(self.stack.as_ptr(), self.len()) }
    }
}

impl<'a> StackMut<'a> {
    pub(crate) const CAPACITY: usize = STACK_LIMIT;

    #[inline]
    #[cfg(test)]
    pub(crate) const fn new(stack: &'a mut [Word; StackMut::CAPACITY], len: &'a mut usize) -> Self {
        debug_assert!(*len <= Self::CAPACITY);
        Self { stack, len }
    }

    /// Returns the stack length.
    #[inline]
    pub const fn len(&self) -> usize {
        let len = *self.len;
        // SAFETY: Type invariant. Can be broken on overflow/underflow, so it's on the caller's to
        // not call this after.
        unsafe { core::hint::assert_unchecked(len <= Self::CAPACITY) };
        len
    }

    /// Returns whether the stack is empty.
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns the stack contents as a slice.
    #[inline]
    pub const fn as_slice(&self) -> &[Word] {
        unsafe { core::slice::from_raw_parts(self.stack.as_ptr(), self.len()) }
    }

    /// Returns the stack contents as a slice.
    #[inline]
    pub const fn as_slice_mut(&mut self) -> &mut [Word] {
        unsafe { core::slice::from_raw_parts_mut(self.stack.as_mut_ptr(), self.len()) }
    }

    /// Checks that an instruction can consume `input` words and produce `output` words.
    #[inline(always)]
    #[cfg(test)]
    pub(crate) fn check_bounds(&self, input: usize, output: usize) -> Result {
        Self::check_bounds_(self.len(), input, output)
    }

    #[inline(always)]
    fn check_bounds_(len: usize, input: usize, output: usize) -> Result {
        debug_assert!(len <= Self::CAPACITY);
        core::debug_assert_matches!(output, 0 | 1);
        if len < input {
            cold_path();
            return Err(InstrStop::StackUnderflow);
        }
        if output > input && len - input == Self::CAPACITY {
            cold_path();
            return Err(InstrStop::StackOverflow);
        }
        Ok(())
    }

    /// Checks that an instruction can consume `input` words and produce `output` words.
    #[inline(always)]
    pub(crate) fn instr_stack_setup(&mut self, input: usize, output: usize) -> Result<*mut Word> {
        let len = self.len();
        // SAFETY: Assumes that the stack is never used after execution fails.
        *self.len = len.wrapping_sub(input).wrapping_add(output);
        Self::check_bounds_(len, input, output)?;
        debug_assert!(*self.len <= Self::CAPACITY);
        Ok(unsafe { self.stack.as_mut_ptr().add(len).sub(input) })
    }

    /// Pushes a word onto the stack.
    #[inline]
    pub fn push(&mut self, value: Word) -> Result {
        let len = self.len();
        if len == Self::CAPACITY {
            cold_path();
            return Err(InstrStop::StackOverflow);
        }
        unsafe {
            let end = self.stack.as_mut_ptr().add(len);
            *end = value;
            *self.len = len + 1;
            debug_assert!(*self.len <= Self::CAPACITY);
        }
        Ok(())
    }

    /// Pops one word from the stack.
    #[inline]
    pub fn pop(&mut self) -> Result<Word> {
        self.popn().map(|[x]| x)
    }

    /// Pops `N` words from the stack.
    #[inline]
    pub fn popn<const N: usize>(&mut self) -> Result<[Word; N]> {
        if self.len() < N {
            cold_path();
            return Err(InstrStop::StackUnderflow);
        }
        Ok(unsafe { self.popn_unchecked() })
    }

    /// Pops `n` words from the stack and returns an iterator over the popped words.
    #[inline]
    pub fn popn_dyn(&mut self, n: usize) -> Result<impl Iterator<Item = Word> + '_> {
        if self.len() < n {
            cold_path();
            return Err(InstrStop::StackUnderflow);
        }
        Ok((0..n).map(move |_| unsafe { self.pop_unchecked() }))
    }

    /// # Safety
    ///
    /// Caller must ensure the stack contains at least `N` initialized words.
    #[inline]
    pub unsafe fn popn_unchecked<const N: usize>(&mut self) -> [Word; N] {
        debug_assert!(self.len() >= N);
        core::array::from_fn(|_| unsafe { self.pop_unchecked() })
    }

    /// Pops `N` words and returns the new top word.
    #[inline(always)]
    pub fn popn_top<const N: usize>(&mut self) -> Result<([Word; N], &mut Word)> {
        if self.len() < (N + 1) {
            cold_path();
            return Err(InstrStop::StackUnderflow);
        }
        let popped = unsafe { self.popn_unchecked() };
        let top = unsafe { self.top_unchecked() };
        Ok((popped, top))
    }

    /// # Safety
    ///
    /// Caller must ensure the stack is not empty.
    #[inline]
    pub unsafe fn top_unchecked(&mut self) -> &mut Word {
        let len = self.len();
        debug_assert!(len > 0);
        unsafe { self.stack.get_unchecked_mut(len - 1) }
    }

    /// # Safety
    ///
    /// Caller must ensure the stack is not empty.
    #[inline]
    pub unsafe fn pop_unchecked(&mut self) -> Word {
        debug_assert!(!self.is_empty());
        *self.len -= 1;
        unsafe { *self.stack.get_unchecked(self.len()) }
    }

    /// Duplicates the `n`th stack word from the top.
    #[inline]
    pub fn dup(&mut self, n: usize) -> Result {
        debug_assert!(n > 0, "attempted to dup 0");
        let len = self.len();
        if (len < n) | (len == Self::CAPACITY) {
            cold_path();
            return Err(if len == Self::CAPACITY {
                InstrStop::StackOverflow
            } else {
                InstrStop::StackUnderflow
            });
        }
        unsafe {
            let ptr = self.stack.as_mut_ptr().add(len);
            *ptr = *ptr.sub(n);
            *self.len = len + 1;
            debug_assert!(*self.len <= Self::CAPACITY);
        }
        Ok(())
    }

    /// Swaps the top word with the `n`th word below it.
    #[inline(always)]
    pub fn swap(&mut self, n: usize) -> Result {
        self.exchange(0, n)
    }

    /// Exchanges the `n`th and `m`th words below the top.
    #[inline]
    pub fn exchange(&mut self, n: usize, m: usize) -> Result {
        debug_assert!(n != m, "overlapping exchange");
        let len = self.len();
        if n >= len || m >= len {
            cold_path();
            return Err(InstrStop::StackUnderflow);
        }
        unsafe {
            let top = self.stack.as_mut_ptr().add(len - 1);
            core::ptr::swap_nonoverlapping(top.sub(n), top.sub(m), 1);
        }
        Ok(())
    }

    /// Pushes big-endian bytes as stack words.
    #[inline]
    pub fn push_slice(&mut self, slice: &[u8]) -> Result {
        if slice.is_empty() {
            cold_path();
            return Ok(());
        }

        let n_words = slice.len().div_ceil(32);
        let len = self.len();
        let new_len = len + n_words;
        if new_len > Self::CAPACITY {
            cold_path();
            return Err(InstrStop::StackOverflow);
        }

        unsafe {
            let dst = self.stack.as_mut_ptr().add(len).cast::<u64>();
            *self.len = new_len;
            debug_assert!(*self.len <= Self::CAPACITY);

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

#[cfg(test)]
mod tests {
    use super::*;

    fn run(f: impl FnOnce(&mut StackMut<'_>)) {
        let mut backing = [Word::MAX; StackMut::CAPACITY];
        let mut len = 0;
        let mut stack = StackMut::new(&mut backing, &mut len);
        f(&mut stack);
    }

    fn run_with_len(len: usize, f: impl FnOnce(&mut StackMut<'_>)) {
        let mut backing = [Word::MAX; StackMut::CAPACITY];
        for (i, word) in backing.iter_mut().take(len).enumerate() {
            *word = Word::from(i);
        }
        let mut len = len;
        let mut stack = StackMut::new(&mut backing, &mut len);
        f(&mut stack);
    }

    #[test]
    fn check_bounds() {
        run_with_len(0, |stack| {
            assert!(stack.check_bounds(0, 0).is_ok());
            assert!(stack.check_bounds(0, 1).is_ok());
            core::assert_matches!(stack.check_bounds(1, 0), Err(InstrStop::StackUnderflow));
            core::assert_matches!(stack.check_bounds(1, 1), Err(InstrStop::StackUnderflow));
        });

        run_with_len(1, |stack| {
            assert!(stack.check_bounds(1, 0).is_ok());
            assert!(stack.check_bounds(1, 1).is_ok());
            core::assert_matches!(stack.check_bounds(2, 0), Err(InstrStop::StackUnderflow));
        });

        run_with_len(StackMut::CAPACITY, |stack| {
            assert!(stack.check_bounds(1, 1).is_ok());
            core::assert_matches!(stack.check_bounds(0, 1), Err(InstrStop::StackOverflow));
        });
    }

    #[test]
    fn push_and_pop() {
        run(|stack| {
            stack.push(Word::from(1)).unwrap();
            stack.push(Word::from(2)).unwrap();
            assert_eq!(stack.as_slice(), [Word::from(1), Word::from(2)]);
            assert_eq!(stack.pop().unwrap(), Word::from(2));
            assert_eq!(stack.popn::<1>().unwrap(), [Word::from(1)]);
            core::assert_matches!(stack.pop(), Err(InstrStop::StackUnderflow));
        });

        run_with_len(StackMut::CAPACITY, |stack| {
            core::assert_matches!(stack.push(Word::ZERO), Err(InstrStop::StackOverflow));
        });
    }

    #[test]
    fn popn_top() {
        run_with_len(3, |stack| {
            let (popped, top) = stack.popn_top::<2>().unwrap();
            assert_eq!(popped, [Word::from(2), Word::from(1)]);
            assert_eq!(*top, Word::from(0));
            *top = Word::from(9);
            assert_eq!(stack.as_slice(), [Word::from(9)]);
        });

        run_with_len(2, |stack| {
            core::assert_matches!(stack.popn_top::<2>(), Err(InstrStop::StackUnderflow));
        });
    }

    #[test]
    fn popn_dyn() {
        run_with_len(4, |stack| {
            let popped = stack.popn_dyn(3).unwrap().collect::<alloc::vec::Vec<_>>();
            assert_eq!(popped, [Word::from(3), Word::from(2), Word::from(1)]);
            assert_eq!(stack.as_slice(), [Word::from(0)]);
        });

        run_with_len(2, |stack| {
            core::assert_matches!(stack.popn_dyn(3).map(|_| ()), Err(InstrStop::StackUnderflow));
            assert_eq!(stack.as_slice(), [Word::from(0), Word::from(1)]);
        });
    }

    #[test]
    fn dup_swap_and_exchange() {
        run_with_len(4, |stack| {
            stack.dup(2).unwrap();
            assert_eq!(
                stack.as_slice(),
                [Word::from(0), Word::from(1), Word::from(2), Word::from(3), Word::from(2)]
            );

            stack.swap(3).unwrap();
            assert_eq!(
                stack.as_slice(),
                [Word::from(0), Word::from(2), Word::from(2), Word::from(3), Word::from(1)]
            );

            stack.exchange(1, 4).unwrap();
            assert_eq!(
                stack.as_slice(),
                [Word::from(3), Word::from(2), Word::from(2), Word::from(0), Word::from(1)]
            );
        });

        run_with_len(1, |stack| {
            core::assert_matches!(stack.dup(2), Err(InstrStop::StackUnderflow));
            core::assert_matches!(stack.swap(1), Err(InstrStop::StackUnderflow));
            core::assert_matches!(stack.exchange(0, 1), Err(InstrStop::StackUnderflow));
        });

        run_with_len(StackMut::CAPACITY, |stack| {
            core::assert_matches!(stack.dup(1), Err(InstrStop::StackOverflow));
        });
    }

    #[test]
    fn push_slices() {
        run(|stack| {
            stack.push_slice(b"").unwrap();
            assert!(stack.as_slice().is_empty());
        });

        run(|stack| {
            stack.push_slice(&[42]).unwrap();
            assert_eq!(stack.as_slice(), [Word::from(42)]);
        });

        let n = 0x1111_2222_3333_4444_5555_6666_7777_8888_u128;
        run(|stack| {
            stack.push_slice(&n.to_be_bytes()).unwrap();
            assert_eq!(stack.as_slice(), [Word::from(n)]);
        });

        run(|stack| {
            let bytes = [Word::from(n).to_be_bytes::<32>(); 2].concat();
            stack.push_slice(&bytes).unwrap();
            assert_eq!(stack.as_slice(), [Word::from(n); 2]);
        });

        run(|stack| {
            let bytes = [&[0; 32][..], &[42u8]].concat();
            stack.push_slice(&bytes).unwrap();
            assert_eq!(stack.as_slice(), [Word::ZERO, Word::from(42)]);
        });

        run(|stack| {
            let bytes = [&[0; 32][..], &n.to_be_bytes()].concat();
            stack.push_slice(&bytes).unwrap();
            assert_eq!(stack.as_slice(), [Word::ZERO, Word::from(n)]);
        });

        run(|stack| {
            let bytes = [&[0; 64][..], &n.to_be_bytes()].concat();
            stack.push_slice(&bytes).unwrap();
            assert_eq!(stack.as_slice(), [Word::ZERO, Word::ZERO, Word::from(n)]);
        });

        run_with_len(StackMut::CAPACITY, |stack| {
            core::assert_matches!(stack.push_slice(&[42]), Err(InstrStop::StackOverflow));
        });
    }
}
