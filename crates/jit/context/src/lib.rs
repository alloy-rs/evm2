#![doc = include_str!("../README.md")]
#![cfg_attr(not(test), warn(unused_extern_crates))]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

use alloc::vec::Vec;
use alloy_primitives::{Address, B256, Bytes, Log, U256, ruint};
use core::{fmt, mem::MaybeUninit, ptr::NonNull};
pub use evm2::interpreter::InstrStop;
use revm_interpreter::{
    Gas, Host, InputsImpl, SharedMemory, context_interface::cfg::GasParams,
    interpreter_types::MemoryTr,
};

mod arch;
use arch::revmc_entry;
pub use arch::revmc_exit;

#[cfg(feature = "evm2")]
pub mod evm2_api;

/// Type-erased evm2 recursive message builtin.
#[doc(hidden)]
pub type Evm2RecursiveMessageFn =
    unsafe extern "C" fn(&mut EvmContext<'_>, *mut EvmWord, u8) -> InstrStop;

/// Dispatches recursive evm2 call/create messages from compiled code.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
#[doc(hidden)]
pub struct Evm2Recursion {
    /// Executes `CREATE` or `CREATE2`.
    pub create: Evm2RecursiveMessageFn,
    /// Executes `CALL`, `CALLCODE`, `DELEGATECALL`, or `STATICCALL`.
    pub call: Evm2RecursiveMessageFn,
}

impl Evm2Recursion {
    /// Returns dispatch functions that reject recursive message opcodes.
    #[inline]
    pub const fn unsupported() -> Self {
        Self {
            create: unsupported_evm2_recursive_message,
            call: unsupported_evm2_recursive_message,
        }
    }

    /// Creates a recursive message dispatch table.
    #[inline]
    pub const fn new(create: Evm2RecursiveMessageFn, call: Evm2RecursiveMessageFn) -> Self {
        Self { create, call }
    }
}

unsafe extern "C" fn unsupported_evm2_recursive_message(
    _ecx: &mut EvmContext<'_>,
    _sp: *mut EvmWord,
    _kind: u8,
) -> InstrStop {
    InstrStop::FatalExternalError
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
    /// The memory.
    pub memory: &'a mut SharedMemory,
    /// Input information (target address, caller, input data, call value).
    pub input: &'a mut InputsImpl,
    /// The gas.
    pub gas: Gas,
    /// The host.
    pub host: &'a mut dyn Host,
    /// The return data.
    pub return_data: &'a [u8],
    /// Whether the context is static.
    pub is_static: bool,
    /// The spec ID for the current execution.
    pub spec_id: u8,
    /// The contract bytecode, for CODECOPY at runtime.
    pub bytecode: *const [u8],
    /// Optional callback invoked by the LOG builtin after constructing the log,
    /// **before** it is passed to [`Host::log`].
    ///
    /// Set to `None` when no inspector is active.
    #[doc(hidden)]
    pub on_log: Option<&'a mut (dyn FnMut(&Log) + 'a)>,
    /// The size of the call input data, cached for CALLDATASIZE.
    pub calldatasize: usize,
    /// The result set by a builtin before exiting via [`revmc_exit`].
    pub exit_result: InstrStop,
    /// Saved RSP from the entry trampoline, used by [`revmc_exit`] to unwind.
    pub exit_sp: *mut u8,
    /// Cached gas parameters from the host.
    pub gas_params: GasParams,
    /// Cached base pointer for the current memory context.
    /// Points to `memory[checkpoint..]`, i.e. the start of the current context's memory.
    /// Refreshed after any memory resize.
    pub mem_base: *mut u8,
    /// Cached length of the current memory context in bytes.
    /// Refreshed after any memory resize.
    pub mem_len: usize,
    /// Output produced by RETURN or REVERT.
    #[doc(hidden)]
    pub output: Bytes,
    /// Recursive evm2 call/create dispatch used by call-like builtins.
    #[doc(hidden)]
    pub evm2_recursion: Evm2Recursion,
}

// Static assertions to ensure the struct layout matches expectations.
// These offsets are used by the JIT compiler to access fields.
const _: () = {
    use core::mem::offset_of;

    // Key fields accessed by JIT code
    assert!(offset_of!(EvmContext<'_>, memory) == 0);
};

impl fmt::Debug for EvmContext<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EvmContext").field("memory", &self.memory).finish_non_exhaustive()
    }
}

impl EvmContext<'_> {
    /// Refreshes the cached memory base pointer and length from `SharedMemory`.
    ///
    /// Must be called after any operation that may resize memory.
    #[inline]
    pub fn refresh_memory_cache(&mut self) {
        let mut slice = self.memory.context_memory_mut();
        self.mem_base = slice.as_mut_ptr();
        self.mem_len = slice.len();
    }
}

/// Performs EVM memory resize and charges memory expansion gas.
#[inline]
pub fn resize_memory<Memory: MemoryTr>(
    gas: &mut Gas,
    memory: &mut Memory,
    gas_params: &GasParams,
    offset: usize,
    len: usize,
) -> Result<(), InstrStop> {
    let new_num_words = offset.saturating_add(len).div_ceil(32);
    if new_num_words > gas.memory().words_num {
        return resize_memory_cold(gas, memory, gas_params, new_num_words);
    }

    Ok(())
}

#[cold]
#[inline(never)]
fn resize_memory_cold<Memory: MemoryTr>(
    gas: &mut Gas,
    memory: &mut Memory,
    gas_params: &GasParams,
    new_num_words: usize,
) -> Result<(), InstrStop> {
    let Some(new_size) = new_num_words.checked_mul(32) else {
        return Err(InstrStop::MemoryOOG);
    };

    let cost = gas_params.memory_cost(new_num_words);
    let cost = unsafe { gas.memory_mut().set_words_num(new_num_words, cost).unwrap_unchecked() };

    if !gas.record_regular_cost(cost) {
        return Err(InstrStop::MemoryOOG);
    }
    memory.resize(new_size);
    Ok(())
}

/// Declare [`RawEvmCompilerFn`] functions in an `extern "C"` block.
///
/// # Examples
///
/// ```no_run
/// use evm2_jit_context::{EvmCompilerFn, extern_revmc};
///
/// extern_revmc! {
///    /// A simple function.
///    pub fn test_fn;
/// }
///
/// let test_fn = EvmCompilerFn::new(test_fn);
/// ```
#[macro_export]
macro_rules! extern_revmc {
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
        revmc_entry(NonNull::from(ecx), NonNull::from(stack), NonNull::from(stack_len), self.0)
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
        self.call(stack, stack_len, ecx)
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
        self.0.get_unchecked(index).assume_init_ref()
    }

    /// Returns the word at the given index as a mutable reference.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the index is within bounds.
    #[inline]
    pub unsafe fn get_unchecked_mut(&mut self, index: usize) -> &mut EvmWord {
        self.0.get_unchecked_mut(index).assume_init_mut()
    }

    /// Sets the value at the top of the stack to `value`, and grows the stack by 1.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the stack is not full.
    #[inline]
    pub unsafe fn push(&mut self, value: EvmWord, len: &mut usize) {
        self.set_unchecked(*len, value);
        *len += 1;
    }

    /// Returns the value at the top of the stack.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the stack is not empty.
    #[inline]
    pub unsafe fn top_unchecked(&self, len: usize) -> &EvmWord {
        self.get_unchecked(len - 1)
    }

    /// Returns the value at the top of the stack as a mutable reference.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the stack is not empty.
    #[inline]
    pub unsafe fn top_unchecked_mut(&mut self, len: usize) -> &mut EvmWord {
        self.get_unchecked_mut(len - 1)
    }

    /// Returns the value at the given index from the top of the stack.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `len >= n + 1`.
    #[inline]
    pub unsafe fn from_top_unchecked(&self, len: usize, n: usize) -> &EvmWord {
        self.get_unchecked(len - n - 1)
    }

    /// Returns the value at the given index from the top of the stack as a mutable reference.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `len >= n + 1`.
    #[inline]
    pub unsafe fn from_top_unchecked_mut(&mut self, len: usize, n: usize) -> &mut EvmWord {
        self.get_unchecked_mut(len - n - 1)
    }

    /// Sets the value at the given index.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the index is within bounds.
    #[inline]
    pub unsafe fn set_unchecked(&mut self, index: usize, value: EvmWord) {
        *self.0.get_unchecked_mut(index) = MaybeUninit::new(value);
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

// Macro re-exports.
// Not public API.
#[doc(hidden)]
pub mod private {
    pub use revm_interpreter;
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

    extern_revmc! {
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
