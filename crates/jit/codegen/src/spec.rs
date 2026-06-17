//! Spec ID helpers.

use evm2::SpecId;

pub(crate) fn to_spec_id_byte(spec_id: SpecId) -> u8 {
    u8::try_from(u32::from(spec_id)).expect("evm2 SpecId does not fit in u8")
}
