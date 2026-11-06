//! Conversions to the reference implementation the fuzzer and replay benchmarks compare against.

use evm2::SpecId;
use revm::primitives::hardfork::SpecId as ReferenceSpecId;

/// Returns the reference spec ID, falling back to Amsterdam for unsupported forks.
pub const fn spec_id(spec: SpecId) -> ReferenceSpecId {
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
        _ => ReferenceSpecId::AMSTERDAM,
    }
}
