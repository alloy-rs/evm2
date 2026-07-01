#![no_main]

use evm_smith::machine::{Config, Machine};
use evm2_fuzzer::{
    CaseContext, Evm2Backend, EvmBackend, RevmBackend, SpecId, bytecode_case_with_spec,
    compare_case,
};
use fastrand::Rng;
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

fuzz_target!(|data: &[u8]| {
    let Some((&spec_byte, seed_data)) = data.split_first() else {
        return;
    };

    let spec = NON_AMSTERDAM_SPECS[usize::from(spec_byte) % NON_AMSTERDAM_SPECS.len()];
    let mut seed_bytes = [0u8; 8];
    let seed_len = seed_data.len().min(seed_bytes.len());
    seed_bytes[..seed_len].copy_from_slice(&seed_data[..seed_len]);
    let seed = u64::from_le_bytes(seed_bytes);

    let mut config = Config::default();
    config.addresses.caller = [0x10; 20].into();
    config.addresses.contract = [0x20; 20].into();

    let mut machine = Machine::new_rng(1_000_000, Rng::with_seed(seed), config);
    while machine.ingest_next().is_ok() {}
    let bytecode = machine.bytecode();

    let backends: [&dyn EvmBackend; 2] = [&RevmBackend, &Evm2Backend];
    if let Err(err) =
        compare_case(&backends, &bytecode_case_with_spec(spec, &bytecode), CaseContext::Bytes)
    {
        panic!("{err}");
    }
});
