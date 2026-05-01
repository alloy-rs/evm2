/// EVM bytecode view.
#[derive(Clone, Copy, Debug)]
pub struct Bytecode<'a> {
    bytecode: &'a [u8],
}

/// Program counter state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pc {
    pc: usize,
}

impl<'a> Bytecode<'a> {
    pub(crate) fn new(bytecode: &'a [u8]) -> Self {
        Self { bytecode }
    }

    /// Returns the opcode at the current program counter.
    #[inline]
    pub fn op(&self, pc: &Pc) -> u8 {
        self.bytecode[pc.get()]
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
        self.bytecode.get(pc) == Some(&0x5B)
    }

    /// # Safety
    ///
    /// Caller must ensure `offset..offset + len` is in bounds of the bytecode allocation.
    #[inline]
    pub unsafe fn code_slice_unchecked(&self, offset: usize, len: usize) -> &'a [u8] {
        unsafe { self.bytecode.get_unchecked(offset..offset + len) }
    }

    /// # Safety
    ///
    /// Caller must ensure `pc.get()..pc.get() + n` is in bounds of the bytecode allocation.
    #[inline]
    pub unsafe fn read_bytes_unchecked(&self, pc: &Pc, n: usize) -> &'a [u8] {
        unsafe { self.bytecode.get_unchecked(pc.get()..pc.get() + n) }
    }
}

impl Pc {
    pub(crate) const fn new(pc: usize) -> Self {
        Self { pc }
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
}
