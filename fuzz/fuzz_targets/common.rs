use evm2_fuzzer::{
    CaseContext, Evm2Backend, EvmBackend, EvmCase, RevmBackend, SpecId, bytecode_case_with_spec,
    compare_case,
};

#[cfg(feature = "jit")]
use evm2_fuzzer::{JitEvm2Backend, jit_case_supported};

#[allow(dead_code)]
pub fn target_spec(prefix: &str) -> SpecId {
    let name = env!("CARGO_BIN_NAME");
    let suffix =
        name.strip_prefix(prefix).unwrap_or_else(|| panic!("{name} does not start with {prefix}"));
    spec_from_suffix(suffix)
}

#[allow(dead_code)]
fn spec_from_suffix(suffix: &str) -> SpecId {
    match suffix {
        "frontier" => SpecId::FRONTIER,
        "homestead" => SpecId::HOMESTEAD,
        "tangerine" => SpecId::TANGERINE,
        "spurious_dragon" => SpecId::SPURIOUS_DRAGON,
        "byzantium" => SpecId::BYZANTIUM,
        "petersburg" => SpecId::PETERSBURG,
        "istanbul" => SpecId::ISTANBUL,
        "berlin" => SpecId::BERLIN,
        "london" => SpecId::LONDON,
        "merge" => SpecId::MERGE,
        "shanghai" => SpecId::SHANGHAI,
        "cancun" => SpecId::CANCUN,
        "prague" => SpecId::PRAGUE,
        "osaka" => SpecId::OSAKA,
        "amsterdam" => SpecId::AMSTERDAM,
        _ => panic!("unknown hardfork suffix {suffix}"),
    }
}

#[allow(dead_code)]
pub fn run_bytecode_compare(spec: SpecId, data: &[u8]) {
    let case = bytecode_case_with_spec(spec, data);
    run_case(case);
}

#[cfg(not(feature = "jit"))]
pub fn run_case(case: EvmCase) {
    let backends: [&dyn EvmBackend; 2] = [&RevmBackend, &Evm2Backend];
    if let Err(err) = compare_case(&backends, &case, CaseContext::Bytes) {
        panic!("{err}");
    }
}

#[cfg(feature = "jit")]
pub fn run_case(case: EvmCase) {
    if !jit_case_supported(&case) {
        let backends: [&dyn EvmBackend; 2] = [&RevmBackend, &Evm2Backend];
        if let Err(err) = compare_case(&backends, &case, CaseContext::Bytes) {
            panic!("{err}");
        }
        return;
    }

    let backends: [&dyn EvmBackend; 3] = [&RevmBackend, &Evm2Backend, &JitEvm2Backend];
    if let Err(err) = compare_case(&backends, &case, CaseContext::Bytes) {
        panic!("{err}");
    }
}
