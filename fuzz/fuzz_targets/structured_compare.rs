#![no_main]

mod common;

use evm2_fuzzer::arbitrary_case_with_spec;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let spec = common::target_spec("structured_compare_");
    if let Ok(case) = arbitrary_case_with_spec(data, spec) {
        common::run_case(case);
    }
});
