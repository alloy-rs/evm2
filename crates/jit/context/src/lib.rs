#![doc = include_str!("../README.md")]
#![cfg_attr(not(test), warn(unused_extern_crates))]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

use alloc::vec::Vec;
use alloy_primitives::{Address, B256, Bytes, U256, ruint};
use core::{fmt, mem::MaybeUninit, ptr::NonNull};
pub use evm2::interpreter::{Gas, InstrStop, Memory};
use evm2::{
    BaseEvmTypes, SpecId,
    env::{BlockEnv, TxEnv},
    interpreter::{Host as Evm2Host, Interpreter, Message},
    version::GasParams,
};

mod arch;
use arch::evm2_jit_entry;
pub use arch::evm2_jit_exit;

pub mod evm2_api;

#[doc(hidden)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallInput {
    Bytes(Bytes),
}

impl CallInput {
    #[inline]
    pub fn len(&self) -> usize {
        match self {
            Self::Bytes(bytes) => bytes.len(),
        }
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Bytes(bytes) => bytes.as_ref(),
        }
    }
}

impl Default for CallInput {
    #[inline]
    fn default() -> Self {
        Self::Bytes(Bytes::new())
    }
}

#[doc(hidden)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct Inputs {
    pub target_address: Address,
    pub bytecode_address: Option<Address>,
    pub caller_address: Address,
    pub input: CallInput,
    pub call_value: U256,
}

impl Inputs {
    #[inline]
    pub const fn input(&self) -> &CallInput {
        &self.input
    }
}

#[doc(hidden)]
pub mod jit_abi {
    pub type Inputs = super::Inputs;

    use super::*;

    #[derive(Clone, Copy, Debug)]
    #[repr(C)]
    pub struct Gas {
        pub tracker: GasTracker,
        pub memory: MemoryGas,
    }

    #[derive(Clone, Copy, Debug)]
    #[repr(C)]
    pub struct GasTracker {
        pub remaining: u64,
        pub limit: u64,
        pub reservoir: u64,
        pub state_gas_spent: u64,
        pub refunded: i64,
    }

    #[derive(Clone, Copy, Debug)]
    #[repr(C)]
    pub struct MemoryGas {
        pub words_num: usize,
        pub expansion_cost: u64,
    }

    const _: () = {
        use core::mem::{align_of, offset_of, size_of};

        assert!(offset_of!(Inputs, target_address) == 0);
        assert!(align_of::<Inputs>() >= align_of::<Address>());

        assert!(size_of::<Gas>() == size_of::<super::Gas>());
        assert!(align_of::<Gas>() == align_of::<super::Gas>());
    };
}

/// The EVM bytecode compiler runtime context.
///
/// This is a simple wrapper around the interpreter's resources, allowing the compiled function to
/// access the memory, input, gas, host, and other resources.
///
/// # Safety
/// This struct uses `#[repr(C)]` to ensure a stable field layout since the JIT compiler
/// generates code that accesses fields by offset using `offset_of!`.
#[repr(C)]
pub struct EvmContext<'a> {
    /// Active interpreter frame.
    interpreter: NonNull<Interpreter<'a, BaseEvmTypes>>,
    /// Input information (target address, caller, input data, call value).
    pub input: &'a mut Inputs,
    /// The gas.
    pub gas: Gas,
    /// The size of return data from the last call-like operation.
    pub return_data_len: usize,
    /// The size of the call input data, cached for CALLDATASIZE.
    pub calldatasize: usize,
    /// The result set by a builtin before exiting via [`evm2_jit_exit`].
    pub exit_result: InstrStop,
    /// Saved RSP from the entry trampoline, used by [`evm2_jit_exit`] to unwind.
    pub exit_sp: *mut u8,
    /// Cached base pointer for the current memory context.
    /// Refreshed after any memory resize.
    pub mem_base: *mut u8,
    /// Cached length of the current memory context in bytes.
    /// Refreshed after any memory resize.
    pub mem_len: usize,
    /// Output produced by RETURN or REVERT.
    #[doc(hidden)]
    output: Bytes,
}

// Static assertions to ensure the struct layout matches expectations.
// These offsets are used by the JIT compiler to access fields.
const _: () = {
    use core::mem::offset_of;

    assert!(offset_of!(EvmContext<'_>, interpreter) == 0);
    assert!(offset_of!(EvmContext<'_>, input) > 0);
    assert!(offset_of!(EvmContext<'_>, gas) > 0);
    assert!(offset_of!(EvmContext<'_>, return_data_len) > 0);
    assert!(offset_of!(EvmContext<'_>, calldatasize) > 0);
    assert!(offset_of!(EvmContext<'_>, exit_result) > 0);
    assert!(offset_of!(EvmContext<'_>, exit_sp) > 0);
    assert!(offset_of!(EvmContext<'_>, mem_base) > 0);
    assert!(offset_of!(EvmContext<'_>, mem_len) > 0);
};

impl fmt::Debug for EvmContext<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EvmContext").field("memory", &self.memory()).finish_non_exhaustive()
    }
}

impl<'a> EvmContext<'a> {
    #[inline]
    fn interpreter(&self) -> &Interpreter<'a, BaseEvmTypes> {
        unsafe { self.interpreter.as_ref() }
    }

    #[inline]
    fn interpreter_mut(&mut self) -> &mut Interpreter<'a, BaseEvmTypes> {
        unsafe { self.interpreter.as_mut() }
    }

    /// Returns the current linear memory.
    #[inline]
    pub fn memory(&self) -> &Memory {
        self.interpreter().memory_ref()
    }

    /// Returns the current linear memory.
    #[inline]
    pub fn memory_mut(&mut self) -> &mut Memory {
        self.interpreter_mut().memory_mut()
    }

    /// Resizes memory using EVM memory gas accounting.
    #[inline]
    pub fn resize_memory(&mut self, offset: usize, len: usize) -> Result<(), InstrStop> {
        let memory = self.memory_mut() as *mut Memory;
        let gas = &mut self.gas as *mut Gas;
        unsafe { (*memory).resize_evm(&mut *gas, offset, len)? };
        self.refresh_memory_cache();
        Ok(())
    }

    /// Returns host state consumed by host-touching builtins.
    #[inline]
    pub fn host(&mut self) -> &mut (impl Evm2Host<BaseEvmTypes> + '_) {
        self.interpreter_mut().host()
    }

    /// Returns the input shim visible to compiled code.
    #[inline]
    pub fn input(&self) -> &Inputs {
        self.input
    }

    /// Returns the input shim visible to compiled code.
    #[inline]
    pub fn input_mut(&mut self) -> &mut Inputs {
        self.input
    }

    /// Returns the current block environment.
    #[inline]
    pub fn block_env(&mut self) -> &BlockEnv<BaseEvmTypes> {
        self.host().block_env()
    }

    /// Returns the transaction-global environment.
    #[inline]
    pub fn tx_env(&self) -> &'a TxEnv<BaseEvmTypes> {
        self.interpreter().tx_env()
    }

    /// Returns active runtime version data.
    #[inline]
    pub fn version(&self) -> &evm2::Version {
        self.interpreter().version()
    }

    /// Returns active runtime gas parameters.
    #[inline]
    pub fn gas_params(&self) -> &GasParams {
        &self.version().gas_params
    }

    /// Returns the active base specification ID.
    #[inline]
    pub fn spec_id(&self) -> SpecId {
        self.interpreter().spec()
    }

    /// Returns the active frame-local call/create message.
    #[inline]
    pub fn message(&self) -> &'a Message<BaseEvmTypes> {
        self.interpreter().message()
    }

    /// Returns whether the active frame forbids state-changing operations.
    #[inline]
    pub fn is_static(&self) -> bool {
        self.interpreter().is_static()
    }

    /// Sets the static-call flag for JIT test setup.
    #[inline]
    #[doc(hidden)]
    pub fn set_static_for_jit(&mut self, is_static: bool) {
        self.interpreter_mut().set_static_for_jit(is_static);
    }

    /// Returns active original bytecode.
    #[inline]
    pub fn bytecode(&self) -> Bytes {
        self.interpreter().original_bytecode()
    }

    /// Returns return data from the last call-like operation.
    #[inline]
    pub fn return_data(&self) -> &[u8] {
        self.interpreter().return_data().as_ref()
    }

    /// Sets return data from the last call-like operation.
    #[inline]
    pub fn set_return_data(&mut self, data: Bytes) {
        self.return_data_len = data.len();
        self.interpreter_mut().set_return_data(data);
    }

    /// Returns output produced by RETURN or REVERT.
    #[inline]
    pub fn output(&self) -> &Bytes {
        &self.output
    }

    /// Sets output produced by RETURN or REVERT.
    #[inline]
    pub fn set_output(&mut self, output: Bytes) {
        self.output = output;
    }

    /// Refreshes the cached memory base pointer and length.
    ///
    /// Must be called after any operation that may resize memory.
    #[inline]
    pub fn refresh_memory_cache(&mut self) {
        let (mem_base, mem_len) = {
            let slice = self.memory_mut().as_mut_slice();
            (slice.as_mut_ptr(), slice.len())
        };
        self.mem_base = mem_base;
        self.mem_len = mem_len;
    }
}

/// Declare [`RawEvmCompilerFn`] functions in an `extern "C"` block.
///
/// # Examples
///
/// ```no_run
/// use evm2_jit_context::{EvmCompilerFn, extern_evm2_jit};
///
/// extern_evm2_jit! {
///    /// A simple function.
///    pub fn test_fn;
/// }
///
/// let test_fn = EvmCompilerFn::new(test_fn);
/// ```
#[macro_export]
macro_rules! extern_evm2_jit {
    ($( $(#[$attr:meta])* $vis:vis fn $name:ident; )+) => {
        #[allow(improper_ctypes)]
        unsafe extern "C" {
            $(
                $(#[$attr])*
                $vis fn $name(
                    ecx: ::core::ptr::NonNull<$crate::EvmContext<'_>>,
                    stack: ::core::ptr::NonNull<$crate::EvmStack>,
                    stack_len: ::core::ptr::NonNull<usize>,
                ) -> $crate::InstrStop;
            )+
        }
    };
}

/// The raw function signature of a bytecode function.
///
/// Prefer using [`EvmCompilerFn`] instead of this type. See [`EvmCompilerFn::call`] for more
/// information.
// When changing the signature, also update the corresponding declarations in `fn translate`.
pub type RawEvmCompilerFn = unsafe extern "C" fn(
    ecx: NonNull<EvmContext<'_>>,
    stack: NonNull<EvmStack>,
    stack_len: NonNull<usize>,
) -> InstrStop;

/// An EVM bytecode function.
#[derive(Clone, Copy, Debug, Hash)]
pub struct EvmCompilerFn(RawEvmCompilerFn);

impl From<RawEvmCompilerFn> for EvmCompilerFn {
    #[inline]
    fn from(f: RawEvmCompilerFn) -> Self {
        Self::new(f)
    }
}

impl From<EvmCompilerFn> for RawEvmCompilerFn {
    #[inline]
    fn from(f: EvmCompilerFn) -> Self {
        f.into_inner()
    }
}

impl EvmCompilerFn {
    /// Wraps the function.
    #[inline]
    pub const fn new(f: RawEvmCompilerFn) -> Self {
        Self(f)
    }

    /// Unwraps the function.
    #[inline]
    pub const fn into_inner(self) -> RawEvmCompilerFn {
        self.0
    }

    /// Calls the function.
    ///
    /// Arguments:
    /// - `stack`: The stack buffer.
    /// - `stack_len`: The stack length.
    /// - `ecx`: The context object.
    ///
    /// Use of this method is discouraged, as setup and cleanup need to be done manually.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the arguments are valid and that the function is safe to call.
    #[inline]
    pub unsafe fn call(
        self,
        stack: &mut EvmStack,
        stack_len: &mut usize,
        ecx: &mut EvmContext<'_>,
    ) -> InstrStop {
        unsafe {
            evm2_jit_entry(
                NonNull::from(ecx),
                NonNull::from(stack),
                NonNull::from(stack_len),
                self.0,
            )
        }
    }

    /// Same as [`call`](Self::call) but with `#[inline(never)]`.
    ///
    /// Use of this method is discouraged, as setup and cleanup need to be done manually.
    ///
    /// # Safety
    ///
    /// See [`call`](Self::call).
    #[inline(never)]
    pub unsafe fn call_noinline(
        self,
        stack: &mut EvmStack,
        stack_len: &mut usize,
        ecx: &mut EvmContext<'_>,
    ) -> InstrStop {
        unsafe { self.call(stack, stack_len, ecx) }
    }
}

/// EVM context stack.
#[repr(C)]
#[allow(missing_debug_implementations)]
pub struct EvmStack([MaybeUninit<EvmWord>; 1024]);

#[allow(clippy::new_without_default)]
impl EvmStack {
    /// The size of the stack in bytes.
    pub const SIZE: usize = 32 * Self::CAPACITY;

    /// The size of the stack in U256 elements.
    pub const CAPACITY: usize = 1024;

    /// Creates a new EVM stack, allocated on the stack.
    ///
    /// Use [`EvmStack::new_heap`] to create a stack on the heap.
    #[inline]
    pub fn new() -> Self {
        Self(unsafe { MaybeUninit::uninit().assume_init() })
    }

    /// Creates a vector that can be used as a stack.
    #[inline]
    pub fn new_heap() -> Vec<EvmWord> {
        Vec::with_capacity(1024)
    }

    /// Creates a stack from a vector's buffer.
    ///
    /// # Panics
    ///
    /// Panics if the vector's capacity is less than the required stack capacity.
    #[inline]
    pub fn from_vec(vec: &Vec<EvmWord>) -> &Self {
        assert!(vec.capacity() >= Self::CAPACITY);
        unsafe { Self::from_ptr(vec.as_ptr()) }
    }

    /// Creates a stack from a mutable vector's buffer.
    ///
    /// # Panics
    ///
    /// Panics if the vector's capacity is less than the required stack capacity.
    #[inline]
    pub fn from_mut_vec(vec: &mut Vec<EvmWord>) -> &mut Self {
        assert!(vec.capacity() >= Self::CAPACITY);
        unsafe { Self::from_mut_ptr(vec.as_mut_ptr()) }
    }

    /// Creates a stack from a pointer to a buffer.
    ///
    /// # Safety
    ///
    /// See [`from_vec`](Self::from_vec).
    #[inline]
    pub unsafe fn from_ptr<'a>(ptr: *const EvmWord) -> &'a Self {
        debug_assert!(ptr.is_aligned());
        unsafe { &*ptr.cast::<Self>() }
    }

    /// Creates a stack from a mutable pointer to a buffer.
    ///
    /// # Safety
    ///
    /// See [`from_mut_vec`](Self::from_mut_vec).
    #[inline]
    pub unsafe fn from_mut_ptr<'a>(ptr: *mut EvmWord) -> &'a mut Self {
        debug_assert!(ptr.is_aligned());
        unsafe { &mut *ptr.cast::<Self>() }
    }

    /// Returns a pointer to the stack.
    #[inline]
    pub const fn as_ptr(&self) -> *const EvmWord {
        self.0.as_ptr().cast()
    }

    /// Returns a mutable pointer to the stack.
    #[inline]
    pub fn as_mut_ptr(&mut self) -> *mut EvmWord {
        self.0.as_mut_ptr().cast()
    }

    /// Returns a slice of the initialized portion of the stack.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the first `len` slots are initialized.
    #[inline]
    pub unsafe fn as_slice(&self, len: usize) -> &[EvmWord] {
        assert!(len <= Self::CAPACITY);
        unsafe { core::slice::from_raw_parts(self.as_ptr(), len) }
    }

    /// Returns a mutable slice of the initialized portion of the stack.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the first `len` slots are initialized.
    #[inline]
    pub unsafe fn as_mut_slice(&mut self, len: usize) -> &mut [EvmWord] {
        assert!(len <= Self::CAPACITY);
        unsafe { core::slice::from_raw_parts_mut(self.as_mut_ptr(), len) }
    }

    /// Sets the value at the given index.
    ///
    /// # Panics
    ///
    /// Panics if the index is out of bounds.
    #[inline]
    pub fn set(&mut self, index: usize, value: EvmWord) {
        self.0[index] = MaybeUninit::new(value);
    }

    /// Returns the word at the given index as a reference.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the slot at `index` is initialized.
    #[inline]
    pub unsafe fn get(&self, index: usize) -> Option<&EvmWord> {
        self.0.get(index).map(|slot| unsafe { slot.assume_init_ref() })
    }

    /// Returns the word at the given index as a mutable reference.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the slot at `index` is initialized.
    #[inline]
    pub unsafe fn get_mut(&mut self, index: usize) -> Option<&mut EvmWord> {
        self.0.get_mut(index).map(|slot| unsafe { slot.assume_init_mut() })
    }

    /// Returns the word at the given index as a reference.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the index is within bounds.
    #[inline]
    pub unsafe fn get_unchecked(&self, index: usize) -> &EvmWord {
        unsafe { self.0.get_unchecked(index).assume_init_ref() }
    }

    /// Returns the word at the given index as a mutable reference.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the index is within bounds.
    #[inline]
    pub unsafe fn get_unchecked_mut(&mut self, index: usize) -> &mut EvmWord {
        unsafe { self.0.get_unchecked_mut(index).assume_init_mut() }
    }

    /// Sets the value at the top of the stack to `value`, and grows the stack by 1.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the stack is not full.
    #[inline]
    pub unsafe fn push(&mut self, value: EvmWord, len: &mut usize) {
        unsafe { self.set_unchecked(*len, value) };
        *len += 1;
    }

    /// Returns the value at the top of the stack.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the stack is not empty.
    #[inline]
    pub unsafe fn top_unchecked(&self, len: usize) -> &EvmWord {
        unsafe { self.get_unchecked(len - 1) }
    }

    /// Returns the value at the top of the stack as a mutable reference.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the stack is not empty.
    #[inline]
    pub unsafe fn top_unchecked_mut(&mut self, len: usize) -> &mut EvmWord {
        unsafe { self.get_unchecked_mut(len - 1) }
    }

    /// Returns the value at the given index from the top of the stack.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `len >= n + 1`.
    #[inline]
    pub unsafe fn from_top_unchecked(&self, len: usize, n: usize) -> &EvmWord {
        unsafe { self.get_unchecked(len - n - 1) }
    }

    /// Returns the value at the given index from the top of the stack as a mutable reference.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `len >= n + 1`.
    #[inline]
    pub unsafe fn from_top_unchecked_mut(&mut self, len: usize, n: usize) -> &mut EvmWord {
        unsafe { self.get_unchecked_mut(len - n - 1) }
    }

    /// Sets the value at the given index.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the index is within bounds.
    #[inline]
    pub unsafe fn set_unchecked(&mut self, index: usize, value: EvmWord) {
        unsafe { *self.0.get_unchecked_mut(index) = MaybeUninit::new(value) };
    }
}

/// An EVM stack word, which is stored in native-endian order.
#[repr(C, align(8))]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct EvmWord(B256);

impl Default for EvmWord {
    #[inline]
    fn default() -> Self {
        Self::ZERO
    }
}

impl fmt::Debug for EvmWord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.to_u256().fmt(f)
    }
}

impl fmt::Display for EvmWord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.to_u256().fmt(f)
    }
}

impl TryFrom<EvmWord> for usize {
    type Error = ruint::FromUintError<Self>;

    #[inline]
    fn try_from(w: EvmWord) -> Result<Self, Self::Error> {
        Self::try_from(&w)
    }
}

impl TryFrom<&EvmWord> for usize {
    type Error = ruint::FromUintError<Self>;

    #[inline]
    fn try_from(w: &EvmWord) -> Result<Self, Self::Error> {
        w.to_u256().try_into()
    }
}

impl TryFrom<&mut EvmWord> for usize {
    type Error = ruint::FromUintError<Self>;

    #[inline]
    fn try_from(w: &mut EvmWord) -> Result<Self, Self::Error> {
        Self::try_from(&*w)
    }
}

impl From<U256> for EvmWord {
    #[inline]
    fn from(u: U256) -> Self {
        Self::from_u256(u)
    }
}

impl EvmWord {
    /// Zero.
    pub const ZERO: Self = Self(B256::ZERO);

    /// Create a new word from big-endian bytes.
    #[inline]
    pub const fn from_be_bytes(bytes: B256) -> Self {
        Self::from_be(Self(bytes))
    }

    /// Create a new word from big-endian bytes.
    #[inline]
    pub const fn from_be_slice(bytes: &[u8]) -> Self {
        Self::from_u256(U256::from_be_slice(bytes))
    }

    /// Create a new word from little-endian bytes.
    #[inline]
    pub const fn from_le_bytes(bytes: B256) -> Self {
        Self::from_le(Self(bytes))
    }

    /// Create a new word from little-endian slice.
    #[inline]
    pub const fn from_le_slice(bytes: &[u8]) -> Self {
        Self::from_u256(U256::from_le_slice(bytes))
    }

    /// Create a new word from native-endian bytes.
    #[inline]
    pub const fn from_ne_bytes(bytes: B256) -> Self {
        Self(bytes)
    }

    /// Create a new word from a [`U256`]. This is a no-op on little-endian systems.
    #[inline]
    pub const fn from_u256(u: U256) -> Self {
        #[cfg(target_endian = "little")]
        return unsafe { core::mem::transmute::<U256, Self>(u) };
        #[cfg(target_endian = "big")]
        return Self(B256::new(u.to_be_bytes()));
    }

    /// Converts a big-endian representation into a native one.
    #[inline]
    pub const fn from_be(x: Self) -> Self {
        #[cfg(target_endian = "little")]
        return x.swap_bytes();
        #[cfg(target_endian = "big")]
        return x;
    }

    /// Converts a little-endian representation into a native one.
    #[inline]
    pub const fn from_le(x: Self) -> Self {
        #[cfg(target_endian = "little")]
        return x;
        #[cfg(target_endian = "big")]
        return x.swap_bytes();
    }

    /// Return the memory representation of this integer as a byte array in big-endian byte order.
    #[inline]
    pub const fn to_be_bytes(self) -> B256 {
        self.to_be().to_ne_bytes()
    }

    /// Return the memory representation of this integer as a byte array in little-endian byte
    /// order.
    #[inline]
    pub const fn to_le_bytes(self) -> B256 {
        self.to_le().to_ne_bytes()
    }

    /// Return the memory representation of this integer as a byte array in native byte order.
    #[inline]
    pub const fn to_ne_bytes(self) -> B256 {
        self.0
    }

    /// Converts `self` to big endian from the target's endianness.
    #[inline]
    pub const fn to_be(self) -> Self {
        #[cfg(target_endian = "little")]
        return self.swap_bytes();
        #[cfg(target_endian = "big")]
        return self;
    }

    /// Converts `self` to little endian from the target's endianness.
    #[inline]
    pub const fn to_le(self) -> Self {
        #[cfg(target_endian = "little")]
        return self;
        #[cfg(target_endian = "big")]
        return self.swap_bytes();
    }

    /// Reverses the byte order of the integer.
    #[inline]
    pub const fn swap_bytes(mut self) -> Self {
        self.0.0.reverse();
        self
    }

    /// Casts this value to a [`U256`]. This is a no-op on little-endian systems.
    #[cfg(target_endian = "little")]
    #[inline]
    pub const fn as_u256(&self) -> &U256 {
        unsafe { &*(self as *const Self as *const U256) }
    }

    /// Casts this value to a [`U256`]. This is a no-op on little-endian systems.
    #[cfg(target_endian = "little")]
    #[inline]
    pub const fn as_u256_mut(&mut self) -> &mut U256 {
        unsafe { &mut *(self as *mut Self as *mut U256) }
    }

    /// Converts this value to a [`U256`]. This is a simple copy on little-endian systems.
    #[inline]
    pub const fn to_u256(&self) -> U256 {
        #[cfg(target_endian = "little")]
        return *self.as_u256();
        #[cfg(target_endian = "big")]
        return U256::from_be_bytes(self.0.0);
    }

    /// Converts this value to a [`U256`]. This is a no-op on little-endian systems.
    #[inline]
    pub const fn into_u256(self) -> U256 {
        #[cfg(target_endian = "little")]
        return unsafe { core::mem::transmute::<Self, U256>(self) };
        #[cfg(target_endian = "big")]
        return U256::from_be_bytes(self.0.0);
    }

    /// Converts this value to an [`Address`].
    #[inline]
    pub fn to_address(self) -> Address {
        Address::from_word(self.to_be_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversions() {
        let mut word = EvmWord::ZERO;
        assert_eq!(usize::try_from(word), Ok(0));
        assert_eq!(usize::try_from(&word), Ok(0));
        assert_eq!(usize::try_from(&mut word), Ok(0));
    }

    extern_evm2_jit! {
        #[link_name = "__test_fn"]
        fn test_fn;
    }

    #[unsafe(no_mangle)]
    extern "C" fn __test_fn(
        _ecx: NonNull<EvmContext<'_>>,
        _stack: NonNull<EvmStack>,
        _stack_len: NonNull<usize>,
    ) -> InstrStop {
        InstrStop::Stop
    }

    #[test]
    fn extern_macro() {
        let f1 = EvmCompilerFn::new(test_fn).0;
        let f2 = EvmCompilerFn::new(__test_fn).0;
        assert!(core::ptr::fn_addr_eq(f1, f2), "{f1:?} != {f2:?}");
    }
}
