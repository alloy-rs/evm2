#![no_main]

use libfuzzer_sys::fuzz_target;

#[cfg(feature = "jit")]
use evm2_fuzzer::{
    CaseContext, Evm2Backend, EvmBackend, JitEvm2Backend, SpecId, bytecode_case_with_spec,
    compare_case, jit_bytecode_supported,
};

#[cfg(feature = "jit")]
fuzz_target!(|data: &[u8]| {
    let Some((&spec_byte, bytecode)) = data.split_first() else { return };
    let spec = SpecId::try_from_u32(u32::from(spec_byte) % SpecId::COUNT as u32)
        .unwrap_or(SpecId::DEFAULT);
    if !jit_bytecode_supported(spec, bytecode) {
        return;
    }

    let case = bytecode_case_with_spec(spec, bytecode);
    let backends: [&dyn EvmBackend; 2] = [&Evm2Backend, &JitEvm2Backend];
    if let Err(err) = compare_case(&backends, &case, CaseContext::Bytes) {
        panic!("{err}");
    }
});

#[cfg(not(feature = "jit"))]
fuzz_target!(|data: &[u8]| {
    let _ = data;
});
