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
use alloy_primitives::{B256, Bytes, KECCAK256_EMPTY, Log, LogData, U256, keccak256};
use evm2::{SpecId, interpreter::i256, version::GasId};
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
    unsafe { evm2_jit_context::revmc_exit(ecx) }
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
pub unsafe extern "C" fn __revmc_builtin_assert_spec_id(ecx: &EvmContext<'_>, expected: u8) {
    assert_eq!(
        ecx.spec_id, expected,
        "evm2_jit panic: runtime spec_id does not match compilation spec_id"
    );
}

#[inline]
fn spec_enabled(active: u8, required: SpecId) -> bool {
    let active = SpecId::try_from_u32(active.into()).expect("invalid evm2 spec id");
    active.enables(required)
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
    gas!(ecx, ecx.gas_params.exp_cost(exponent));
    *exponent_ptr = base.to_u256().pow(exponent).into();
    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_exp_gas(
    ecx: &mut EvmContext<'_>,
    exponent: &EvmWord,
) -> BuiltinResult {
    gas!(ecx, ecx.gas_params.exp_cost(exponent.to_u256()));
    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_keccak256(
    ecx: &mut EvmContext<'_>,
    sp: &mut [EvmWord; 2],
) -> BuiltinResult {
    let rev![offset, len_ptr] = sp;
    let len = try_into_usize!(len_ptr);
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
        gas!(ecx, ecx.gas_params.keccak256_word_cost(len));
        let offset = try_into_usize!(offset);
        ensure_memory(ecx, offset, len)?;
        keccak256(ecx.memory.slice(offset, len))
    });
    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_balance(
    ecx: &mut EvmContext<'_>,
    address: &mut EvmWord,
) -> BuiltinResult {
    let addr = address.to_address();
    let account = load_account(ecx, addr, false)?;
    *address = account.balance.into();
    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_origin(ecx: &EvmContext<'_>, slot: &mut EvmWord) {
    *slot = EvmWord::from_be_bytes(ecx.host.caller().into_word());
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_calldataload(
    ecx: &EvmContext<'_>,
    offset_ptr: &mut EvmWord,
) {
    do_calldataload(ecx, offset_ptr, as_usize_saturated!(offset_ptr.to_u256()));
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
    let input = ecx.input.input();
    let input_len = input.len();
    if offset < input_len {
        let count = 32.min(input_len - offset);
        let input = ecx.input.input().as_bytes();
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
    let len = try_into_usize!(len);
    if len != 0 {
        gas!(ecx, ecx.gas_params.copy_cost(len));
        let memory_offset = try_into_usize!(memory_offset);
        ensure_memory(ecx, memory_offset, len)?;
        let data_offset = as_usize_saturated!(data_offset.to_u256());
        ecx.memory.set_data(memory_offset, data_offset, len, ecx.input.input().as_bytes());
    }
    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_codecopy(
    ecx: &mut EvmContext<'_>,
    sp: &mut [EvmWord; 3],
) -> BuiltinResult {
    let bytecode = unsafe { &*ecx.bytecode };
    copy_operation(ecx, sp, bytecode)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_gas_price(ecx: &EvmContext<'_>, slot: &mut EvmWord) {
    *slot = ecx.host.effective_gas_price().into();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_extcodesize(
    ecx: &mut EvmContext<'_>,
    address: &mut EvmWord,
) -> BuiltinResult {
    let addr = address.to_address();
    let account = load_account(ecx, addr, true)?;
    *address = U256::from(account.code.len()).into();
    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_extcodecopy(
    ecx: &mut EvmContext<'_>,
    sp: &mut [EvmWord; 4],
) -> BuiltinResult {
    let rev![address, memory_offset, code_offset, len] = sp;
    let addr = address.to_address();
    let len = try_into_usize!(len);
    gas!(ecx, ecx.gas_params.extcodecopy_cost(len));

    let mut memory_offset_usize = 0;
    if len != 0 {
        memory_offset_usize = try_into_usize!(memory_offset);
        ensure_memory(ecx, memory_offset_usize, len)?;
    }

    let account = load_account(ecx, addr, true)?;
    let code = account.code.original_bytes();

    let code_offset_usize = core::cmp::min(as_usize_saturated!(code_offset.to_u256()), code.len());
    ecx.memory.set_data(memory_offset_usize, code_offset_usize, len, &code);
    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_returndatacopy(
    ecx: &mut EvmContext<'_>,
    sp: &mut [EvmWord; 3],
) -> BuiltinResult {
    let rev![memory_offset, offset, len] = sp;
    let len = try_into_usize!(len);
    let data_offset = as_usize_saturated!(offset.to_u256());

    // Bounds check BEFORE charging gas, matching revm.
    let data_end = data_offset.saturating_add(len);
    if data_end > ecx.return_data.len() {
        return Err(InstrStop::OutOfOffset.into());
    }

    gas!(ecx, ecx.gas_params.copy_cost(len));
    if len != 0 {
        let memory_offset = try_into_usize!(memory_offset);
        ensure_memory(ecx, memory_offset, len)?;
        ecx.memory.set(memory_offset, &ecx.return_data[data_offset..data_end]);
    }
    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_extcodehash(
    ecx: &mut EvmContext<'_>,
    address: &mut EvmWord,
) -> BuiltinResult {
    let addr = address.to_address();
    let account = load_account(ecx, addr, false)?;
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
    let block_number = ecx.host.block_number();

    // Check if requested block is in the future
    let Some(diff) = block_number.checked_sub(requested_number) else {
        *number_ptr = EvmWord::ZERO;
        return Ok(());
    };

    let diff = as_u64_saturated!(diff);

    // Current block returns 0
    if diff == 0 {
        *number_ptr = EvmWord::ZERO;
        return Ok(());
    }

    // BLOCK_HASH_HISTORY is 256
    const BLOCK_HASH_HISTORY: u64 = 256;

    if diff <= BLOCK_HASH_HISTORY {
        let hash = ecx.host.block_hash(as_u64_saturated!(requested_number)).ok_or_fatal()?;
        *number_ptr = EvmWord::from_be_bytes(hash);
    } else {
        // Too old, return 0
        *number_ptr = EvmWord::ZERO;
    }

    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_coinbase(ecx: &EvmContext<'_>, slot: &mut EvmWord) {
    *slot = EvmWord::from_be_bytes(ecx.host.beneficiary().into_word());
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_timestamp(ecx: &EvmContext<'_>, slot: &mut EvmWord) {
    *slot = ecx.host.timestamp().into();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_number(ecx: &EvmContext<'_>, slot: &mut EvmWord) {
    *slot = ecx.host.block_number().into();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_difficulty(ecx: &EvmContext<'_>, slot: &mut EvmWord) {
    *slot = if spec_enabled(ecx.spec_id, SpecId::MERGE) {
        ecx.host.prevrandao().unwrap().into()
    } else {
        ecx.host.difficulty().into()
    };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_gaslimit(ecx: &EvmContext<'_>, slot: &mut EvmWord) {
    *slot = ecx.host.gas_limit().into();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_chainid(ecx: &EvmContext<'_>, slot: &mut EvmWord) {
    *slot = ecx.host.chain_id().into();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_self_balance(
    ecx: &mut EvmContext<'_>,
    slot: &mut EvmWord,
) -> BuiltinResult {
    let state = ecx.host.balance(ecx.input.target_address).ok_or_fatal()?;
    *slot = state.data.into();
    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_basefee(ecx: &EvmContext<'_>, slot: &mut EvmWord) {
    *slot = ecx.host.basefee().into();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_blob_hash(ecx: &EvmContext<'_>, index_ptr: &mut EvmWord) {
    let index = index_ptr.to_u256();
    let index_usize = as_usize_saturated!(index);
    *index_ptr = ecx.host.blob_hash(index_usize).unwrap_or_default().into();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_blob_base_fee(ecx: &EvmContext<'_>, slot: &mut EvmWord) {
    *slot = ecx.host.blob_gasprice().into();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_mresize(ecx: &mut EvmContext<'_>, min_size: u64) -> BuiltinResult {
    ensure_memory(ecx, min_size as usize, 0)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_slot_num(ecx: &EvmContext<'_>, slot: &mut EvmWord) {
    *slot = ecx.host.slot_num().into();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_sload(
    ecx: &mut EvmContext<'_>,
    index: &mut EvmWord,
) -> BuiltinResult {
    let address = ecx.input.target_address;
    let key = index.to_u256();
    if spec_enabled(ecx.spec_id, SpecId::BERLIN) {
        let additional_cold_cost = u64::from(ecx.gas_params.get(GasId::ColdStorageAdditionalCost));
        let skip_cold = ecx.gas.remaining() < additional_cold_cost;
        let storage = ecx.host.sload_skip_cold_load(address, key, skip_cold)?;
        if storage.is_cold {
            gas!(ecx, additional_cold_cost);
        }
        *index = storage.data.into();
    } else {
        let storage = ecx.host.sload(address, key).ok_or_fatal()?;
        *index = storage.data.into();
    }

    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_sload_c(
    ecx: &mut EvmContext<'_>,
    index: &mut EvmWord,
    key: u64,
) -> BuiltinResult {
    *index = U256::from(key).into();
    __revmc_builtin_sload(ecx, index);
    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_sstore(
    ecx: &mut EvmContext<'_>,
    sp: &mut [EvmWord; 2],
) -> BuiltinResult {
    let rev![index, value] = sp;
    ensure_non_staticcall!(ecx);

    let target = ecx.input.target_address;
    let is_istanbul = spec_enabled(ecx.spec_id, SpecId::ISTANBUL);

    // EIP-2200: If gasleft is less than or equal to gas stipend, fail with OOG.
    if is_istanbul && ecx.gas.remaining() <= u64::from(ecx.gas_params.get(GasId::CallStipend)) {
        return Err(InstrStop::ReentrancySentryOOG.into());
    }

    gas!(ecx, u64::from(ecx.gas_params.get(GasId::SstoreStatic)));

    let state_load = if spec_enabled(ecx.spec_id, SpecId::BERLIN) {
        let additional_cold_cost = u64::from(ecx.gas_params.get(GasId::ColdStorageAdditionalCost));
        let skip_cold = ecx.gas.remaining() < additional_cold_cost;
        ecx.host.sstore_skip_cold_load(target, index.to_u256(), value.to_u256(), skip_cold)?
    } else {
        ecx.host.sstore(target, index.to_u256(), value.to_u256()).ok_or_fatal()?
    };

    let gp = &ecx.gas_params;
    gas!(ecx, gp.sstore_dynamic_gas(is_istanbul, &state_load.data));

    // State gas for new slot creation (EIP-8037).
    if ecx.host.is_amsterdam_eip8037_enabled() {
        state_gas!(ecx, gp.sstore_state_gas(&state_load.data));
    }

    ecx.gas.record_refund(gp.sstore_refund(is_istanbul, &state_load.data));
    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_tload(ecx: &mut EvmContext<'_>, key: &mut EvmWord) {
    *key = ecx.host.tload(ecx.input.target_address, key.to_u256()).into();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_tstore(
    ecx: &mut EvmContext<'_>,
    sp: &mut [EvmWord; 2],
) -> BuiltinResult {
    let rev![key, value] = sp;
    ensure_non_staticcall!(ecx);
    ecx.host.tstore(ecx.input.target_address, key.to_u256(), value.to_u256());
    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_mcopy(
    ecx: &mut EvmContext<'_>,
    sp: &mut [EvmWord; 3],
) -> BuiltinResult {
    let rev![dst, src, len] = sp;
    let len = try_into_usize!(len);
    gas!(ecx, ecx.gas_params.mcopy_cost(len));
    if len != 0 {
        let dst = try_into_usize!(dst);
        let src = try_into_usize!(src);
        ensure_memory(ecx, dst.max(src), len)?;
        ecx.memory.copy(dst, src, len);
    }
    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_log(
    ecx: &mut EvmContext<'_>,
    sp: *mut EvmWord,
    n: u8,
) -> BuiltinResult {
    ensure_non_staticcall!(ecx);
    assume!(n <= 4, "invalid log topic count: {n}");
    let sp = sp.add(n as usize);
    read_words!(sp, offset, len);
    let len = try_into_usize!(len);
    gas!(ecx, ecx.gas_params.log_cost(n, len));
    let data = if len != 0 {
        let offset = try_into_usize!(offset);
        ensure_memory(ecx, offset, len)?;
        Bytes::copy_from_slice(ecx.memory.slice(offset, len))
    } else {
        Bytes::new()
    };

    let mut topics = Vec::with_capacity(n as usize);
    for i in 1..=n {
        topics.push(sp.sub(i as usize).read().to_be_bytes());
    }

    let log = Log {
        address: ecx.input.target_address,
        data: LogData::new(topics, data).expect("too many topics"),
    };
    if let Some(on_log) = &mut ecx.on_log {
        ecx.host.log(log.clone());
        on_log(&log);
    } else {
        ecx.host.log(log);
    }
    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_create(
    ecx: &mut EvmContext<'_>,
    sp: *mut EvmWord,
    create_kind: CreateKind,
) -> BuiltinResult {
    unsafe { (ecx.evm2_recursion.create)(ecx, sp, create_kind as u8) }?;
    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_call(
    ecx: &mut EvmContext<'_>,
    sp: *mut EvmWord,
    call_kind: CallKind,
) -> BuiltinResult {
    unsafe { (ecx.evm2_recursion.call)(ecx, sp, call_kind as u8) }?;
    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_do_return(
    ecx: &mut EvmContext<'_>,
    sp: &mut [EvmWord; 2],
    result: InstrStop,
) -> BuiltinResult {
    let rev![offset, len] = sp;
    let len = try_into_usize!(len);
    let output = if len != 0 {
        let offset = try_into_usize!(offset);
        ensure_memory(ecx, offset, len)?;
        ecx.memory.slice(offset, len).to_vec().into()
    } else {
        Bytes::new()
    };
    ecx.output = output;
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
        ensure_memory(ecx, offset, len)?;
        ecx.memory.slice(offset, len).to_vec().into()
    } else {
        Bytes::new()
    };
    ecx.output = output;
    Err(result.into())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __revmc_builtin_selfdestruct(
    ecx: &mut EvmContext<'_>,
    target: &mut EvmWord,
) -> BuiltinResult {
    ensure_non_staticcall!(ecx);

    let cold_load_gas = ecx.gas_params.selfdestruct_cold_cost();
    let skip_cold_load = ecx.gas.remaining() < cold_load_gas;
    let res =
        ecx.host.selfdestruct(ecx.input.target_address, target.to_address(), skip_cold_load)?;

    // EIP-161: State trie clearing (invariant-preserving alternative)
    let should_charge_topup = if spec_enabled(ecx.spec_id, SpecId::SPURIOUS_DRAGON) {
        res.had_value && res.target_is_empty
    } else {
        res.target_is_empty
    };

    gas!(ecx, ecx.gas_params.selfdestruct_cost(should_charge_topup, res.is_cold));

    // State gas for new account creation (EIP-8037).
    if ecx.host.is_amsterdam_eip8037_enabled() && should_charge_topup {
        state_gas!(ecx, u64::from(ecx.gas_params.get(GasId::NewAccountState)));
    }

    if !res.previously_destroyed {
        ecx.gas.record_refund(i64::from(ecx.gas_params.get(GasId::SelfdestructRefund)));
    }

    Err(InstrStop::SelfDestruct.into())
}

}
