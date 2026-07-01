#![no_main]

mod common;

use evm2_fuzzer::SpecId;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    common::run_bytecode_compare(SpecId::TANGERINE, data);
});
