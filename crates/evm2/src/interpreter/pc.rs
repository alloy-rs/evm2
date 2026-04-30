#[derive(Clone, Copy)]
pub struct Pc<'a> {
    pub(crate) base: *const u8,
    pub(crate) pc: usize,
    _marker: core::marker::PhantomData<&'a [u8]>,
}

pub struct PcRef<'a> {
    base: *const u8,
    pc: &'a mut usize,
    _marker: core::marker::PhantomData<&'a [u8]>,
}

impl<'a> Pc<'a> {
    #[inline]
    pub fn as_mut(&mut self) -> PcRef<'_> {
        PcRef { base: self.base, pc: &mut self.pc, _marker: core::marker::PhantomData }
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
    pub unsafe fn read_bytes_unchecked(&self, n: usize) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.base.add(self.pc), n) }
    }
}

impl<'a> PcRef<'a> {
    pub(crate) fn new(bytecode: &'a [u8], pc: &'a mut usize) -> Self {
        Self { base: bytecode.as_ptr(), pc, _marker: core::marker::PhantomData }
    }

    #[inline]
    pub fn reborrow(&mut self) -> PcRef<'_> {
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
    pub unsafe fn read_bytes_unchecked(&self, n: usize) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.base.add(*self.pc), n) }
    }
}
