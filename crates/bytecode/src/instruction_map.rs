//! Map between program counters and instruction counters.

use crate::{Instructions, SpecId};
use alloc::vec::Vec;

/// A map between the program counter and the instruction counter of each instruction.
///
/// The instruction counter is the index of an instruction in [`Instructions`], and the program
/// counter is the offset of its opcode byte. Lookups by program counter only find the first byte
/// of an instruction, not a byte of its immediate data.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InstructionMap {
    /// The program counter of every instruction, in order.
    pcs: Vec<u32>,
}

impl InstructionMap {
    /// Creates the map for `code`, decoded as [`Instructions::new`] does.
    ///
    /// # Panics
    ///
    /// Panics if `code` is longer than `u32::MAX` bytes.
    pub fn new(code: &[u8], spec_id: SpecId) -> Self {
        assert!(u32::try_from(code.len()).is_ok(), "bytecode is too big");
        let mut pcs =
            Instructions::new(code, spec_id).map(|inst| inst.pc as u32).collect::<Vec<_>>();
        pcs.shrink_to_fit();
        Self { pcs }
    }

    /// Returns the number of instructions.
    #[inline]
    pub const fn len(&self) -> usize {
        self.pcs.len()
    }

    /// Returns `true` if there are no instructions.
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.pcs.is_empty()
    }

    /// Returns the program counter of the instruction with instruction counter `ic`.
    #[inline]
    pub fn pc(&self, ic: usize) -> Option<usize> {
        self.pcs.get(ic).map(|&pc| pc as usize)
    }

    /// Returns the instruction counter of the instruction that starts at `pc`.
    #[inline]
    pub fn ic(&self, pc: usize) -> Option<usize> {
        self.pcs.binary_search(&u32::try_from(pc).ok()?).ok()
    }

    /// Returns an iterator over the `(instruction counter, program counter)` pairs.
    #[inline]
    pub fn iter(&self) -> impl ExactSizeIterator<Item = (usize, usize)> + '_ {
        self.pcs.iter().enumerate().map(|(ic, &pc)| (ic, pc as usize))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opcode::op;

    #[test]
    fn maps_both_ways() {
        // PUSH2 0xaaaa, ADD, PUSH1 0xbb, JUMPDEST, PUSH3 with a truncated immediate.
        let code = [op::PUSH2, 0xaa, 0xaa, op::ADD, op::PUSH1, 0xbb, op::JUMPDEST, op::PUSH3, 0x01];
        let map = InstructionMap::new(&code, SpecId::OSAKA);
        assert_eq!(map.len(), 5);
        assert!(!map.is_empty());
        assert_eq!(map.iter().collect::<Vec<_>>(), [(0, 0), (1, 3), (2, 4), (3, 6), (4, 7)]);
        for (ic, pc) in map.iter() {
            assert_eq!(map.pc(ic), Some(pc));
            assert_eq!(map.ic(pc), Some(ic));
        }
        assert_eq!(map.pc(5), None);
        // Immediate bytes and offsets past the end are not instructions.
        for pc in [1, 2, 5, 8, 9, usize::MAX] {
            assert_eq!(map.ic(pc), None, "{pc}");
        }
    }

    #[test]
    fn follows_eip8024_decoding() {
        // DUPN takes its valid immediate only from Amsterdam on.
        let code = [op::DUPN, 0x80, op::STOP];
        let amsterdam = InstructionMap::new(&code, SpecId::AMSTERDAM);
        assert_eq!(amsterdam.iter().collect::<Vec<_>>(), [(0, 0), (1, 2)]);
        let osaka = InstructionMap::new(&code, SpecId::OSAKA);
        assert_eq!(osaka.iter().collect::<Vec<_>>(), [(0, 0), (1, 1), (2, 2)]);
    }

    #[test]
    fn empty() {
        let map = InstructionMap::new(&[], SpecId::OSAKA);
        assert!(map.is_empty());
        assert_eq!(map.pc(0), None);
        assert_eq!(map.ic(0), None);
        assert_eq!(map, InstructionMap::default());
    }
}
