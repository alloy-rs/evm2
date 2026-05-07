//! EVMC C API adapter for evm2.

#![allow(missing_copy_implementations, missing_debug_implementations, missing_docs)]

use alloy_primitives::{Address, B256, Bytes, Log, U256};
use evm2::{
    BaseEvmConfig, BaseEvmConfigSelector, EvmTypes, SpecId,
    bytecode::Bytecode,
    env::{BlockEnv, TxEnv},
    evm::{AccountLoad, InMemoryDB, SLoad, SStore, SelfDestructResult, precompile::NoPrecompiles},
    interpreter::{Host, InstrStop, Interpreter, Message, MessageKind, MessageResult, Word},
    spec_to_generic,
};
use std::{
    ffi::CStr,
    panic::{AssertUnwindSafe, catch_unwind},
    ptr, slice,
};

#[allow(
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    improper_ctypes,
    missing_copy_implementations,
    missing_debug_implementations,
    missing_docs,
    unreachable_pub,
    clippy::use_self
)]
pub mod ffi {
    include!(concat!(env!("OUT_DIR"), "/evmc.rs"));
}

use ffi::{
    evmc_access_status::*, evmc_call_kind::*, evmc_capabilities::*, evmc_flags::*,
    evmc_revision::*, evmc_status_code::*, evmc_storage_status::*, *,
};

const NAME: &CStr = c"evm2";
const VERSION: &CStr = c"0.1.0";

#[derive(Clone, Copy)]
struct EvmcTypes;

impl EvmTypes for EvmcTypes {
    type ConfigSelector = BaseEvmConfigSelector;
    type SpecId = SpecId;
    type Tx = ();
    type Host = EvmcHost;
    type Database = InMemoryDB;
    type Precompiles = NoPrecompiles;
}

struct EvmcHost {
    spec_id: SpecId,
    interface: *const evmc_host_interface,
    context: *mut evmc_host_context,
    tx_context: evmc_tx_context,
    block: BlockEnv,
}

impl Host for EvmcHost {
    fn spec_id(&self) -> SpecId {
        self.spec_id
    }

    fn block_env(&mut self) -> &BlockEnv {
        &self.block
    }

    fn load_account(
        &mut self,
        address: Address,
        load_code: bool,
        skip_cold_load: bool,
    ) -> Result<AccountLoad, InstrStop> {
        let evmc_address = address_to_evmc(address);
        let is_cold = self.access_account(&evmc_address) == EVMC_ACCESS_COLD;
        if skip_cold_load && is_cold {
            return Err(InstrStop::OutOfGas);
        }

        let exists = self.account_exists(&evmc_address);
        let balance = self.balance(&evmc_address);
        let code_hash = self.code_hash(&evmc_address);
        let code_size = self.code_size(&evmc_address);
        let code = if load_code && code_size != 0 {
            self.copy_code(&evmc_address, code_size)
        } else {
            Bytes::new()
        };

        Ok(AccountLoad {
            balance,
            code_hash,
            code,
            exists,
            is_empty: !exists || (balance.is_zero() && code_size == 0),
            is_cold,
        })
    }

    fn target_is_empty_for_new_account_gas(&mut self, address: Address, _spec: SpecId) -> bool {
        let evmc_address = address_to_evmc(address);
        !self.account_exists(&evmc_address)
            || (self.balance(&evmc_address).is_zero() && self.code_size(&evmc_address) == 0)
    }

    fn block_hash(&mut self, number: Word) -> Option<B256> {
        let number = i64::try_from(number).ok()?;
        let get_block_hash = self.host().get_block_hash?;
        let hash = unsafe { get_block_hash(self.context, number) };
        (!hash.bytes.iter().all(|byte| *byte == 0)).then(|| B256::from(hash.bytes))
    }

    fn sload(
        &mut self,
        address: Address,
        key: Word,
        skip_cold_load: bool,
    ) -> Result<SLoad, InstrStop> {
        let address = address_to_evmc(address);
        let key = word_to_bytes32(key);
        let is_cold = self.access_storage(&address, &key) == EVMC_ACCESS_COLD;
        if skip_cold_load && is_cold {
            return Err(InstrStop::OutOfGas);
        }
        let Some(get_storage) = self.host().get_storage else {
            return Ok(SLoad { value: Word::ZERO, is_cold });
        };
        Ok(SLoad {
            value: bytes32_to_word(unsafe { get_storage(self.context, &address, &key) }),
            is_cold,
        })
    }

    fn sstore(
        &mut self,
        address: Address,
        key: Word,
        value: Word,
        skip_cold_load: bool,
    ) -> Result<SStore, InstrStop> {
        let address = address_to_evmc(address);
        let key = word_to_bytes32(key);
        let value = word_to_bytes32(value);
        let is_cold = self.access_storage(&address, &key) == EVMC_ACCESS_COLD;
        if skip_cold_load && is_cold {
            return Err(InstrStop::OutOfGas);
        }
        let Some(set_storage) = self.host().set_storage else {
            return Ok(SStore { is_cold, ..SStore::default() });
        };
        let status = unsafe { set_storage(self.context, &address, &key, &value) };
        Ok(storage_status_to_sstore(status, is_cold))
    }

    fn tload(&mut self, address: Address, key: Word) -> Word {
        let Some(get_transient_storage) = self.host().get_transient_storage else {
            return Word::ZERO;
        };
        let address = address_to_evmc(address);
        let key = word_to_bytes32(key);
        bytes32_to_word(unsafe { get_transient_storage(self.context, &address, &key) })
    }

    fn tstore(&mut self, address: Address, key: Word, value: Word) {
        let Some(set_transient_storage) = self.host().set_transient_storage else {
            return;
        };
        let address = address_to_evmc(address);
        let key = word_to_bytes32(key);
        let value = word_to_bytes32(value);
        unsafe {
            set_transient_storage(self.context, &address, &key, &value);
        }
    }

    fn log(&mut self, log: Log) {
        let Some(emit_log) = self.host().emit_log else {
            return;
        };
        let address = address_to_evmc(log.address);
        let topics = log.data.topics().iter().copied().map(b256_to_bytes32).collect::<Vec<_>>();
        unsafe {
            emit_log(
                self.context,
                &address,
                log.data.data.as_ptr(),
                log.data.data.len(),
                topics.as_ptr(),
                topics.len(),
            );
        }
    }

    fn execute_message(
        &mut self,
        _tx_env: &TxEnv,
        bytecode: Bytecode,
        message: &Message,
        caller_is_static: bool,
    ) -> MessageResult {
        let Some(call) = self.host().call else {
            return MessageResult {
                stop: InstrStop::FatalExternalError,
                ..MessageResult::default()
            };
        };
        let evmc_message =
            message_to_evmc(message, bytecode.original_byte_slice(), caller_is_static);
        result_to_message(unsafe { call(self.context, &evmc_message) })
    }

    fn selfdestruct(
        &mut self,
        contract: Address,
        target: Address,
        skip_cold_load: bool,
    ) -> Result<SelfDestructResult, InstrStop> {
        let target = address_to_evmc(target);
        let is_cold = self.access_account(&target) == EVMC_ACCESS_COLD;
        if skip_cold_load && is_cold {
            return Err(InstrStop::OutOfGas);
        }
        let Some(selfdestruct) = self.host().selfdestruct else {
            return Ok(SelfDestructResult { is_cold, ..SelfDestructResult::default() });
        };
        let contract = address_to_evmc(contract);
        let previously_destroyed = !unsafe { selfdestruct(self.context, &contract, &target) };
        Ok(SelfDestructResult {
            is_cold,
            target_is_empty: false,
            previously_destroyed,
            had_value: false,
        })
    }
}

impl EvmcHost {
    fn new(
        spec_id: SpecId,
        interface: *const evmc_host_interface,
        context: *mut evmc_host_context,
    ) -> Option<Self> {
        if interface.is_null() {
            return None;
        }
        let tx_context = {
            let host = unsafe { &*interface };
            host.get_tx_context
                .map_or_else(zero_tx_context, |get_tx_context| unsafe { get_tx_context(context) })
        };
        let block = block_env(&tx_context);
        Some(Self { spec_id, interface, context, tx_context, block })
    }

    const fn host(&self) -> &evmc_host_interface {
        unsafe { &*self.interface }
    }

    fn account_exists(&mut self, address: &evmc_address) -> bool {
        self.host()
            .account_exists
            .is_some_and(|account_exists| unsafe { account_exists(self.context, address) })
    }

    fn access_account(&mut self, address: &evmc_address) -> evmc_access_status {
        self.host().access_account.map_or(EVMC_ACCESS_WARM, |access_account| unsafe {
            access_account(self.context, address)
        })
    }

    fn access_storage(&mut self, address: &evmc_address, key: &evmc_bytes32) -> evmc_access_status {
        self.host().access_storage.map_or(EVMC_ACCESS_WARM, |access_storage| unsafe {
            access_storage(self.context, address, key)
        })
    }

    fn balance(&mut self, address: &evmc_address) -> Word {
        self.host().get_balance.map_or(Word::ZERO, |get_balance| {
            bytes32_to_word(unsafe { get_balance(self.context, address) })
        })
    }

    fn code_hash(&mut self, address: &evmc_address) -> B256 {
        self.host().get_code_hash.map_or(B256::ZERO, |get_code_hash| {
            B256::from(unsafe { get_code_hash(self.context, address) }.bytes)
        })
    }

    fn code_size(&mut self, address: &evmc_address) -> usize {
        self.host()
            .get_code_size
            .map_or(0, |get_code_size| unsafe { get_code_size(self.context, address) })
    }

    fn copy_code(&mut self, address: &evmc_address, code_size: usize) -> Bytes {
        let Some(copy_code) = self.host().copy_code else {
            return Bytes::new();
        };
        let mut code = vec![0; code_size];
        let copied = unsafe { copy_code(self.context, address, 0, code.as_mut_ptr(), code.len()) };
        code.truncate(copied);
        code.into()
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn evmc_create_evm2() -> *mut evmc_vm {
    let vm = Box::new(evmc_vm {
        abi_version: EVMC_ABI_VERSION.try_into().expect("EVMC ABI version fits in c_int"),
        name: NAME.as_ptr(),
        version: VERSION.as_ptr(),
        destroy: Some(destroy),
        execute: Some(execute),
        get_capabilities: Some(get_capabilities),
        set_option: None,
    });
    Box::into_raw(vm)
}

unsafe extern "C" fn destroy(vm: *mut evmc_vm) {
    if !vm.is_null() {
        unsafe {
            drop(Box::from_raw(vm));
        }
    }
}

const unsafe extern "C" fn get_capabilities(_vm: *mut evmc_vm) -> evmc_capabilities_flagset {
    EVMC_CAPABILITY_EVM1 as evmc_capabilities_flagset
}

unsafe extern "C" fn execute(
    _vm: *mut evmc_vm,
    host: *const evmc_host_interface,
    context: *mut evmc_host_context,
    rev: evmc_revision,
    msg: *const evmc_message,
    code: *const u8,
    code_size: usize,
) -> evmc_result {
    catch_unwind(AssertUnwindSafe(|| execute_inner(host, context, rev, msg, code, code_size)))
        .unwrap_or_else(|_| failure_result(EVMC_INTERNAL_ERROR))
}

fn execute_inner(
    host: *const evmc_host_interface,
    context: *mut evmc_host_context,
    rev: evmc_revision,
    msg: *const evmc_message,
    code: *const u8,
    code_size: usize,
) -> evmc_result {
    let Some(spec_id) = revision_to_spec_id(rev) else {
        return failure_result(EVMC_REJECTED);
    };
    let Some(msg) = (!msg.is_null()).then(|| unsafe { &*msg }) else {
        return failure_result(EVMC_REJECTED);
    };
    let Some(mut host) = EvmcHost::new(spec_id, host, context) else {
        return failure_result(EVMC_REJECTED);
    };
    let Some(code) = bytes_from_raw(code, code_size) else {
        return failure_result(EVMC_REJECTED);
    };
    if msg.gas < 0 || msg.depth < 0 {
        return failure_result(EVMC_REJECTED);
    }

    let tx_env = tx_env(&host.tx_context);
    let message = message_from_evmc(msg);
    let bytecode = Bytecode::new_legacy(Bytes::copy_from_slice(code));
    let mut interpreter = Interpreter::<EvmcTypes>::new(bytecode, &tx_env, &message, false);
    let stop = spec_to_generic!(spec_id, |BASE_SPEC_ID| {
        interpreter.run::<BaseEvmConfig<BASE_SPEC_ID>>(&mut host)
    });
    interpreter_result(stop, &interpreter)
}

fn message_from_evmc(msg: &evmc_message) -> Message {
    Message {
        kind: match msg.kind {
            EVMC_DELEGATECALL => MessageKind::DelegateCall,
            EVMC_CALLCODE => MessageKind::CallCode,
            EVMC_CREATE => MessageKind::Create,
            EVMC_CREATE2 => MessageKind::Create2,
            _ if msg.flags & EVMC_STATIC as u32 != 0 => MessageKind::StaticCall,
            _ => MessageKind::Call,
        },
        depth: msg.depth.try_into().unwrap_or_default(),
        gas_limit: msg.gas.try_into().unwrap_or_default(),
        destination: address_from_evmc(msg.recipient),
        caller: address_from_evmc(msg.sender),
        input: bytes_from_raw(msg.input_data, msg.input_size)
            .map(Bytes::copy_from_slice)
            .unwrap_or_default(),
        value: bytes32_to_word(msg.value),
        code_address: address_from_evmc(msg.code_address),
        disable_precompiles: false,
        salt: B256::from(msg.create2_salt.bytes),
    }
}

fn message_to_evmc(message: &Message, code: &[u8], caller_is_static: bool) -> evmc_message {
    evmc_message {
        kind: match message.kind {
            MessageKind::DelegateCall => EVMC_DELEGATECALL,
            MessageKind::CallCode => EVMC_CALLCODE,
            MessageKind::Create => EVMC_CREATE,
            MessageKind::Create2 => EVMC_CREATE2,
            MessageKind::Call | MessageKind::StaticCall => EVMC_CALL,
            _ => EVMC_CALL,
        },
        flags: (u32::from(caller_is_static || matches!(message.kind, MessageKind::StaticCall))
            * EVMC_STATIC as u32)
            | (u32::from(message.disable_precompiles) * EVMC_DELEGATED as u32),
        depth: message.depth.into(),
        gas: i64::try_from(message.gas_limit).unwrap_or(i64::MAX),
        recipient: address_to_evmc(message.destination),
        sender: address_to_evmc(message.caller),
        input_data: message.input.as_ptr(),
        input_size: message.input.len(),
        value: word_to_bytes32(message.value),
        create2_salt: b256_to_bytes32(message.salt),
        code_address: address_to_evmc(message.code_address),
        code: code.as_ptr(),
        code_size: code.len(),
    }
}

fn tx_env(tx_context: &evmc_tx_context) -> TxEnv {
    TxEnv {
        origin: address_from_evmc(tx_context.tx_origin),
        gas_price: bytes32_to_word(tx_context.tx_gas_price),
        chain_id: bytes32_to_word(tx_context.chain_id),
        blob_hashes: evmc_blob_hashes(tx_context)
            .iter()
            .map(|hash| U256::from_be_bytes(hash.bytes))
            .collect(),
    }
}

fn block_env(tx_context: &evmc_tx_context) -> BlockEnv {
    BlockEnv {
        number: signed_to_u256(tx_context.block_number),
        beneficiary: address_from_evmc(tx_context.block_coinbase),
        timestamp: signed_to_u256(tx_context.block_timestamp),
        gas_limit: signed_to_u256(tx_context.block_gas_limit),
        basefee: bytes32_to_word(tx_context.block_base_fee),
        difficulty: U256::ZERO,
        prevrandao: bytes32_to_word(tx_context.block_prev_randao),
        blob_basefee: bytes32_to_word(tx_context.blob_base_fee),
        slot_num: U256::from(tx_context.block_slot_number),
    }
}

fn interpreter_result(stop: InstrStop, interpreter: &Interpreter<'_, EvmcTypes>) -> evmc_result {
    let status_code = status_code(stop);
    let output = if stop.is_success() || stop.is_revert() { interpreter.output() } else { &[] };
    let mut result = result_with_output(status_code, output);
    if stop.is_success() || stop.is_revert() {
        result.gas_left = i64::try_from(interpreter.gas().remaining()).unwrap_or(i64::MAX);
    }
    if stop.is_success() {
        result.gas_refund = interpreter.gas().refunded();
    }
    result
}

fn result_to_message(result: evmc_result) -> MessageResult {
    let stop = match result.status_code {
        EVMC_SUCCESS => InstrStop::Return,
        EVMC_REVERT => InstrStop::Revert,
        EVMC_OUT_OF_GAS => InstrStop::OutOfGas,
        EVMC_INVALID_INSTRUCTION => InstrStop::InvalidFEOpcode,
        EVMC_UNDEFINED_INSTRUCTION => InstrStop::OpcodeNotFound,
        EVMC_STACK_OVERFLOW => InstrStop::StackOverflow,
        EVMC_STACK_UNDERFLOW => InstrStop::StackUnderflow,
        EVMC_BAD_JUMP_DESTINATION => InstrStop::InvalidJump,
        EVMC_INVALID_MEMORY_ACCESS | EVMC_ARGUMENT_OUT_OF_RANGE => InstrStop::OutOfOffset,
        EVMC_CALL_DEPTH_EXCEEDED => InstrStop::CallTooDeep,
        EVMC_STATIC_MODE_VIOLATION => InstrStop::StateChangeDuringStaticCall,
        EVMC_PRECOMPILE_FAILURE => InstrStop::PrecompileError,
        EVMC_INSUFFICIENT_BALANCE => InstrStop::OutOfFunds,
        _ => InstrStop::FatalExternalError,
    };
    let output = bytes_from_raw(result.output_data, result.output_size)
        .map(Bytes::copy_from_slice)
        .unwrap_or_default();
    if let Some(release) = result.release {
        unsafe {
            release(&result);
        }
    }
    MessageResult {
        stop,
        gas_remaining: result.gas_left.try_into().unwrap_or_default(),
        gas_refunded: result.gas_refund,
        output,
        created_address: (result.status_code == EVMC_SUCCESS)
            .then(|| address_from_evmc(result.create_address)),
    }
}

fn result_with_output(status_code: evmc_status_code, output: &[u8]) -> evmc_result {
    if output.is_empty() {
        return evmc_result {
            status_code,
            gas_left: 0,
            gas_refund: 0,
            output_data: ptr::null(),
            output_size: 0,
            release: None,
            create_address: zero_address(),
            padding: [0; 4],
        };
    }

    let output = output.to_vec().into_boxed_slice();
    let output_data = output.as_ptr();
    let output_size = output.len();
    Box::leak(output);
    evmc_result {
        status_code,
        gas_left: 0,
        gas_refund: 0,
        output_data,
        output_size,
        release: Some(release_result),
        create_address: zero_address(),
        padding: [0; 4],
    }
}

fn failure_result(status_code: evmc_status_code) -> evmc_result {
    evmc_result {
        status_code,
        gas_left: 0,
        gas_refund: 0,
        output_data: ptr::null(),
        output_size: 0,
        release: None,
        create_address: zero_address(),
        padding: [0; 4],
    }
}

unsafe extern "C" fn release_result(result: *const evmc_result) {
    if result.is_null() {
        return;
    }
    let result = unsafe { &*result };
    if !result.output_data.is_null() && result.output_size != 0 {
        let data = ptr::slice_from_raw_parts_mut(result.output_data.cast_mut(), result.output_size);
        unsafe {
            drop(Box::from_raw(data));
        }
    }
}

const fn status_code(stop: InstrStop) -> evmc_status_code {
    match stop {
        InstrStop::Stop | InstrStop::Return | InstrStop::SelfDestruct => EVMC_SUCCESS,
        InstrStop::Revert
        | InstrStop::CreateInitCodeStartingEF00
        | InstrStop::InvalidEOFInitCode
        | InstrStop::InvalidExtDelegateCallTarget => EVMC_REVERT,
        InstrStop::OutOfGas
        | InstrStop::MemoryOOG
        | InstrStop::MemoryLimitOOG
        | InstrStop::InvalidOperandOOG
        | InstrStop::ReentrancySentryOOG => EVMC_OUT_OF_GAS,
        InstrStop::InvalidFEOpcode => EVMC_INVALID_INSTRUCTION,
        InstrStop::OpcodeNotFound
        | InstrStop::NotActivated
        | InstrStop::InvalidImmediateEncoding => EVMC_UNDEFINED_INSTRUCTION,
        InstrStop::StackOverflow => EVMC_STACK_OVERFLOW,
        InstrStop::StackUnderflow => EVMC_STACK_UNDERFLOW,
        InstrStop::InvalidJump => EVMC_BAD_JUMP_DESTINATION,
        InstrStop::OutOfOffset => EVMC_INVALID_MEMORY_ACCESS,
        InstrStop::CallTooDeep => EVMC_CALL_DEPTH_EXCEEDED,
        InstrStop::CallNotAllowedInsideStatic | InstrStop::StateChangeDuringStaticCall => {
            EVMC_STATIC_MODE_VIOLATION
        }
        InstrStop::PrecompileOOG => EVMC_OUT_OF_GAS,
        InstrStop::PrecompileError => EVMC_PRECOMPILE_FAILURE,
        InstrStop::OutOfFunds => EVMC_INSUFFICIENT_BALANCE,
        InstrStop::CreateCollision
        | InstrStop::OverflowPayment
        | InstrStop::NonceOverflow
        | InstrStop::CreateContractSizeLimit
        | InstrStop::CreateContractStartingWithEF
        | InstrStop::CreateInitCodeSizeLimit
        | InstrStop::FatalExternalError => EVMC_FAILURE,
        _ => EVMC_FAILURE,
    }
}

const fn revision_to_spec_id(rev: evmc_revision) -> Option<SpecId> {
    match rev {
        EVMC_FRONTIER => Some(SpecId::FRONTIER),
        EVMC_HOMESTEAD => Some(SpecId::HOMESTEAD),
        EVMC_TANGERINE_WHISTLE => Some(SpecId::TANGERINE),
        EVMC_SPURIOUS_DRAGON => Some(SpecId::SPURIOUS_DRAGON),
        EVMC_BYZANTIUM => Some(SpecId::BYZANTIUM),
        EVMC_CONSTANTINOPLE | EVMC_PETERSBURG => Some(SpecId::PETERSBURG),
        EVMC_ISTANBUL => Some(SpecId::ISTANBUL),
        EVMC_BERLIN => Some(SpecId::BERLIN),
        EVMC_LONDON => Some(SpecId::LONDON),
        EVMC_PARIS => Some(SpecId::MERGE),
        EVMC_SHANGHAI => Some(SpecId::SHANGHAI),
        EVMC_CANCUN => Some(SpecId::CANCUN),
        EVMC_PRAGUE => Some(SpecId::PRAGUE),
        EVMC_OSAKA => Some(SpecId::OSAKA),
        EVMC_AMSTERDAM => Some(SpecId::AMSTERDAM),
        _ => None,
    }
}

fn storage_status_to_sstore(status: evmc_storage_status, is_cold: bool) -> SStore {
    let (original_value, present_value, new_value) = match status {
        EVMC_STORAGE_ADDED => (Word::ZERO, Word::ZERO, Word::from(1)),
        EVMC_STORAGE_DELETED => (Word::from(1), Word::from(1), Word::ZERO),
        EVMC_STORAGE_MODIFIED => (Word::from(1), Word::from(1), Word::from(2)),
        EVMC_STORAGE_DELETED_ADDED => (Word::from(1), Word::ZERO, Word::from(2)),
        EVMC_STORAGE_MODIFIED_DELETED => (Word::from(1), Word::from(2), Word::ZERO),
        EVMC_STORAGE_DELETED_RESTORED => (Word::from(1), Word::ZERO, Word::from(1)),
        EVMC_STORAGE_ADDED_DELETED => (Word::ZERO, Word::from(1), Word::ZERO),
        EVMC_STORAGE_MODIFIED_RESTORED => (Word::from(1), Word::from(2), Word::from(1)),
        EVMC_STORAGE_ASSIGNED => (Word::from(1), Word::from(2), Word::from(3)),
    };
    SStore { original_value, present_value, new_value, is_cold }
}

fn address_from_evmc(address: evmc_address) -> Address {
    Address::from(address.bytes)
}

const fn address_to_evmc(address: Address) -> evmc_address {
    evmc_address { bytes: address.into_array() }
}

const fn b256_to_bytes32(value: B256) -> evmc_bytes32 {
    evmc_bytes32 { bytes: value.0 }
}

const fn word_to_bytes32(value: Word) -> evmc_bytes32 {
    evmc_bytes32 { bytes: value.to_be_bytes() }
}

const fn bytes32_to_word(value: evmc_bytes32) -> Word {
    Word::from_be_bytes(value.bytes)
}

const fn zero_address() -> evmc_address {
    evmc_address { bytes: [0; 20] }
}

const fn zero_tx_context() -> evmc_tx_context {
    unsafe { std::mem::zeroed() }
}

fn signed_to_u256(value: i64) -> U256 {
    u64::try_from(value).map_or(U256::ZERO, U256::from)
}

fn evmc_blob_hashes(tx_context: &evmc_tx_context) -> &[evmc_bytes32] {
    bytes_from_raw(tx_context.blob_hashes, tx_context.blob_hashes_count).unwrap_or_default()
}

fn bytes_from_raw<'a, T>(data: *const T, len: usize) -> Option<&'a [T]> {
    if len == 0 {
        return Some(&[]);
    }
    (!data.is_null()).then(|| unsafe { slice::from_raw_parts(data, len) })
}
