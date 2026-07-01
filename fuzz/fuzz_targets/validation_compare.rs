#![no_main]

use evm2_fuzzer::{
    CaseContext, Evm2Backend, EvmBackend, RevmBackend, compare_case_acceptance,
    generate_validation_case,
};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut bytes = [0u8; 16];
    let len = data.len().min(bytes.len());
    bytes[..len].copy_from_slice(&data[..len]);

    let seed = u64::from_le_bytes(bytes[..8].try_into().unwrap());
    let case_index = u64::from_le_bytes(bytes[8..].try_into().unwrap());
    let backends: [&dyn EvmBackend; 2] = [&RevmBackend, &Evm2Backend];
    if let Err(err) = compare_case_acceptance(
        &backends,
        &generate_validation_case(seed, case_index),
        CaseContext::Generated { seed, case_index },
    ) {
        panic!("{err}");
    }
});
