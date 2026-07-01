//! Shared support for the `evm2` command-line crate.

#[allow(missing_docs)]
pub mod evm_bench;

#[allow(missing_docs)]
pub mod fuzzer;

pub use fuzzer::{
    BYTECODE_FUZZ_SPEC, CaseContext, Evm2Backend, EvmBackend, EvmCase, Outcome, RevmBackend,
    SpecId, arbitrary_case, bytecode_case, bytecode_case_with_spec, compare_case,
    compare_case_acceptance, generate_case, generate_validation_case,
};
#[cfg(feature = "jit")]
pub use fuzzer::{JitEvm2Backend, jit_bytecode_supported};
