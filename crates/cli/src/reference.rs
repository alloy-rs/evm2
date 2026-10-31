//! Conversions to the reference implementation the fuzzer and replay benchmarks compare against.

use evm2::SpecId;
use revm::primitives::hardfork::SpecId as ReferenceSpecId;

/// Returns the reference implementation's spec ID for `spec`.
///
/// # Panics
///
/// Panics when the reference implementation has no counterpart of `spec`.
pub fn spec_id(spec: SpecId) -> ReferenceSpecId {
    macro_rules! convert {
        ([$spec:ident] $($variant:ident $lower:ident,)*) => {
            match $spec {
                $(SpecId::$variant => ReferenceSpecId::$variant,)*
                other => panic!("unsupported spec: {other:?}"),
            }
        };
    }
    evm2::for_each_spec!([spec] convert)
}
