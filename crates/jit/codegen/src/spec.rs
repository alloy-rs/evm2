//! Spec ID conversions for imported revm-facing compiler internals.

use evm2::SpecId;
#[cfg(any(test, feature = "__fuzzing"))]
use revm_primitives::hardfork::SpecId as RevmSpecId;

pub(crate) fn to_spec_id_byte(spec_id: SpecId) -> u8 {
    u8::try_from(u32::from(spec_id)).expect("evm2 SpecId does not fit in u8")
}

#[cfg(any(test, feature = "__fuzzing"))]
pub(crate) fn from_revm_spec_id(spec_id: RevmSpecId) -> SpecId {
    SpecId::try_from_u32(u32::from(u8::from(spec_id))).expect("revm SpecId has no evm2 equivalent")
}
