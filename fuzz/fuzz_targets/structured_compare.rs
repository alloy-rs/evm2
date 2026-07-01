#![no_main]

mod common;

use evm2_fuzzer::arbitrary_case;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(case) = arbitrary_case(data) {
        common::run_case(case);
    }
});
