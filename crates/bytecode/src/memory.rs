//! Memory ranges that opcodes write.

use crate::opcode::{OpCode, op};
use alloy_primitives::U256;

impl OpCode {
    /// Returns the memory range `(offset, size)` that the opcode overwrites, derived from its
    /// inputs on the `stack`, which holds the top item last.
    ///
    /// These are `MSTORE`, `MSTORE8`, the copy opcodes, and the output range of the calls. The
    /// size can be zero. The result is `None` for any other opcode, if the stack is too short, or
    /// if an input does not fit a `usize`; the opcode fails without writing in the last two cases.
    pub fn memory_write_range(self, stack: &[U256]) -> Option<(usize, usize)> {
        let peek = |index: usize| usize::try_from(*stack.iter().rev().nth(index)?).ok();
        match self.get() {
            op::MSTORE => Some((peek(0)?, 32)),
            op::MSTORE8 => Some((peek(0)?, 1)),
            op::CALLDATACOPY | op::CODECOPY | op::RETURNDATACOPY | op::MCOPY => {
                Some((peek(0)?, peek(2)?))
            }
            op::EXTCODECOPY => Some((peek(1)?, peek(3)?)),
            op::CALL | op::CALLCODE => Some((peek(5)?, peek(6)?)),
            op::DELEGATECALL | op::STATICCALL => Some((peek(4)?, peek(5)?)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_write_range_reads_the_stack_from_the_top() {
        // The top of the stack is the last item.
        let stack = [1, 2, 3, 4, 5, 6, 7].map(U256::from);
        let range = |opcode: u8| OpCode::new_or_unknown(opcode).memory_write_range(&stack);
        assert_eq!(range(op::MSTORE), Some((7, 32)));
        assert_eq!(range(op::MSTORE8), Some((7, 1)));
        assert_eq!(range(op::MCOPY), Some((7, 5)));
        assert_eq!(range(op::CALLDATACOPY), Some((7, 5)));
        assert_eq!(range(op::CODECOPY), Some((7, 5)));
        assert_eq!(range(op::RETURNDATACOPY), Some((7, 5)));
        assert_eq!(range(op::EXTCODECOPY), Some((6, 4)));
        assert_eq!(range(op::CALL), Some((2, 1)));
        assert_eq!(range(op::CALLCODE), Some((2, 1)));
        assert_eq!(range(op::DELEGATECALL), Some((3, 2)));
        assert_eq!(range(op::STATICCALL), Some((3, 2)));
        // Reads, and opcodes that do not touch memory.
        assert_eq!(range(op::MLOAD), None);
        assert_eq!(range(op::KECCAK256), None);
        assert_eq!(range(op::ADD), None);
    }

    #[test]
    fn memory_write_range_keeps_zero_sizes_and_rejects_bad_inputs() {
        let range =
            |opcode: u8, stack: &[U256]| OpCode::new_or_unknown(opcode).memory_write_range(stack);
        // Zero size is a range of size zero.
        assert_eq!(range(op::MCOPY, &[U256::ZERO, U256::from(1), U256::from(2)]), Some((2, 0)));
        // A stack that is too short for the opcode.
        assert_eq!(range(op::MCOPY, &[U256::from(1), U256::from(2)]), None);
        assert_eq!(range(op::MSTORE, &[]), None);
        // An input that does not fit a usize.
        assert_eq!(range(op::MSTORE, &[U256::MAX]), None);
        assert_eq!(range(op::MCOPY, &[U256::MAX, U256::ZERO, U256::ZERO]), None);
        assert_eq!(
            range(op::MCOPY, &[U256::from(1), U256::ZERO, U256::from(usize::MAX)]),
            Some((usize::MAX, 1))
        );
    }
}
