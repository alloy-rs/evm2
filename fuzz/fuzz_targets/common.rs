use evm2_fuzzer::{
    CaseContext, Evm2Backend, EvmBackend, EvmCase, RevmBackend, SpecId, bytecode_case_with_spec,
    compare_case,
};

#[cfg(feature = "jit")]
use evm2_fuzzer::{JitEvm2Backend, jit_bytecode_supported};

#[allow(dead_code)]
pub fn run_bytecode_compare(spec: SpecId, data: &[u8]) {
    let case = bytecode_case_with_spec(spec, data);
    run_bytecode_case(spec, case, data);
}

pub fn run_case(case: EvmCase) {
    let backends: [&dyn EvmBackend; 2] = [&RevmBackend, &Evm2Backend];
    if let Err(err) = compare_case(&backends, &case, CaseContext::Bytes) {
        panic!("{err}");
    }
}

#[cfg(not(feature = "jit"))]
fn run_bytecode_case(_spec: SpecId, case: EvmCase, _bytecode: &[u8]) {
    run_case(case);
}

#[cfg(feature = "jit")]
fn run_bytecode_case(spec: SpecId, case: EvmCase, bytecode: &[u8]) {
    if !jit_bytecode_supported(spec, bytecode) {
        run_case(case);
        return;
    }

    let backends: [&dyn EvmBackend; 3] = [&RevmBackend, &Evm2Backend, &JitEvm2Backend];
    if let Err(err) = compare_case(&backends, &case, CaseContext::Bytes) {
        panic!("{err}");
    }
}
