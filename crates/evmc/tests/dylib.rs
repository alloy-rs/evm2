#![allow(missing_docs)]

use evm2_evmc::ffi::*;
use libloading::Library;
use std::{env, path::PathBuf, ptr};

type CreateFn = unsafe extern "C" fn() -> *mut evmc_vm;

#[test]
fn loads_and_executes_via_evmc_abi() {
    let library = unsafe { Library::new(dylib_path()) }.expect("load evm2 EVMC dylib");
    let create = unsafe { library.get::<CreateFn>(b"evmc_create_evm2") }
        .expect("load evmc_create_evm2 symbol");
    let vm = unsafe { create() };
    assert!(!vm.is_null());

    let vm = unsafe { &mut *vm };
    assert_eq!(unsafe { vm.get_capabilities.expect("get_capabilities")(vm) }, EVMC_CAPABILITY_EVM1);

    let host = evmc_host_interface {
        account_exists: None,
        get_storage: None,
        set_storage: None,
        get_balance: None,
        get_code_size: None,
        get_code_hash: None,
        copy_code: None,
        selfdestruct: None,
        call: None,
        get_tx_context: Some(get_tx_context),
        get_block_hash: None,
        emit_log: None,
        access_account: None,
        access_storage: None,
        get_transient_storage: None,
        set_transient_storage: None,
    };
    let message = evmc_message {
        kind: EVMC_CALL,
        flags: 0,
        depth: 0,
        gas: 1_000_000,
        recipient: zero_address(),
        sender: zero_address(),
        input_data: ptr::null(),
        input_size: 0,
        value: zero_bytes32(),
        create2_salt: zero_bytes32(),
        code_address: zero_address(),
        code: ptr::null(),
        code_size: 0,
    };

    let result =
        execute(vm, &host, &message, &[0x60, 0x2a, 0x60, 0x00, 0x52, 0x60, 0x20, 0x60, 0x00, 0xf3]);
    assert_eq!(result.status_code, EVMC_SUCCESS);
    assert_eq!(result.gas_left, 999_982);
    assert_eq!(result.output_size, 32);
    let output = unsafe { std::slice::from_raw_parts(result.output_data, result.output_size) };
    assert_eq!(output[31], 0x2a);
    release(result);

    let result =
        execute(vm, &host, &message, &[0x42, 0x60, 0x00, 0x52, 0x60, 0x20, 0x60, 0x00, 0xf3]);
    assert_eq!(result.status_code, EVMC_SUCCESS);
    assert_eq!(result.output_size, 32);
    let output = unsafe { std::slice::from_raw_parts(result.output_data, result.output_size) };
    assert_eq!(&output[24..], &[0xff; 8]);
    release(result);

    unsafe {
        vm.destroy.expect("destroy")(vm);
    }
}

fn execute(
    vm: &mut evmc_vm,
    host: &evmc_host_interface,
    message: &evmc_message,
    code: &[u8],
) -> evmc_result {
    unsafe {
        vm.execute.expect("execute")(
            vm,
            host,
            ptr::null_mut(),
            EVMC_SHANGHAI,
            message,
            code.as_ptr(),
            code.len(),
        )
    }
}

fn release(result: evmc_result) {
    if let Some(release) = result.release {
        unsafe {
            release(&result);
        }
    }
}

const unsafe extern "C" fn get_tx_context(_context: *mut evmc_host_context) -> evmc_tx_context {
    evmc_tx_context {
        tx_gas_price: zero_bytes32(),
        tx_origin: zero_address(),
        block_coinbase: zero_address(),
        block_number: -1,
        block_timestamp: -1,
        block_gas_limit: -1,
        block_prev_randao: zero_bytes32(),
        chain_id: zero_bytes32(),
        block_base_fee: zero_bytes32(),
        blob_base_fee: zero_bytes32(),
        blob_hashes: ptr::null(),
        blob_hashes_count: 0,
        block_slot_number: 0,
    }
}

const fn zero_address() -> evmc_address {
    evmc_address { bytes: [0; 20] }
}

const fn zero_bytes32() -> evmc_bytes32 {
    evmc_bytes32 { bytes: [0; 32] }
}

fn dylib_path() -> PathBuf {
    let mut path = env::current_exe().expect("current test executable");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.push(format!("{}evm2_evmc{}", env::consts::DLL_PREFIX, env::consts::DLL_SUFFIX));
    path
}
