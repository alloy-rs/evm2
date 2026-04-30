#[derive(Clone, Copy)]
pub struct Ctrl<'a> {
    pub(crate) base: *const u8,
    pub(crate) len: usize,
    pub(crate) pc: usize,
    _marker: core::marker::PhantomData<&'a [u8]>,
}

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

    #[inline]
    pub fn as_mut(&mut self) -> CtrlRef<'_> {
        CtrlRef {
            base: self.base,
            len: self.len,
            pc: &mut self.pc,
            _marker: core::marker::PhantomData,
        }
    }

    #[inline]
    pub unsafe fn advance_unchecked(&mut self, n: usize) {
        self.pc += n;
    }

    #[inline]
    pub fn op(&self) -> u8 {
        unsafe { *self.base.add(self.pc) }
    }

    #[inline]
    pub fn pc(&self) -> usize {
        self.pc
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

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

    #[inline]
    pub fn reborrow(&mut self) -> CtrlRef<'_> {
        unsafe { core::ptr::read(self) }
    }

    #[inline]
    pub unsafe fn advance_unchecked(&mut self, n: usize) {
        *self.pc += n;
    }

    #[inline]
    pub fn op(&self) -> u8 {
        unsafe { *self.base.add(*self.pc) }
    }

    #[inline]
    pub fn pc(&self) -> usize {
        *self.pc
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline]
    pub unsafe fn set_unchecked(&mut self, pc: usize) {
        *self.pc = pc;
    }

    #[inline]
    pub fn is_valid_jumpdest(&self, pc: usize) -> bool {
        pc < self.len && unsafe { *self.base.add(pc) } == 0x5B
    }

    #[inline]
    pub unsafe fn code_slice_unchecked(&self, offset: usize, len: usize) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.base.add(offset), len) }
    }

    #[inline]
    pub unsafe fn read_bytes_unchecked(&self, n: usize) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.base.add(*self.pc), n) }
    }
}
