use evm2::interpreter::opcode::OPCODE_INFO;

pub use evm2::interpreter::instructions::{decode_pair, decode_single, encode_pair, encode_single};

/// Returns the length of the immediate data for the given opcode, or `0` if none.
#[inline]
pub const fn min_imm_len(op: u8) -> u8 {
    if let Some(info) = &OPCODE_INFO[op as usize] { info.immediate_size() } else { 0 }
}

/// Returns the number of input and output stack elements of the given opcode.
#[inline]
pub const fn stack_io(op: u8) -> (u8, u8) {
    if let Some(info) = &OPCODE_INFO[op as usize] {
        (info.inputs(), info.outputs())
    } else {
        (0, 0)
    }
}
