#[derive(Clone, Copy)]
pub struct Ctrl<'a> {
    pub(crate) base: *const u8,
    pub(crate) ctrl: usize,
    _marker: core::marker::PhantomData<&'a [u8]>,
}

pub struct CtrlRef<'a> {
    base: *const u8,
    ctrl: &'a mut usize,
    _marker: core::marker::PhantomData<&'a [u8]>,
}

impl<'a> Ctrl<'a> {
    #[inline]
    pub fn as_mut(&mut self) -> CtrlRef<'_> {
        CtrlRef { base: self.base, ctrl: &mut self.ctrl, _marker: core::marker::PhantomData }
    }

    #[inline]
    pub unsafe fn advance_unchecked(&mut self, n: usize) {
        self.ctrl += n;
    }

    #[inline]
    pub fn op(&self) -> u8 {
        unsafe { *self.base.add(self.ctrl) }
    }

    #[inline]
    pub fn ctrl(&self) -> usize {
        self.ctrl
    }

    #[inline]
    pub unsafe fn read_bytes_unchecked(&self, n: usize) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.base.add(self.ctrl), n) }
    }
}

impl<'a> CtrlRef<'a> {
    pub(crate) fn new(bytecode: &'a [u8], ctrl: &'a mut usize) -> Self {
        Self { base: bytecode.as_ptr(), ctrl, _marker: core::marker::PhantomData }
    }

    #[inline]
    pub fn reborrow(&mut self) -> CtrlRef<'_> {
        unsafe { core::ptr::read(self) }
    }

    #[inline]
    pub unsafe fn advance_unchecked(&mut self, n: usize) {
        *self.ctrl += n;
    }

    #[inline]
    pub fn op(&self) -> u8 {
        unsafe { *self.base.add(*self.ctrl) }
    }

    #[inline]
    pub fn ctrl(&self) -> usize {
        *self.ctrl
    }

    #[inline]
    pub unsafe fn read_bytes_unchecked(&self, n: usize) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.base.add(*self.ctrl), n) }
    }
}
