/// Bytecode control state.
#[derive(Clone, Copy, Debug)]
pub struct Ctrl<'a> {
    pub(crate) base: *const u8,
    pub(crate) len: usize,
    pub(crate) pc: usize,
    _marker: core::marker::PhantomData<&'a [u8]>,
}

/// Mutable bytecode control reference.
#[derive(Debug)]
pub struct CtrlRef<'a> {
    base: *const u8,
    len: usize,
    pc: &'a mut usize,
    _marker: core::marker::PhantomData<&'a [u8]>,
}

impl<'a> Ctrl<'a> {
    pub(crate) fn new(bytecode: &'a [u8], pc: usize) -> Self {
        Self {
            base: bytecode.as_ptr(),
            len: bytecode.len(),
            pc,
            _marker: core::marker::PhantomData,
        }
    }

    /// Returns a mutable control reference.
    #[inline]
    pub fn as_mut(&mut self) -> CtrlRef<'_> {
        CtrlRef {
            base: self.base,
            len: self.len,
            pc: &mut self.pc,
            _marker: core::marker::PhantomData,
        }
    }

    /// # Safety
    ///
    /// Caller must ensure advancing by `n` keeps `pc` within valid bytecode bounds for
    /// subsequent reads.
    #[inline]
    pub unsafe fn advance_unchecked(&mut self, n: usize) {
        self.pc += n;
    }

    /// Returns the opcode at the current program counter.
    #[inline]
    pub fn op(&self) -> u8 {
        unsafe { *self.base.add(self.pc) }
    }

    /// Returns the current program counter.
    #[inline]
    pub fn pc(&self) -> usize {
        self.pc
    }

    /// Returns the bytecode length.
    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Returns whether the bytecode is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns the bytecode slice.
    #[inline]
    pub fn as_slice(&self) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.base, self.len) }
    }

    /// # Safety
    ///
    /// Caller must ensure `self.pc..self.pc + n` is in bounds of the bytecode allocation.
    #[inline]
    pub unsafe fn read_bytes_unchecked(&self, n: usize) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.base.add(self.pc), n) }
    }
}

impl<'a> CtrlRef<'a> {
    pub(crate) fn new(bytecode: &'a [u8], pc: &'a mut usize) -> Self {
        Self {
            base: bytecode.as_ptr(),
            len: bytecode.len(),
            pc,
            _marker: core::marker::PhantomData,
        }
    }

    /// Reborrows the control reference.
    #[inline]
    pub fn reborrow(&mut self) -> CtrlRef<'_> {
        unsafe { core::ptr::read(self) }
    }

    /// # Safety
    ///
    /// Caller must ensure advancing by `n` keeps the referenced program counter within
    /// valid bytecode bounds for subsequent reads.
    #[inline]
    pub unsafe fn advance_unchecked(&mut self, n: usize) {
        *self.pc += n;
    }

    /// Returns the opcode at the current program counter.
    #[inline]
    pub fn op(&self) -> u8 {
        unsafe { *self.base.add(*self.pc) }
    }

    /// Returns the current program counter.
    #[inline]
    pub fn pc(&self) -> usize {
        *self.pc
    }

    /// Returns the bytecode length.
    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Returns whether the bytecode is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns the bytecode slice.
    #[inline]
    pub fn as_slice(&self) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.base, self.len) }
    }

    /// # Safety
    ///
    /// Caller must ensure `pc` is a valid program counter for this bytecode.
    #[inline]
    pub unsafe fn set_unchecked(&mut self, pc: usize) {
        *self.pc = pc;
    }

    /// Returns whether `pc` points to a valid jump destination.
    #[inline]
    pub fn is_valid_jumpdest(&self, pc: usize) -> bool {
        pc < self.len && unsafe { *self.base.add(pc) } == 0x5B
    }

    /// # Safety
    ///
    /// Caller must ensure `offset..offset + len` is in bounds of the bytecode allocation.
    #[inline]
    pub unsafe fn code_slice_unchecked(&self, offset: usize, len: usize) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.base.add(offset), len) }
    }

    /// # Safety
    ///
    /// Caller must ensure `self.pc()..self.pc() + n` is in bounds of the bytecode allocation.
    #[inline]
    pub unsafe fn read_bytes_unchecked(&self, n: usize) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.base.add(*self.pc), n) }
    }
}
