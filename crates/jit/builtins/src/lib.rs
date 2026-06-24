#![doc = include_str!("../README.md")]
#![allow(missing_docs, clippy::missing_safety_doc)]
#![cfg_attr(not(test), warn(unused_extern_crates))]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

#[macro_use]
#[cfg(feature = "ir")]
extern crate tracing;

use alloc::vec::Vec;
use alloy_primitives::{Address, B256, Bytes, KECCAK256_EMPTY, Log, LogData, U256, keccak256};
use core::cmp::min;
use evm2::{
    bytecode::Bytecode,
    interpreter::{Host, Message, MessageKind, Word, i256},
    utils::{word_to_usize, word_to_usize_saturated},
    version::{EvmFeatures, GasId},
};
use evm2_jit_context::{EvmContext, EvmWord, InstrStop};

pub mod gas;

#[cfg(feature = "ir")]
mod ir;
#[cfg(feature = "ir")]
pub use ir::*;

#[macro_use]
mod macros;

mod utils;
use utils::*;
pub use utils::{BuiltinError, BuiltinResult};

/// The kind of a `*CALL*` instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum CallKind {
    /// `CALL`.
    Call,
    /// `CALLCODE`.
    CallCode,
    /// `DELEGATECALL`.
    DelegateCall,
    /// `STATICCALL`.
    StaticCall,
}

impl From<CallKind> for MessageKind {
    fn from(kind: CallKind) -> Self {
        match kind {
            CallKind::Call => Self::Call,
            CallKind::CallCode => Self::CallCode,
            CallKind::DelegateCall => Self::DelegateCall,
            CallKind::StaticCall => Self::StaticCall,
        }
    }
}

/// The kind of a `CREATE*` instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum CreateKind {
    /// `CREATE`.
    Create,
    /// `CREATE2`.
    Create2,
}

// NOTE: All functions MUST be `extern "C"` and their parameters must match `Builtin` enum.
//
// The `sp` parameter always points to the last popped stack element.
// If results are expected to be pushed back onto the stack, they must be written to the read
// pointers in **reverse order**, meaning the last pointer is the first return value.

#[inline(always)]
#[cold]
fn fail(ecx: &mut EvmContext<'_>, e: BuiltinError) -> ! {
    ecx.exit_result = e.into();
    unsafe { evm2_jit_context::evm2_jit_exit(ecx) }
}

macro_rules! builtins {
    () => {};

    // Fallible builtins: `-> BuiltinResult` is stripped and errors route through `fail()`.
    ($(#[$attr:meta])* pub unsafe extern "C" fn $name:ident($ecx:ident : &mut EvmContext<'_> $(, $rest_i:ident : $rest_t:ty)* $(,)?) -> BuiltinResult $block:block $($more:tt)*) => {
        $(#[$attr])*
        pub unsafe extern "C" fn $name($ecx: &mut EvmContext<'_> $(, $rest_i : $rest_t)*) {
            #[inline(always)]
            unsafe fn imp($ecx: &mut EvmContext<'_> $(, $rest_i : $rest_t)*) -> BuiltinResult $block
            match unsafe { imp($ecx $(, $rest_i)*) } {
                Ok(()) => {}
                Err(e) => fail($ecx, e),
            }
        }

        builtins! { $($more)* }
    };

    // Infallible items pass through unchanged.
    ($item:item $($rest:tt)*) => {
        $item
        builtins! { $($rest)* }
    };
}

builtins! {

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_panic(data: *const u8, len: usize) -> ! {
    let msg = unsafe { core::str::from_utf8_unchecked(core::slice::from_raw_parts(data, len)) };
    panic!("{msg}");
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_assert_spec_id(ecx: &EvmContext<'_>, expected: u32) {
    assert_eq!(
        u32::from(ecx.spec_id()), expected,
        "evm2_jit panic: runtime spec_id does not match compilation spec_id"
    );
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_div(rev![a, b]: &mut [EvmWord; 2]) {
    let divisor = b.to_u256();
    *b = if divisor.is_zero() { U256::ZERO } else { a.to_u256().wrapping_div(divisor) }.into();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_sdiv(rev![a, b]: &mut [EvmWord; 2]) {
    *b = i256::i256_div(a.to_u256(), b.to_u256()).into();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_mod(rev![a, b]: &mut [EvmWord; 2]) {
    let divisor = b.to_u256();
    *b = if divisor.is_zero() { U256::ZERO } else { a.to_u256().wrapping_rem(divisor) }.into();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_smod(rev![a, b]: &mut [EvmWord; 2]) {
    *b = i256::i256_mod(a.to_u256(), b.to_u256()).into();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_addmod(rev![a, b, c]: &mut [EvmWord; 3]) {
    *c = a.to_u256().add_mod(b.to_u256(), c.to_u256()).into();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_mulmod(rev![a, b, c]: &mut [EvmWord; 3]) {
    *c = a.to_u256().mul_mod(b.to_u256(), c.to_u256()).into();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_exp(
    ecx: &mut EvmContext<'_>,
    sp: &mut [EvmWord; 2],
) -> BuiltinResult {
    let rev![base, exponent_ptr] = sp;
    let exponent = exponent_ptr.to_u256();
    ecx.gas.spend(ecx.gas_params().exp_cost(exponent))?;
    *exponent_ptr = base.to_u256().pow(exponent).into();
    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_exp_gas(
    ecx: &mut EvmContext<'_>,
    exponent: &EvmWord,
) -> BuiltinResult {
    ecx.gas.spend(ecx.gas_params().exp_cost(exponent.to_u256()))?;
    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_keccak256(
    ecx: &mut EvmContext<'_>,
    sp: &mut [EvmWord; 2],
) -> BuiltinResult {
    let rev![offset, len_ptr] = sp;
    let len = word_to_usize(len_ptr.to_u256())?;
    do_keccak256(ecx, len_ptr, *offset, len)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_keccak256_cc(
    ecx: &mut EvmContext<'_>,
    out: &mut EvmWord,
    offset: u64,
    len: u64,
) -> BuiltinResult {
    do_keccak256(ecx, out, U256::from(offset).into(), len as usize)
}

fn do_keccak256(
    ecx: &mut EvmContext<'_>,
    out: &mut EvmWord,
    offset: EvmWord,
    len: usize,
) -> BuiltinResult {
    *out = EvmWord::from_be_bytes(if len == 0 {
        KECCAK256_EMPTY
    } else {
        ecx.gas.spend(ecx.gas_params().keccak256_word_cost(len))?;
        let offset = word_to_usize(offset.to_u256())?;
        ensure_memory(ecx, offset, len)?;
        keccak256(ecx.memory().slice(offset, len))
    });
    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_balance(
    ecx: &mut EvmContext<'_>,
    address: &mut EvmWord,
) -> BuiltinResult {
    let account = load_account(ecx, &address.to_address(), false)?;
    *address = account.balance.into();
    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_origin(ecx: &EvmContext<'_>, slot: &mut EvmWord) {
    *slot = EvmWord::from_be_bytes(ecx.tx_env().origin.into_word());
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_calldataload(
    ecx: &EvmContext<'_>,
    offset_ptr: &mut EvmWord,
) {
    do_calldataload(ecx, offset_ptr, word_to_usize_saturated(offset_ptr.to_u256()));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_calldataload_c(
    ecx: &EvmContext<'_>,
    offset_ptr: &mut EvmWord,
    offset: u64,
) {
    do_calldataload(ecx, offset_ptr, offset as usize);
}

fn do_calldataload(ecx: &EvmContext<'_>, out: &mut EvmWord, offset: usize) {
    let mut word = B256::ZERO;
    let input = ecx.input();
    let input_len = input.len();
    if offset < input_len {
        let count = 32.min(input_len - offset);
        let input = ecx.input().as_ref();
        // SAFETY: `count` is bounded by the calldata length.
        // This is `word[..count].copy_from_slice(input[offset..offset + count])`, written using
        // raw pointers as apparently the compiler cannot optimize the slice version, and using
        // `get_unchecked` twice is uglier.
        unsafe {
            core::ptr::copy_nonoverlapping(input.as_ptr().add(offset), word.as_mut_ptr(), count)
        };
    }
    *out = EvmWord::from_be_bytes(word);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_calldatacopy(
    ecx: &mut EvmContext<'_>,
    sp: &mut [EvmWord; 3],
) -> BuiltinResult {
    let rev![memory_offset, data_offset, len] = sp;
    let len = word_to_usize(len.to_u256())?;
    if len != 0 {
        ecx.gas.spend(ecx.gas_params().copy_cost(len))?;
        let memory_offset = word_to_usize(memory_offset.to_u256())?;
        ensure_memory(ecx, memory_offset, len)?;
        let data_offset = word_to_usize_saturated(data_offset.to_u256());
        let input = ecx.input().as_ref();
        let input = unsafe { core::slice::from_raw_parts(input.as_ptr(), input.len()) };
        ecx.memory_mut().set_data(memory_offset, data_offset, len, input);
    }
    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_codecopy(
    ecx: &mut EvmContext<'_>,
    sp: &mut [EvmWord; 3],
) -> BuiltinResult {
    let bytecode = ecx.bytecode();
    unsafe { copy_operation(ecx, sp, bytecode.as_ref()) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_gas_price(ecx: &EvmContext<'_>, slot: &mut EvmWord) {
    *slot = ecx.tx_env().gas_price.into();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_extcodesize(
    ecx: &mut EvmContext<'_>,
    address: &mut EvmWord,
) -> BuiltinResult {
    let account = load_account(ecx, &address.to_address(), true)?;
    *address = U256::from(account.code.len()).into();
    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_extcodecopy(
    ecx: &mut EvmContext<'_>,
    sp: &mut [EvmWord; 4],
) -> BuiltinResult {
    let rev![address, memory_offset, code_offset, len] = sp;
    let len = word_to_usize(len.to_u256())?;
    ecx.gas.spend(ecx.gas_params().extcodecopy_cost(len))?;

    let mut memory_offset_usize = 0;
    if len != 0 {
        memory_offset_usize = word_to_usize(memory_offset.to_u256())?;
        ensure_memory(ecx, memory_offset_usize, len)?;
    }

    let account = load_account(ecx, &address.to_address(), true)?;
    let code = account.code.original_bytes();

    let code_offset_usize = core::cmp::min(word_to_usize_saturated(code_offset.to_u256()), code.len());
    ecx.memory_mut().set_data(memory_offset_usize, code_offset_usize, len, &code);
    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_returndatacopy(
    ecx: &mut EvmContext<'_>,
    sp: &mut [EvmWord; 3],
) -> BuiltinResult {
    let rev![memory_offset, offset, len] = sp;
    let len = word_to_usize(len.to_u256())?;
    let data_offset = word_to_usize_saturated(offset.to_u256());

    // Bounds check before charging gas.
    let data_end = data_offset.saturating_add(len);
    if data_end > ecx.return_data().len() {
        return Err(InstrStop::OutOfOffset.into());
    }

    ecx.gas.spend(ecx.gas_params().copy_cost(len))?;
    if len != 0 {
        let memory_offset = word_to_usize(memory_offset.to_u256())?;
        ensure_memory(ecx, memory_offset, len)?;
        let return_data = ecx.return_data();
        let data =
            unsafe { core::slice::from_raw_parts(return_data.as_ptr().add(data_offset), len) };
        ecx.memory_mut().set(memory_offset, data);
    }
    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_extcodehash(
    ecx: &mut EvmContext<'_>,
    address: &mut EvmWord,
) -> BuiltinResult {
    let account = load_account(ecx, &address.to_address(), false)?;
    let code_hash = if account.is_empty { B256::ZERO } else { account.code_hash };
    *address = EvmWord::from_be_bytes(code_hash);
    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_blockhash(
    ecx: &mut EvmContext<'_>,
    number_ptr: &mut EvmWord,
) -> BuiltinResult {
    let requested_number = number_ptr.to_u256();
    let block_number = ecx.block_env().number;

    // Check if requested block is in the future
    let Some(diff) = block_number.checked_sub(requested_number) else {
        *number_ptr = EvmWord::ZERO;
        return Ok(());
    };

    let diff = word_to_u64_saturated(diff);

    // Current block returns 0
    if diff == 0 {
        *number_ptr = EvmWord::ZERO;
        return Ok(());
    }

    // BLOCK_HASH_HISTORY is 256
    const BLOCK_HASH_HISTORY: u64 = 256;

    if diff <= BLOCK_HASH_HISTORY {
        let requested_number = U256::from(word_to_u64_saturated(requested_number));
        let hash = ecx.host().block_hash(&requested_number).ok().flatten().ok_or_fatal()?;
        *number_ptr = EvmWord::from_be_bytes(hash);
    } else {
        // Too old, return 0
        *number_ptr = EvmWord::ZERO;
    }

    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_coinbase(ecx: &mut EvmContext<'_>, slot: &mut EvmWord) {
    *slot = EvmWord::from_be_bytes(ecx.block_env().beneficiary.into_word());
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_timestamp(ecx: &mut EvmContext<'_>, slot: &mut EvmWord) {
    *slot = ecx.block_env().timestamp.into();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_number(ecx: &mut EvmContext<'_>, slot: &mut EvmWord) {
    *slot = ecx.block_env().number.into();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_difficulty(ecx: &mut EvmContext<'_>, slot: &mut EvmWord) {
    *slot = if ecx.enables(EvmFeatures::EIP4399) {
        ecx.block_env().prevrandao.into()
    } else {
        ecx.block_env().difficulty.into()
    };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_gaslimit(ecx: &mut EvmContext<'_>, slot: &mut EvmWord) {
    *slot = ecx.block_env().gas_limit.into();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_chainid(ecx: &EvmContext<'_>, slot: &mut EvmWord) {
    *slot = ecx.tx_env().chain_id.into();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_self_balance(
    ecx: &mut EvmContext<'_>,
    slot: &mut EvmWord,
) -> BuiltinResult {
    let balance = load_account(ecx, &ecx.message().destination, false)?.balance;
    *slot = balance.into();
    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_basefee(ecx: &mut EvmContext<'_>, slot: &mut EvmWord) {
    *slot = ecx.block_env().basefee.into();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_blob_hash(ecx: &EvmContext<'_>, index_ptr: &mut EvmWord) {
    let index = index_ptr.to_u256();
    let index_usize = word_to_usize_saturated(index);
    *index_ptr = ecx.tx_env().blob_hashes.get(index_usize).copied().unwrap_or_default().into();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_blob_base_fee(
    ecx: &mut EvmContext<'_>,
    slot: &mut EvmWord,
) {
    *slot = ecx.block_env().blob_basefee.into();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_mresize(ecx: &mut EvmContext<'_>, min_size: u64) -> BuiltinResult {
    ensure_memory(ecx, min_size as usize, 0)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_slot_num(ecx: &mut EvmContext<'_>, slot: &mut EvmWord) {
    *slot = ecx.block_env().slot_num.into();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_sload(
    ecx: &mut EvmContext<'_>,
    index: &mut EvmWord,
) -> BuiltinResult {
    let address = &ecx.message().destination;
    let key = index.to_u256();
    let additional_cold_cost = u64::from(ecx.gas_params().get(GasId::ColdStorageAdditionalCost));
    let skip_cold =
        ecx.enables(EvmFeatures::EIP2929) && ecx.gas.remaining() < additional_cold_cost;
    let storage =
        ecx.host().sload(address, &key, skip_cold).map_err(|stop| host_error_stop(stop, skip_cold))?;
    if storage.is_cold {
        ecx.gas.spend(additional_cold_cost)?;
    }
    *index = storage.value.into();

    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_sload_c(
    ecx: &mut EvmContext<'_>,
    index: &mut EvmWord,
    key: u64,
) -> BuiltinResult {
    *index = U256::from(key).into();
    unsafe { __revmc_builtin_sload(ecx, index) };
    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_sstore(
    ecx: &mut EvmContext<'_>,
    sp: &mut [EvmWord; 2],
) -> BuiltinResult {
    let rev![index, value] = sp;
    require_non_staticcall(ecx)?;

    let target = &ecx.message().destination;
    let is_eip2200 = ecx.enables(EvmFeatures::EIP2200);

    // EIP-2200: If gasleft is less than or equal to gas stipend, fail with OOG.
    if is_eip2200 && ecx.gas.remaining() <= u64::from(ecx.gas_params().get(GasId::CallStipend)) {
        return Err(InstrStop::ReentrancySentryOOG.into());
    }

    ecx.gas.spend(u64::from(ecx.gas_params().get(GasId::SstoreStatic)))?;

    let additional_cold_cost = u64::from(ecx.gas_params().get(GasId::ColdStorageAdditionalCost));
    let skip_cold =
        ecx.enables(EvmFeatures::EIP2929) && ecx.gas.remaining() < additional_cold_cost;
    let index = index.to_u256();
    let value = value.to_u256();
    let state_load = ecx
        .host()
        .sstore(target, &index, &value, skip_cold)
        .map_err(|stop| host_error_stop(stop, skip_cold))?;

    let dynamic_gas = ecx.gas_params().sstore_dynamic_gas(is_eip2200, &state_load);
    ecx.gas.spend(dynamic_gas)?;

    // State gas for new slot creation (EIP-8037).
    if ecx.enables(EvmFeatures::EIP8037) {
        let state_gas = ecx.gas_params().sstore_state_gas(&state_load);
        ecx.gas.spend_state(state_gas)?;
    }

    let refund = ecx.gas_params().sstore_refund(is_eip2200, &state_load);
    ecx.gas.record_refund(refund);
    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_tload(ecx: &mut EvmContext<'_>, key: &mut EvmWord) {
    let target = &ecx.message().destination;
    let key_word = key.to_u256();
    *key = ecx.host().tload(target, &key_word).into();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_tstore(
    ecx: &mut EvmContext<'_>,
    sp: &mut [EvmWord; 2],
) -> BuiltinResult {
    let rev![key, value] = sp;
    require_non_staticcall(ecx)?;
    let target = &ecx.message().destination;
    ecx.host().tstore(target, &key.to_u256(), &value.to_u256());
    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_mcopy(
    ecx: &mut EvmContext<'_>,
    sp: &mut [EvmWord; 3],
) -> BuiltinResult {
    let rev![dst, src, len] = sp;
    let len = word_to_usize(len.to_u256())?;
    ecx.gas.spend(ecx.gas_params().mcopy_cost(len))?;
    if len != 0 {
        let dst = word_to_usize(dst.to_u256())?;
        let src = word_to_usize(src.to_u256())?;
        ensure_memory(ecx, dst.max(src), len)?;
        ecx.memory_mut().copy(dst, src, len);
    }
    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_log(
    ecx: &mut EvmContext<'_>,
    sp: *mut EvmWord,
    n: u8,
) -> BuiltinResult {
    require_non_staticcall(ecx)?;
    assume!(n <= 4, "invalid log topic count: {n}");
    let sp = unsafe { sp.add(n as usize) };
    let rev![offset, len] = unsafe { read_words_rev(sp) };
    let len = word_to_usize(len.to_u256())?;
    ecx.gas.spend(ecx.gas_params().log_cost(n, len))?;
    let data = if len != 0 {
        let offset = word_to_usize(offset.to_u256())?;
        ensure_memory(ecx, offset, len)?;
        Bytes::copy_from_slice(ecx.memory().slice(offset, len))
    } else {
        Bytes::new()
    };

    let mut topics = Vec::with_capacity(n as usize);
    for i in 1..=n {
        topics.push(unsafe { sp.sub(i as usize).read() }.to_be_bytes());
    }

    let address = ecx.message().destination;
    let log = Log {
        address,
        data: LogData::new(topics, data).expect("too many topics"),
    };
    ecx.host().log(log);
    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_create(
    ecx: &mut EvmContext<'_>,
    sp: *mut EvmWord,
    create_kind: CreateKind,
) -> BuiltinResult {
    require_non_staticcall(ecx)?;

    let len = match create_kind {
        CreateKind::Create => 3,
        CreateKind::Create2 => 4,
    };
    let mut sp = unsafe { sp.add(len) };
    pop!(sp; value, code_offset, len);

    let len = word_to_usize(len.to_u256())?;
    let version = *ecx.version();
    let code = if len != 0 {
        if ecx.enables(EvmFeatures::EIP3860) {
            if len > version.max_initcode_size {
                return Err(InstrStop::CreateInitCodeSizeLimit.into());
            }
            ecx.gas.spend(version.gas_params.initcode_cost(len))?;
        }

        let code_offset = word_to_usize(code_offset.to_u256())?;
        ensure_memory(ecx, code_offset, len)?;
        Bytes::copy_from_slice(ecx.memory().slice(code_offset, len))
    } else {
        Bytes::new()
    };

    let is_create2 = create_kind == CreateKind::Create2;
    let create_cost = if is_create2 {
        version.gas_params.create2_cost(len)
    } else {
        version.gas_params.get(GasId::Create).into()
    };
    ecx.gas.spend(create_cost)?;

    let mut gas_limit = ecx.gas.remaining();
    if ecx.enables(EvmFeatures::EIP150) {
        gas_limit = version.gas_params.call_stipend_reduction(gas_limit);
    }
    ecx.gas.spend(gas_limit)?;
    let salt = if is_create2 {
        pop!(sp; salt);
        B256::from(salt.to_be_bytes())
    } else {
        B256::ZERO
    };

    let current = ecx.message();
    let mut message = Message {
        kind: if is_create2 { MessageKind::Create2 } else { MessageKind::Create },
        depth: current.depth.saturating_add(1),
        gas_limit,
        destination: current.destination,
        caller: current.destination,
        input: code,
        value: value.to_u256(),
        code_address: current.destination,
        disable_precompiles: false,
        caller_is_static: false,
        salt,
        ext: (),
        _non_exhaustive: (),
    };
    let bytecode = Bytecode::new_legacy(message.input.clone());
    let tx_env = ecx.tx_env();
    let result = ecx.host().execute_message(tx_env, bytecode, &mut message);
    ecx.gas.erase_cost(result.gas_returned_to_parent());
    ecx.gas.record_refund(result.refund_propagated_to_parent());

    let return_data = if result.stop == InstrStop::Revert { result.output } else { Bytes::new() };
    let address = result
        .created_address
        .filter(|_| result.stop.is_success())
        .map(|address| EvmWord::from_be_slice(address.as_slice()))
        .unwrap_or_default();
    unsafe {
        sp.write(address);
    }
    ecx.set_return_data(return_data);
    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_call(
    ecx: &mut EvmContext<'_>,
    sp: *mut EvmWord,
    call_kind: CallKind,
) -> BuiltinResult {
    let len = match call_kind {
        CallKind::Call | CallKind::CallCode => 7,
        CallKind::DelegateCall | CallKind::StaticCall => 6,
    };
    let mut sp = unsafe { sp.add(len) };

    pop!(sp; local_gas_limit, to);
    let local_gas_limit = local_gas_limit.to_u256();
    let to = to.to_address();

    let local_gas_limit = word_to_u64_saturated(local_gas_limit);

    let value = match call_kind {
        CallKind::Call | CallKind::CallCode => {
            pop!(sp; value);
            let value = value.to_u256();
            if call_kind == CallKind::Call && ecx.is_static() && value != U256::ZERO {
                return Err(InstrStop::CallNotAllowedInsideStatic.into());
            }
            value
        }
        CallKind::DelegateCall | CallKind::StaticCall => U256::ZERO,
    };
    let transfers_value = value != U256::ZERO;

    pop!(sp; in_offset, in_len, out_offset, out_len);

    let in_len = word_to_usize(in_len.to_u256())?;
    let input = if in_len != 0 {
        let in_offset = word_to_usize(in_offset.to_u256())?;
        ensure_memory(ecx, in_offset, in_len)?;
        Bytes::copy_from_slice(ecx.memory().slice(in_offset, in_len))
    } else {
        Bytes::new()
    };

    let out_len = word_to_usize(out_len.to_u256())?;
    let out_offset = if out_len != 0 {
        let out_offset = word_to_usize(out_offset.to_u256())?;
        ensure_memory(ecx, out_offset, out_len)?;
        out_offset
    } else {
        usize::MAX
    };

    let (gas_limit, loaded_code, resolved_code_address, disable_precompiles) =
        load_acc_and_calc_gas(ecx, to, transfers_value, call_kind == CallKind::Call, local_gas_limit)?;

    let current = ecx.message();
    let (destination, caller, call_value, code_address) = match call_kind {
        CallKind::Call => (to, current.destination, value, resolved_code_address),
        CallKind::CallCode => {
            (current.destination, current.destination, value, resolved_code_address)
        }
        CallKind::DelegateCall => {
            (current.destination, current.caller, current.value, resolved_code_address)
        }
        CallKind::StaticCall => (to, current.destination, U256::ZERO, resolved_code_address),
    };
    let mut message = Message {
        kind: call_kind.into(),
        depth: current.depth.saturating_add(1),
        gas_limit,
        destination,
        caller,
        input,
        value: call_value,
        code_address,
        disable_precompiles,
        caller_is_static: ecx.is_static(),
        salt: B256::ZERO,
        ext: (),
        _non_exhaustive: (),
    };

    let tx_env = ecx.tx_env();
    let mut result = ecx.host().execute_message(tx_env, loaded_code, &mut message);
    ecx.gas.erase_cost(result.gas_returned_to_parent());
    ecx.gas.record_refund(result.refund_propagated_to_parent());

    let copy_len = min(out_len, result.output.len());
    if copy_len != 0 {
        ecx.memory_mut().set(out_offset, &result.output[..copy_len]);
    }
    let success = EvmWord::from(Word::from(u8::from(result.stop.is_success())));
    unsafe {
        sp.write(success);
    }
    ecx.set_return_data(core::mem::take(&mut result.output));
    Ok(())
}

const fn should_charge_new_account_gas(
    eip161: bool,
    transfers_value: bool,
    target_is_empty_for_new_account_gas: bool,
) -> bool {
    target_is_empty_for_new_account_gas && (!eip161 || transfers_value)
}

fn load_acc_and_calc_gas(
    ecx: &mut EvmContext<'_>,
    to: Address,
    transfers_value: bool,
    create_empty_account: bool,
    stack_gas_limit: u64,
) -> Result<(u64, Bytecode, Address, bool), BuiltinError> {
    let version = *ecx.version();
    if transfers_value {
        ecx.gas.spend(version.gas_params.get(GasId::TransferValueCost).into())?;
    }

    let additional_cold_cost = version.gas_params.cold_account_additional_cost();
    let remaining_gas = ecx.gas.remaining();
    let skip_cold_load = remaining_gas < additional_cold_cost;
    let account = ecx
        .host()
        .load_account(&to, true, skip_cold_load)
        .map_err(|stop| host_error_stop(stop, skip_cold_load))?;

    let mut cost = 0;
    if account.is_cold {
        cost += additional_cold_cost;
    }
    let mut code = account.code;
    let mut code_address = to;
    if ecx.enables(EvmFeatures::EIP7702)
        && let Some(delegated_address) = code.eip7702_address()
    {
        cost += u64::from(version.gas_params.get(GasId::WarmStorageReadCost));
        if cost > remaining_gas {
            return Err(InstrStop::OutOfGas.into());
        }
        let skip_cold_load = remaining_gas < cost.saturating_add(additional_cold_cost);
        let delegated_account = ecx
            .host()
            .load_account(&delegated_address, true, skip_cold_load)
            .map_err(|stop| host_error_stop(stop, skip_cold_load))?;
        if delegated_account.is_cold {
            cost += additional_cold_cost;
        }
        code = delegated_account.code;
        code_address = delegated_address;
    }
    let spec_id = ecx.spec_id();
    if create_empty_account
        && should_charge_new_account_gas(
            ecx.enables(EvmFeatures::EIP161),
            transfers_value,
            ecx.host().target_is_empty_for_new_account_gas(&to, spec_id)?,
        )
    {
        cost += u64::from(version.gas_params.get(GasId::NewAccountCost));
    }
    ecx.gas.spend(cost)?;

    let mut gas_limit = if ecx.enables(EvmFeatures::EIP150) {
        min(version.gas_params.call_stipend_reduction(ecx.gas.remaining()), stack_gas_limit)
    } else {
        stack_gas_limit
    };
    ecx.gas.spend(gas_limit)?;

    if transfers_value {
        gas_limit = gas_limit.saturating_add(version.gas_params.get(GasId::CallStipend).into());
    }

    let disable_precompiles = code_address != to;
    Ok((gas_limit, code, code_address, disable_precompiles))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_do_return(
    ecx: &mut EvmContext<'_>,
    sp: &mut [EvmWord; 2],
    result: InstrStop,
) -> BuiltinResult {
    let rev![offset, len] = sp;
    let len = word_to_usize(len.to_u256())?;
    let output = if len != 0 {
        let offset = word_to_usize(offset.to_u256())?;
        let Some(end) = offset.checked_add(len) else {
            return Err(InstrStop::MemoryOOG.into());
        };
        if end > u32::MAX as usize {
            return Err(InstrStop::MemoryLimitOOG.into());
        }
        ensure_memory(ecx, offset, len)?;
        offset as u32..end as u32
    } else {
        0..0
    };
    ecx.interpreter_mut().set_output(output);
    Err(result.into())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_do_return_cc(
    ecx: &mut EvmContext<'_>,
    offset: u64,
    len: u64,
    result: InstrStop,
) -> BuiltinResult {
    let offset = offset as usize;
    let len = len as usize;
    let output = if len != 0 {
        let Some(end) = offset.checked_add(len) else {
            return Err(InstrStop::MemoryOOG.into());
        };
        if end > u32::MAX as usize {
            return Err(InstrStop::MemoryLimitOOG.into());
        }
        ensure_memory(ecx, offset, len)?;
        offset as u32..end as u32
    } else {
        0..0
    };
    ecx.interpreter_mut().set_output(output);
    Err(result.into())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_selfdestruct(
    ecx: &mut EvmContext<'_>,
    target: &mut EvmWord,
) -> BuiltinResult {
    require_non_staticcall(ecx)?;

    let cold_load_gas = ecx.gas_params().selfdestruct_cold_cost();
    let skip_cold_load = ecx.gas.remaining() < cold_load_gas;
    let address = &ecx.message().destination;
    let target = target.to_address();
    let res = ecx
        .host()
        .selfdestruct(address, &target, skip_cold_load)
        .map_err(|stop| host_error_stop(stop, skip_cold_load))?;

    // EIP-161: State trie clearing (invariant-preserving alternative)
    let should_charge_topup =
        should_charge_new_account_gas(ecx.enables(EvmFeatures::EIP161), res.had_value, res.target_is_empty);

    ecx.gas.spend(ecx.gas_params().selfdestruct_cost(should_charge_topup, res.is_cold))?;

    // State gas for new account creation (EIP-8037).
    if ecx.enables(EvmFeatures::EIP8037) && should_charge_topup {
        ecx.gas.spend_state(u64::from(ecx.gas_params().get(GasId::NewAccountState)))?;
    }

    if !res.previously_destroyed {
        ecx.gas.record_refund(i64::from(ecx.gas_params().get(GasId::SelfdestructRefund)));
    }

    Err(InstrStop::SelfDestruct.into())
}

}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::boxed::Box;
    use alloy_primitives::Address;
    use evm2::{
        BaseEvmConfigSelector, BaseEvmTypes, Evm, EvmConfigSelector, Precompiles, SpecId,
        env::{BlockEnv, TxEnv},
        ethereum::ethereum_tx_registry,
        evm::{EmptyDB, inspector::Inspector},
        interpreter::{GasTracker, MessageResult, op},
    };
    use evm2_jit_context::EvmStack;

    #[derive(Debug)]
    struct MessageInspector {
        execute_result: MessageResult<BaseEvmTypes>,
        calls: Vec<Message<BaseEvmTypes>>,
        creates: Vec<Message<BaseEvmTypes>>,
        call_static_flags: Vec<bool>,
    }

    impl Default for MessageInspector {
        fn default() -> Self {
            Self {
                execute_result: MessageResult {
                    stop: InstrStop::Return,
                    ..MessageResult::default()
                },
                calls: Vec::new(),
                creates: Vec::new(),
                call_static_flags: Vec::new(),
            }
        }
    }

    impl Inspector<BaseEvmTypes> for MessageInspector {
        fn call(
            &mut self,
            interp: &mut evm2::interpreter::Interpreter<'_, BaseEvmTypes>,
            message: &mut Message<BaseEvmTypes>,
        ) -> Option<MessageResult<BaseEvmTypes>> {
            self.call_static_flags
                .push(interp.is_static() || message.kind == MessageKind::StaticCall);
            self.calls.push(message.clone());
            Some(self.execute_result.clone())
        }

        fn create(
            &mut self,
            _interp: &mut evm2::interpreter::Interpreter<'_, BaseEvmTypes>,
            message: &mut Message<BaseEvmTypes>,
        ) -> Option<MessageResult<BaseEvmTypes>> {
            self.creates.push(message.clone());
            Some(self.execute_result.clone())
        }
    }

    struct PreparedJitFrame<'a> {
        ecx: EvmContext<'a>,
        stack: &'a mut EvmStack,
    }

    fn address_word(address: &Address) -> EvmWord {
        EvmWord::from_be_slice(address.as_slice())
    }

    fn write_call_stack(
        stack: &mut EvmStack,
        target: Address,
        gas_limit: u64,
        value: Option<Word>,
    ) {
        stack.set(0, EvmWord::ZERO);
        stack.set(1, EvmWord::ZERO);
        stack.set(2, EvmWord::ZERO);
        stack.set(3, EvmWord::ZERO);
        if let Some(value) = value {
            stack.set(4, EvmWord::from(value));
            stack.set(5, address_word(&target));
            stack.set(6, EvmWord::from(Word::from(gas_limit)));
        } else {
            stack.set(4, address_word(&target));
            stack.set(5, EvmWord::from(Word::from(gas_limit)));
        }
    }

    fn base_evm(spec_id: SpecId) -> Evm<BaseEvmTypes> {
        Evm::<BaseEvmTypes>::new(
            spec_id,
            BlockEnv::default(),
            ethereum_tx_registry(spec_id),
            EmptyDB::default(),
            Precompiles::base(spec_id),
        )
    }

    fn prepare_frame<'a>(
        interpreter: &'a mut evm2::interpreter::Interpreter<'_, BaseEvmTypes>,
        host: &'a mut Evm<BaseEvmTypes>,
    ) -> PreparedJitFrame<'a> {
        let config = <BaseEvmConfigSelector as EvmConfigSelector<BaseEvmTypes>>::execution_config(
            SpecId::CANCUN,
        );
        let config = Box::leak(Box::new(config));
        interpreter.prepare_jit_run(config, host);
        let (ecx, stack, _stack_len) = EvmContext::from_interpreter_with_stack(interpreter);
        PreparedJitFrame { ecx, stack }
    }

    #[test]
    fn call_builtin_executes_message() {
        let target = Address::from([0x22; 20]);
        let caller = Address::from([0x11; 20]);
        let child_output = Bytes::from_static(&[0xaa, 0xbb, 0xcc]);
        let mut host = base_evm(SpecId::CANCUN);
        host.set_inspector(MessageInspector {
            execute_result: MessageResult {
                stop: InstrStop::Return,
                gas: GasTracker::new(37),
                output: child_output.clone(),
                ..MessageResult::default()
            },
            ..MessageInspector::default()
        });
        let tx_env = TxEnv::default();
        let message = Message { gas_limit: 1_000_000, destination: caller, ..Message::default() };
        let mut interpreter = evm2::interpreter::Interpreter::<BaseEvmTypes>::new(
            Bytecode::new_legacy(Bytes::from_static(&[op::STOP])),
            &tx_env,
            &message,
        );

        {
            let mut frame = prepare_frame(&mut interpreter, &mut host);
            frame.ecx.memory_mut().resize(0, 16).unwrap();
            frame.ecx.memory_mut().set(4, b"in");
            frame.stack.set(0, EvmWord::from(Word::from(2)));
            frame.stack.set(1, EvmWord::from(Word::from(8)));
            frame.stack.set(2, EvmWord::from(Word::from(2)));
            frame.stack.set(3, EvmWord::from(Word::from(4)));
            frame.stack.set(4, EvmWord::ZERO);
            frame.stack.set(5, address_word(&target));
            frame.stack.set(6, EvmWord::from(Word::from(50_000)));

            let sp = frame.stack.as_mut_ptr();
            unsafe { __revmc_builtin_call(&mut frame.ecx, sp, CallKind::Call) };

            assert_eq!(unsafe { frame.stack.get_unchecked(0) }.to_u256(), Word::from(1));
            assert_eq!(frame.ecx.return_data(), child_output.as_ref());
            assert_eq!(frame.ecx.memory().slice(8, 2), &[0xaa, 0xbb]);
            let output = b"frame-output";
            frame.ecx.memory_mut().resize(0, output.len()).unwrap();
            frame.ecx.memory_mut().set(0, output);
            frame.ecx.refresh_memory_cache();
            frame.ecx.interpreter_mut().set_output(0..output.len() as u32);
            assert_eq!(frame.ecx.return_data(), child_output.as_ref());
        }

        let inspector = host.clear_inspector_as::<MessageInspector>().unwrap();
        assert_eq!(inspector.calls.len(), 1);
        assert_eq!(inspector.calls[0].kind, MessageKind::Call);
        assert_eq!(inspector.calls[0].destination, target);
        assert_eq!(inspector.calls[0].caller, caller);
        assert_eq!(inspector.calls[0].input.as_ref(), b"in");
        assert!(!inspector.call_static_flags[0]);
    }

    #[test]
    fn callcode_builtin_maps_message_fields() {
        let target = Address::from([0x22; 20]);
        let destination = Address::from([0x33; 20]);
        let caller = Address::from([0x11; 20]);
        let stack_value = Word::from(0x12);
        let mut host = base_evm(SpecId::CANCUN);
        host.set_inspector(MessageInspector::default());
        let tx_env = TxEnv::default();
        let message = Message {
            gas_limit: 1_000_000,
            destination,
            caller,
            value: Word::from(0x99),
            ..Message::default()
        };
        let mut interpreter = evm2::interpreter::Interpreter::<BaseEvmTypes>::new(
            Bytecode::new_legacy(Bytes::from_static(&[op::STOP])),
            &tx_env,
            &message,
        );

        {
            let mut frame = prepare_frame(&mut interpreter, &mut host);
            write_call_stack(frame.stack, target, 50_000, Some(stack_value));

            let sp = frame.stack.as_mut_ptr();
            unsafe { __revmc_builtin_call(&mut frame.ecx, sp, CallKind::CallCode) };

            assert_eq!(unsafe { frame.stack.get_unchecked(0) }.to_u256(), Word::from(1));
        }

        let inspector = host.clear_inspector_as::<MessageInspector>().unwrap();
        assert_eq!(inspector.calls.len(), 1);
        assert_eq!(inspector.calls[0].kind, MessageKind::CallCode);
        assert_eq!(inspector.calls[0].destination, destination);
        assert_eq!(inspector.calls[0].caller, destination);
        assert_eq!(inspector.calls[0].value, stack_value);
        assert_eq!(inspector.calls[0].code_address, target);
        assert!(!inspector.call_static_flags[0]);
    }

    #[test]
    fn delegatecall_builtin_maps_message_fields() {
        let target = Address::from([0x22; 20]);
        let destination = Address::from([0x33; 20]);
        let caller = Address::from([0x11; 20]);
        let current_value = Word::from(0x99);
        let mut host = base_evm(SpecId::CANCUN);
        host.set_inspector(MessageInspector::default());
        let tx_env = TxEnv::default();
        let message = Message {
            gas_limit: 1_000_000,
            destination,
            caller,
            value: current_value,
            ..Message::default()
        };
        let mut interpreter = evm2::interpreter::Interpreter::<BaseEvmTypes>::new(
            Bytecode::new_legacy(Bytes::from_static(&[op::STOP])),
            &tx_env,
            &message,
        );

        {
            let mut frame = prepare_frame(&mut interpreter, &mut host);
            write_call_stack(frame.stack, target, 50_000, None);

            let sp = frame.stack.as_mut_ptr();
            unsafe { __revmc_builtin_call(&mut frame.ecx, sp, CallKind::DelegateCall) };

            assert_eq!(unsafe { frame.stack.get_unchecked(0) }.to_u256(), Word::from(1));
        }

        let inspector = host.clear_inspector_as::<MessageInspector>().unwrap();
        assert_eq!(inspector.calls.len(), 1);
        assert_eq!(inspector.calls[0].kind, MessageKind::DelegateCall);
        assert_eq!(inspector.calls[0].destination, destination);
        assert_eq!(inspector.calls[0].caller, caller);
        assert_eq!(inspector.calls[0].value, current_value);
        assert_eq!(inspector.calls[0].code_address, target);
        assert!(!inspector.call_static_flags[0]);
    }

    #[test]
    fn staticcall_builtin_maps_message_fields() {
        let target = Address::from([0x22; 20]);
        let destination = Address::from([0x33; 20]);
        let caller = Address::from([0x11; 20]);
        let mut host = base_evm(SpecId::CANCUN);
        host.set_inspector(MessageInspector::default());
        let tx_env = TxEnv::default();
        let message = Message {
            gas_limit: 1_000_000,
            destination,
            caller,
            value: Word::from(0x99),
            ..Message::default()
        };
        let mut interpreter = evm2::interpreter::Interpreter::<BaseEvmTypes>::new(
            Bytecode::new_legacy(Bytes::from_static(&[op::STOP])),
            &tx_env,
            &message,
        );

        {
            let mut frame = prepare_frame(&mut interpreter, &mut host);
            write_call_stack(frame.stack, target, 50_000, None);

            let sp = frame.stack.as_mut_ptr();
            unsafe { __revmc_builtin_call(&mut frame.ecx, sp, CallKind::StaticCall) };

            assert_eq!(unsafe { frame.stack.get_unchecked(0) }.to_u256(), Word::from(1));
        }

        let inspector = host.clear_inspector_as::<MessageInspector>().unwrap();
        assert_eq!(inspector.calls.len(), 1);
        assert_eq!(inspector.calls[0].kind, MessageKind::StaticCall);
        assert_eq!(inspector.calls[0].destination, target);
        assert_eq!(inspector.calls[0].caller, destination);
        assert_eq!(inspector.calls[0].value, Word::ZERO);
        assert_eq!(inspector.calls[0].code_address, target);
        assert!(inspector.call_static_flags[0]);
    }

    #[test]
    fn create_builtin_executes_message() {
        let created = Address::from([0x77; 20]);
        let initcode = [op::STOP];
        let mut host = base_evm(SpecId::CANCUN);
        host.set_inspector(MessageInspector {
            execute_result: MessageResult {
                stop: InstrStop::Return,
                gas: GasTracker::new(11),
                created_address: Some(created),
                ..MessageResult::default()
            },
            ..MessageInspector::default()
        });
        let tx_env = TxEnv::default();
        let message = Message { gas_limit: 1_000_000, ..Message::default() };
        let mut interpreter = evm2::interpreter::Interpreter::<BaseEvmTypes>::new(
            Bytecode::new_legacy(Bytes::from_static(&[op::STOP])),
            &tx_env,
            &message,
        );

        {
            let mut frame = prepare_frame(&mut interpreter, &mut host);
            frame.ecx.memory_mut().resize(0, 1).unwrap();
            frame.ecx.memory_mut().set(0, &initcode);
            frame.stack.set(0, EvmWord::from(Word::from(initcode.len())));
            frame.stack.set(1, EvmWord::ZERO);
            frame.stack.set(2, EvmWord::ZERO);

            let sp = frame.stack.as_mut_ptr();
            unsafe { __revmc_builtin_create(&mut frame.ecx, sp, CreateKind::Create) };

            assert_eq!(unsafe { frame.stack.get_unchecked(0) }, &address_word(&created));
            assert!(frame.ecx.return_data().is_empty());
        }

        let inspector = host.clear_inspector_as::<MessageInspector>().unwrap();
        assert_eq!(inspector.creates.len(), 1);
        assert_eq!(inspector.creates[0].kind, MessageKind::Create);
        assert_eq!(inspector.creates[0].input.as_ref(), initcode);
    }

    #[test]
    fn create2_builtin_maps_salt() {
        let created = Address::from([0x77; 20]);
        let initcode = [op::STOP];
        let salt = EvmWord::from(Word::from(0xabcdu64));
        let mut host = base_evm(SpecId::CANCUN);
        host.set_inspector(MessageInspector {
            execute_result: MessageResult {
                stop: InstrStop::Return,
                gas: GasTracker::new(11),
                created_address: Some(created),
                ..MessageResult::default()
            },
            ..MessageInspector::default()
        });
        let tx_env = TxEnv::default();
        let message = Message { gas_limit: 1_000_000, ..Message::default() };
        let mut interpreter = evm2::interpreter::Interpreter::<BaseEvmTypes>::new(
            Bytecode::new_legacy(Bytes::from_static(&[op::STOP])),
            &tx_env,
            &message,
        );

        {
            let mut frame = prepare_frame(&mut interpreter, &mut host);
            frame.ecx.memory_mut().resize(0, 1).unwrap();
            frame.ecx.memory_mut().set(0, &initcode);
            frame.stack.set(0, salt);
            frame.stack.set(1, EvmWord::from(Word::from(initcode.len())));
            frame.stack.set(2, EvmWord::ZERO);
            frame.stack.set(3, EvmWord::ZERO);

            let sp = frame.stack.as_mut_ptr();
            unsafe { __revmc_builtin_create(&mut frame.ecx, sp, CreateKind::Create2) };

            assert_eq!(unsafe { frame.stack.get_unchecked(0) }, &address_word(&created));
            assert!(frame.ecx.return_data().is_empty());
        }

        let inspector = host.clear_inspector_as::<MessageInspector>().unwrap();
        assert_eq!(inspector.creates.len(), 1);
        assert_eq!(inspector.creates[0].kind, MessageKind::Create2);
        assert_eq!(inspector.creates[0].input.as_ref(), initcode);
        assert_eq!(inspector.creates[0].salt, B256::from(salt.to_be_bytes()));
    }
}
