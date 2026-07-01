#![no_main]

mod common;

use evm_smith::machine::{Config, Machine};
use evm2_fuzzer::bytecode_case_with_spec;
use fastrand::Rng;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let spec = common::target_spec("evm_smith_compare_");
    let mut seed_bytes = [0u8; 8];
    let seed_len = data.len().min(seed_bytes.len());
    seed_bytes[..seed_len].copy_from_slice(&data[..seed_len]);
    let seed = u64::from_le_bytes(seed_bytes);

    let mut config = Config::default();
    config.addresses.caller = [0x10; 20].into();
    config.addresses.contract = [0x20; 20].into();

    let mut machine = Machine::new_rng(1_000_000, Rng::with_seed(seed), config);
    while machine.ingest_next().is_ok() {}
    let bytecode = machine.bytecode();

    common::run_case(bytecode_case_with_spec(spec, &bytecode));
});
