//! Conversions to the reference implementation the fuzzer and replay benchmarks compare against.

use evm2::SpecId;
use revm::primitives::hardfork::SpecId as ReferenceSpecId;

/// Returns the reference implementation's spec ID for `spec`.
///
/// # Panics
///
/// Panics when the reference implementation has no counterpart of `spec`.
pub fn spec_id(spec: SpecId) -> ReferenceSpecId {
    match spec {
        SpecId::FRONTIER => ReferenceSpecId::FRONTIER,
        SpecId::HOMESTEAD => ReferenceSpecId::HOMESTEAD,
        SpecId::TANGERINE => ReferenceSpecId::TANGERINE,
        SpecId::SPURIOUS_DRAGON => ReferenceSpecId::SPURIOUS_DRAGON,
        SpecId::BYZANTIUM => ReferenceSpecId::BYZANTIUM,
        SpecId::PETERSBURG => ReferenceSpecId::PETERSBURG,
        SpecId::ISTANBUL => ReferenceSpecId::ISTANBUL,
        SpecId::BERLIN => ReferenceSpecId::BERLIN,
        SpecId::LONDON => ReferenceSpecId::LONDON,
        SpecId::MERGE => ReferenceSpecId::MERGE,
        SpecId::SHANGHAI => ReferenceSpecId::SHANGHAI,
        SpecId::CANCUN => ReferenceSpecId::CANCUN,
        SpecId::PRAGUE => ReferenceSpecId::PRAGUE,
        SpecId::OSAKA => ReferenceSpecId::OSAKA,
        SpecId::AMSTERDAM => ReferenceSpecId::AMSTERDAM,
        other => panic!("unsupported spec: {other:?}"),
    }
}
