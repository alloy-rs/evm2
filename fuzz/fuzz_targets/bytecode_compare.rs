#![no_main]

mod bytecode_mutator;
mod common;

use evm2_fuzzer::BYTECODE_FUZZ_SPEC;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    common::run_bytecode_compare(BYTECODE_FUZZ_SPEC, data);
});
