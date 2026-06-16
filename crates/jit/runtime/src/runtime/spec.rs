//! Spec ID conversions for imported revm-facing compiler code.

use evm2::SpecId;
#[cfg(feature = "llvm")]
use revm_primitives::hardfork::SpecId as RevmSpecId;

#[cfg(feature = "llvm")]
pub(crate) fn to_revm_spec_id(spec_id: SpecId) -> RevmSpecId {
    let spec_id = u8::try_from(u32::from(spec_id)).expect("evm2 SpecId does not fit in u8");
    RevmSpecId::try_from_u8(spec_id).expect("evm2 SpecId has no revm equivalent")
}

pub(crate) fn from_revm_spec_id(spec_id: revm_primitives::hardfork::SpecId) -> SpecId {
    SpecId::try_from_u32(u32::from(u8::from(spec_id))).expect("revm SpecId has no evm2 equivalent")
}
