#![no_main]

use evm_smith::machine::{Config, Machine, OpcodeWeights};
use evm2_fuzzer::{
    CaseContext, Evm2Backend, EvmBackend, RevmBackend, SpecId, bytecode_case_with_spec,
    compare_case,
};
use libfuzzer_sys::fuzz_target;

const NON_AMSTERDAM_SPECS: &[SpecId] = &[
    SpecId::FRONTIER,
    SpecId::HOMESTEAD,
    SpecId::TANGERINE,
    SpecId::SPURIOUS_DRAGON,
    SpecId::BYZANTIUM,
    SpecId::PETERSBURG,
    SpecId::ISTANBUL,
    SpecId::BERLIN,
    SpecId::LONDON,
    SpecId::MERGE,
    SpecId::SHANGHAI,
    SpecId::CANCUN,
    SpecId::PRAGUE,
    SpecId::OSAKA,
];

const GAS_LIMIT: u64 = 250_000;
const MAX_GENERATION_LEN: usize = 512;
const MAX_ROOT_STEPS: usize = 512;

fuzz_target!(|data: &[u8]| {
    let Some((&spec_byte, generation_data)) = data.split_first() else {
        return;
    };

    let spec = NON_AMSTERDAM_SPECS[usize::from(spec_byte) % NON_AMSTERDAM_SPECS.len()];
    let mut config = Config::default();
    config.addresses.caller = [0x10; 20].into();
    config.addresses.contract = [0x20; 20].into();
    config.opcode_weights = OpcodeWeights { create: 0, ..OpcodeWeights::stateful() };

    let generation_len = generation_data.len().min(MAX_GENERATION_LEN);
    let mut machine =
        Machine::new_arbitrary(GAS_LIMIT, generation_data[..generation_len].to_vec(), config);
    for _ in 0..MAX_ROOT_STEPS {
        if machine.ingest_next().is_err() {
            break;
        }
    }
    let bytecode = machine.bytecode();

    let backends: [&dyn EvmBackend; 2] = [&RevmBackend, &Evm2Backend];
    if let Err(err) =
        compare_case(&backends, &bytecode_case_with_spec(spec, &bytecode), CaseContext::Bytes)
    {
        panic!("{err}");
    }
});
