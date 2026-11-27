//! Map between program counters and instruction counters.

use crate::{Instructions, SpecId};
use alloc::{vec, vec::Vec};

/// A map between the program counter and the instruction counter of each instruction.
///
/// The instruction counter is the index of an instruction in [`Instructions`], and the program
/// counter is the offset of its opcode byte. Lookups by program counter only find the first byte
/// of an instruction, not a byte of its immediate data.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InstructionMap {
    /// The program counter of every instruction, in order.
    pcs: Vec<u32>,
    /// One bit per byte of the code: whether an instruction starts there.
    starts: Vec<u64>,
    /// The number of instructions that start before each word of `starts`.
    ranks: Vec<u32>,
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
        let mut starts = vec![0u64; code.len().div_ceil(64)];
        for &pc in &pcs {
            starts[pc as usize / 64] |= 1 << (pc % 64);
        }
        let ranks = starts
            .iter()
            .scan(0, |rank, word| {
                let before = *rank;
                *rank += word.count_ones();
                Some(before)
            })
            .collect();
        Self { pcs, starts, ranks }
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
        let word = *self.starts.get(pc / 64)?;
        let bit = pc % 64;
        (word >> bit & 1 != 0)
            .then(|| self.ranks[pc / 64] as usize + (word & ((1 << bit) - 1)).count_ones() as usize)
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
    const SPEC: SpecId = SpecId::OSAKA;

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
    fn lookups_across_words() {
        // Instructions of every length, so that starts fall on and between the 64-bit words.
        let mut code = Vec::new();
        for i in 0..400u32 {
            let push = op::PUSH0 + (i % 33) as u8;
            code.push(if i % 3 == 0 { push } else { op::ADD });
            if i % 3 == 0 {
                code.extend((0..push - op::PUSH0).map(|_| 0xee));
            }
        }
        let map = InstructionMap::new(&code, SPEC);
        let starts = Instructions::new(&code, SPEC).map(|inst| inst.pc).collect::<Vec<_>>();
        assert_eq!(map.len(), starts.len());
        for (ic, &pc) in starts.iter().enumerate() {
            assert_eq!((map.pc(ic), map.ic(pc)), (Some(pc), Some(ic)));
        }
        let found = (0..code.len() + 200).filter_map(|pc| map.ic(pc)).collect::<Vec<_>>();
        assert_eq!(found, (0..starts.len()).collect::<Vec<_>>());
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
