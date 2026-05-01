use crate::bytecode::{Bytecode, JumpTableRef};
use core::{marker::PhantomData, ptr};

/// EVM bytecode view.
#[derive(Clone, Copy, Debug)]
pub struct BytecodeRef<'a> {
    bytecode: &'a [u8],
    jump_table: JumpTableRef<'a>,
}

/// Program counter state.
#[derive(Clone, Copy, Debug)]
pub struct Pc<'a> {
    base: *const u8,
    pc: usize,
    _marker: PhantomData<&'a [u8]>,
}

/// Mutable program counter state.
#[derive(Debug)]
pub struct PcMut<'a> {
    base: *const u8,
    pc: &'a mut usize,
}

impl<'a> BytecodeRef<'a> {
    pub(crate) fn new(bytecode: &'a Bytecode) -> Self {
        Self {
            bytecode: bytecode.original_byte_slice(),
            jump_table: bytecode.jump_table().as_ref(),
        }
    }

    /// Returns the bytecode length.
    #[inline]
    pub const fn len(&self) -> usize {
        self.bytecode.len()
    }

    /// Returns whether the bytecode is empty.
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.bytecode.is_empty()
    }

    /// Returns the bytecode slice.
    #[inline]
    pub const fn as_slice(&self) -> &'a [u8] {
        self.bytecode
    }

    /// Returns whether `pc` points to a valid jump destination.
    #[inline]
    pub const fn is_valid_jumpdest(&self, pc: usize) -> bool {
        self.jump_table.is_valid(pc)
    }

    /// # Safety
    ///
    /// Caller must ensure `offset..offset + len` is in bounds of the bytecode allocation.
    #[inline]
    pub unsafe fn code_slice_unchecked(&self, offset: usize, len: usize) -> &'a [u8] {
        unsafe { self.bytecode.get_unchecked(offset..offset + len) }
    }
}

impl<'a> Pc<'a> {
    /// Creates a program counter from bytecode and an offset.
    #[cfg_attr(not(feature = "nightly"), allow(dead_code))]
    pub(crate) const fn new(bytecode: BytecodeRef<'a>, pc: usize) -> Self {
        Self { base: bytecode.bytecode.as_ptr(), pc, _marker: PhantomData }
    }

    /// Returns a mutable program counter reference.
    #[inline]
    pub const fn as_mut(&mut self) -> PcMut<'_> {
        PcMut { base: self.base, pc: &mut self.pc }
    }

    /// Returns the opcode at the current program counter.
    #[inline]
    pub const fn op(&self) -> u8 {
        unsafe { *self.base.add(self.pc) }
    }

    /// Returns the current program counter.
    #[inline]
    pub const fn get(&self) -> usize {
        self.pc
    }

    /// # Safety
    ///
    /// Caller must ensure advancing by `n` keeps `pc` within valid bytecode bounds for
    /// subsequent reads.
    #[inline]
    pub const unsafe fn advance_unchecked(&mut self, n: usize) {
        self.pc += n;
    }

    /// # Safety
    ///
    /// Caller must ensure `pc` is valid for the current bytecode.
    #[inline]
    pub const unsafe fn set_unchecked(&mut self, pc: usize) {
        self.pc = pc;
    }

    /// # Safety
    ///
    /// Caller must ensure `self.get()..self.get() + n` is in bounds of the bytecode allocation.
    #[inline]
    pub const unsafe fn read_bytes_unchecked(&self, n: usize) -> &'a [u8] {
        unsafe { core::slice::from_raw_parts(self.base.add(self.pc), n) }
    }
}

impl<'a> PcMut<'a> {
    pub(crate) const fn new(bytecode: BytecodeRef<'a>, pc: &'a mut usize) -> Self {
        Self { base: bytecode.bytecode.as_ptr(), pc }
    }

    /// Reborrows the program counter.
    #[inline]
    pub const fn reborrow(&mut self) -> PcMut<'_> {
        unsafe { ptr::read(self) }
    }

    /// Returns the opcode at the current program counter.
    #[inline]
    pub const fn op(&self) -> u8 {
        unsafe { *self.base.add(self.get()) }
    }

    /// Returns the current program counter.
    #[inline]
    pub const fn get(&self) -> usize {
        *self.pc
    }

    /// # Safety
    ///
    /// Caller must ensure advancing by `n` keeps `pc` within valid bytecode bounds for
    /// subsequent reads.
    #[inline]
    pub const unsafe fn advance_unchecked(&mut self, n: usize) {
        *self.pc += n;
    }

    /// # Safety
    ///
    /// Caller must ensure `pc` is valid for the current bytecode.
    #[inline]
    pub const unsafe fn set_unchecked(&mut self, pc: usize) {
        *self.pc = pc;
    }

    /// # Safety
    ///
    /// Caller must ensure `self.get()..self.get() + n` is in bounds of the bytecode allocation.
    #[inline]
    pub const unsafe fn read_bytes_unchecked(&self, n: usize) -> &'a [u8] {
        unsafe { core::slice::from_raw_parts(self.base.add(self.get()), n) }
    }
}
