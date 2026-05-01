use crate::bytecode::{Bytecode, JumpTable};
use core::marker::PhantomData;

/// EVM bytecode view.
#[derive(Clone, Copy, Debug)]
pub struct BytecodeRef<'a> {
    bytecode: &'a [u8],
    jump_table: &'a JumpTable,
}

/// Program counter state.
#[derive(Clone, Copy, Debug)]
pub struct Pc<'a> {
    base: *const u8,
    pc: usize,
    _marker: PhantomData<&'a [u8]>,
}

/// Mutable program counter state.
#[derive(Clone, Copy, Debug)]
pub struct PcMut<'a> {
    base: *const u8,
    pc: *mut usize,
    _marker: PhantomData<&'a mut usize>,
}

impl<'a> BytecodeRef<'a> {
    pub(crate) fn new(bytecode: &'a Bytecode) -> Self {
        Self { bytecode: bytecode.bytes_slice(), jump_table: bytecode.jump_table() }
    }

    /// Returns the bytecode length.
    #[inline]
    pub fn len(&self) -> usize {
        self.bytecode.len()
    }

    /// Returns whether the bytecode is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.bytecode.is_empty()
    }

    /// Returns the bytecode slice.
    #[inline]
    pub fn as_slice(&self) -> &'a [u8] {
        self.bytecode
    }

    /// Returns whether `pc` points to a valid jump destination.
    #[inline]
    pub fn is_valid_jumpdest(&self, pc: usize) -> bool {
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
    pub(crate) fn new(bytecode: BytecodeRef<'a>, pc: usize) -> Self {
        Self { base: bytecode.bytecode.as_ptr(), pc, _marker: PhantomData }
    }

    /// Returns a mutable program counter reference.
    #[inline]
    pub fn as_mut(&mut self) -> PcMut<'_> {
        PcMut { base: self.base, pc: &mut self.pc, _marker: PhantomData }
    }

    /// Returns the opcode at the current program counter.
    #[inline]
    pub fn op(&self) -> u8 {
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
    pub unsafe fn advance_unchecked(&mut self, n: usize) {
        self.pc += n;
    }

    /// # Safety
    ///
    /// Caller must ensure `pc` is valid for the current bytecode.
    #[inline]
    pub unsafe fn set_unchecked(&mut self, pc: usize) {
        self.pc = pc;
    }

    /// # Safety
    ///
    /// Caller must ensure `self.get()..self.get() + n` is in bounds of the bytecode allocation.
    #[inline]
    pub unsafe fn read_bytes_unchecked(&self, n: usize) -> &'a [u8] {
        unsafe { core::slice::from_raw_parts(self.base.add(self.pc), n) }
    }
}

impl<'a> PcMut<'a> {
    pub(crate) fn new(bytecode: BytecodeRef<'a>, pc: &'a mut usize) -> Self {
        Self { base: bytecode.bytecode.as_ptr(), pc, _marker: PhantomData }
    }

    /// Returns the opcode at the current program counter.
    #[inline]
    pub fn op(&self) -> u8 {
        unsafe { *self.base.add(self.get()) }
    }

    /// Returns the current program counter.
    #[inline]
    pub fn get(&self) -> usize {
        unsafe { *self.pc }
    }

    /// # Safety
    ///
    /// Caller must ensure advancing by `n` keeps `pc` within valid bytecode bounds for
    /// subsequent reads.
    #[inline]
    pub unsafe fn advance_unchecked(self, n: usize) {
        unsafe { *self.pc += n };
    }

    /// # Safety
    ///
    /// Caller must ensure `pc` is valid for the current bytecode.
    #[inline]
    pub unsafe fn set_unchecked(self, pc: usize) {
        unsafe { *self.pc = pc };
    }

    /// # Safety
    ///
    /// Caller must ensure `self.get()..self.get() + n` is in bounds of the bytecode allocation.
    #[inline]
    pub unsafe fn read_bytes_unchecked(self, n: usize) -> &'a [u8] {
        unsafe { core::slice::from_raw_parts(self.base.add(self.get()), n) }
    }
}
