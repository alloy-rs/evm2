#![no_main]

mod common;

use evm_smith::machine::{Config, Machine, OpcodeWeights};
use evm2_fuzzer::bytecode_case_with_spec;
use libfuzzer_sys::fuzz_target;

const GAS_LIMIT: u64 = 250_000;
const MAX_GENERATION_LEN: usize = 512;
const MAX_ROOT_STEPS: usize = 512;

fuzz_target!(|data: &[u8]| {
    let spec = common::target_spec("evm_smith_arbitrary_compare_");
    let mut config = Config::default();
    config.addresses.caller = [0x10; 20].into();
    config.addresses.contract = [0x20; 20].into();
    config.opcode_weights = OpcodeWeights { create: 0, ..OpcodeWeights::stateful() };

    let generation_len = data.len().min(MAX_GENERATION_LEN);
    let mut machine = Machine::new_arbitrary(GAS_LIMIT, data[..generation_len].to_vec(), config);
    for _ in 0..MAX_ROOT_STEPS {
        if machine.ingest_next().is_err() {
            break;
        }
    }
    let bytecode = machine.bytecode();

    common::run_case(bytecode_case_with_spec(spec, &bytecode));
});
