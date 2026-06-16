//! Spec ID conversions for imported revm-facing compiler code.

use evm2::SpecId;

pub(crate) fn from_revm_spec_id(spec_id: revm_primitives::hardfork::SpecId) -> SpecId {
    SpecId::try_from_u32(u32::from(u8::from(spec_id))).expect("revm SpecId has no evm2 equivalent")
}
