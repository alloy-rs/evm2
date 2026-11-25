//! Basic blocks of a bytecode.

use crate::{Instruction, Instructions, SpecId, opcode::op};
use alloc::{boxed::Box, vec::Vec};
use alloy_primitives::U256;
use core::{fmt, ops::Range, slice};

/// The targets of a `JUMP` or `JUMPI`, as indices of [`BasicBlocks::blocks`].
///
/// Valid targets are the blocks that start with a `JUMPDEST`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JumpTarget {
    /// The target is not known.
    Unknown,
    /// No target is valid: a `JUMP` halts execution with an invalid jump, and a `JUMPI` halts
    /// when it is taken. A jump that never runs can have this target too.
    Invalid,
    /// The jump goes to this block.
    Block(usize),
    /// The jump goes to one of these blocks.
    Blocks(Box<[usize]>),
}

impl JumpTarget {
    /// Returns the blocks the jump can go to, which is none if the target is not known or invalid.
    #[inline]
    pub fn blocks(&self) -> &[usize] {
        match self {
            Self::Unknown | Self::Invalid => &[],
            Self::Block(block) => slice::from_ref(block),
            Self::Blocks(blocks) => blocks,
        }
    }
}

impl fmt::Display for JumpTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown => f.write_str("?"),
            Self::Invalid => f.write_str("invalid"),
            Self::Block(_) | Self::Blocks(_) => {
                for (i, block) in self.blocks().iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "bb{block}")?;
                }
                Ok(())
            }
        }
    }
}

/// How a [`BasicBlock`] ends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Terminator {
    /// Execution continues in the next block, which starts with a `JUMPDEST`.
    FallThrough,
    /// The block ends with a `JUMP`.
    Jump(JumpTarget),
    /// The block ends with a `JUMPI`. Execution continues in the next block if the jump is not
    /// taken, or at the end of the bytecode if there is none.
    JumpI(JumpTarget),
    /// Execution stops: the last instruction is `STOP`, `RETURN`, `REVERT`, `SELFDESTRUCT`,
    /// `INVALID`, an opcode that `spec_id` does not enable, or a `DUPN`, `SWAPN` or `EXCHANGE`
    /// with an invalid immediate, or the bytecode ends.
    Halt,
}

/// A run of instructions that execution enters only at its first instruction and leaves only at
/// its last.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BasicBlock {
    start_pc: usize,
    end_pc: usize,
    insts: Range<usize>,
    terminator: Terminator,
}

impl BasicBlock {
    /// Returns the range of program counters that the block covers.
    #[inline]
    pub const fn pc_range(&self) -> Range<usize> {
        self.start_pc..self.end_pc
    }

    /// Returns the range of the block's instructions in [`BasicBlocks::instructions`].
    #[inline]
    pub fn inst_range(&self) -> Range<usize> {
        self.insts.clone()
    }

    /// Returns how the block ends.
    #[inline]
    pub const fn terminator(&self) -> &Terminator {
        &self.terminator
    }
}

/// The instructions of a bytecode split into basic blocks.
///
/// A block starts at the first instruction, at every `JUMPDEST` that is a valid jump destination,
/// and after every `JUMP`, `JUMPI`, and halting instruction.
///
/// [`BasicBlocks::new`] only knows the target of a `JUMP` or `JUMPI` that follows a `PUSH`.
/// Another analysis can set the others with [`BasicBlocks::set_jump_target`].
#[derive(Clone, Debug)]
pub struct BasicBlocks<'a> {
    insts: Vec<Instruction<'a>>,
    blocks: Vec<BasicBlock>,
    spec_id: SpecId,
}

impl<'a> BasicBlocks<'a> {
    /// Splits `code` into basic blocks.
    pub fn new(code: &'a [u8], spec_id: SpecId) -> Self {
        let mut insts = Vec::with_capacity(code.len() / 2);
        insts.extend(Instructions::new(code, spec_id));

        let mut blocks = Vec::new();
        let mut start = 0;
        let mut close = |start: usize, end: usize, terminator| {
            blocks.push(BasicBlock {
                start_pc: insts[start].pc,
                end_pc: insts[end - 1].end_pc(),
                insts: start..end,
                terminator,
            });
        };
        for (i, inst) in insts.iter().enumerate() {
            let opcode = inst.opcode.get();
            if opcode == op::JUMPDEST && i > start {
                close(start, i, Terminator::FallThrough);
                start = i;
            }
            let terminator = match opcode {
                op::JUMP => Terminator::Jump(JumpTarget::Unknown),
                op::JUMPI => Terminator::JumpI(JumpTarget::Unknown),
                // An enabled `DUPN`, `SWAPN` or `EXCHANGE` without an immediate has an invalid one.
                op::DUPN | op::SWAPN | op::EXCHANGE if inst.immediate.is_empty() => {
                    Terminator::Halt
                }
                _ if !inst.opcode.is_enabled(spec_id) || inst.opcode.info().is_terminating() => {
                    Terminator::Halt
                }
                _ => continue,
            };
            close(start, i + 1, terminator);
            start = i + 1;
        }
        if start < insts.len() {
            close(start, insts.len(), Terminator::Halt);
        }

        let mut this = Self { insts, blocks, spec_id };
        this.resolve_adjacent_jumps();
        this
    }

    /// Returns all instructions in program order.
    #[inline]
    pub fn instructions(&self) -> &[Instruction<'a>] {
        &self.insts
    }

    /// Returns the basic blocks in program order.
    #[inline]
    pub fn blocks(&self) -> &[BasicBlock] {
        &self.blocks
    }

    /// Returns the instructions of the block with index `block`.
    ///
    /// # Panics
    ///
    /// Panics if `block` is out of bounds.
    #[inline]
    pub fn block_instructions(&self, block: usize) -> &[Instruction<'a>] {
        &self.insts[self.blocks[block].insts.clone()]
    }

    /// Returns the index of the block that contains the byte at `pc`.
    #[inline]
    pub fn block_at(&self, pc: usize) -> Option<usize> {
        let block = self.blocks.partition_point(|block| block.end_pc <= pc);
        self.blocks.get(block).filter(|block| block.start_pc <= pc)?;
        Some(block)
    }

    /// Returns the index of the block that starts at `pc`.
    #[inline]
    pub fn block_starting_at(&self, pc: usize) -> Option<usize> {
        self.block_at(pc).filter(|&block| self.blocks[block].start_pc == pc)
    }

    /// Returns the instruction that starts at `pc`.
    #[inline]
    pub fn instruction_at(&self, pc: usize) -> Option<&Instruction<'a>> {
        let index = self.insts.partition_point(|inst| inst.pc < pc);
        self.insts.get(index).filter(|inst| inst.pc == pc)
    }

    /// Returns the blocks that execution can go to from `block`, as far as the jump targets are
    /// known.
    ///
    /// # Panics
    ///
    /// Panics if `block` is out of bounds.
    pub fn successors(&self, block: usize) -> impl Iterator<Item = usize> + '_ {
        let (targets, falls_through) = match &self.blocks[block].terminator {
            Terminator::FallThrough => (&[][..], true),
            Terminator::Jump(target) => (target.blocks(), false),
            Terminator::JumpI(target) => (target.blocks(), true),
            Terminator::Halt => (&[][..], false),
        };
        let next = (falls_through && block + 1 < self.blocks.len()).then_some(block + 1);
        targets.iter().copied().chain(next)
    }

    /// Returns for every block whether execution can reach it from the first block.
    ///
    /// If a reachable jump has an unknown target, every block that starts with a `JUMPDEST` is
    /// reachable.
    pub fn reachable(&self) -> Vec<bool> {
        let mut reachable = alloc::vec![false; self.blocks.len()];
        if reachable.is_empty() {
            return reachable;
        }
        reachable[0] = true;
        let mut stack = alloc::vec![0];
        let mut any_jumpdest = false;
        while let Some(block) = stack.pop() {
            let unknown = matches!(
                self.blocks[block].terminator,
                Terminator::Jump(JumpTarget::Unknown) | Terminator::JumpI(JumpTarget::Unknown)
            );
            let mut visit = |block: usize| {
                if !reachable[block] {
                    reachable[block] = true;
                    stack.push(block);
                }
            };
            if unknown && !any_jumpdest {
                any_jumpdest = true;
                (0..self.blocks.len()).filter(|&i| self.is_jumpdest(i)).for_each(&mut visit);
            }
            self.successors(block).for_each(visit);
        }
        reachable
    }

    /// Sets the target of the `JUMP` or `JUMPI` that ends `block`.
    ///
    /// Returns `false` and changes nothing if `block` does not end with a `JUMP` or `JUMPI`, or if
    /// `target` names a block that does not start with a `JUMPDEST`.
    pub fn set_jump_target(&mut self, block: usize, target: JumpTarget) -> bool {
        if !target
            .blocks()
            .iter()
            .all(|&target| target < self.blocks.len() && self.is_jumpdest(target))
        {
            return false;
        }
        match self.blocks.get_mut(block).map(|block| &mut block.terminator) {
            Some(Terminator::Jump(old) | Terminator::JumpI(old)) => {
                *old = target;
                true
            }
            _ => false,
        }
    }

    fn is_jumpdest(&self, block: usize) -> bool {
        self.insts[self.blocks[block].insts.start].opcode.is_jumpdest()
    }

    /// Sets the target of every `JUMP` and `JUMPI` that follows a `PUSH`.
    fn resolve_adjacent_jumps(&mut self) {
        for block in 0..self.blocks.len() {
            let range = self.blocks[block].insts.clone();
            let [.., push, _jump] = &self.insts[range] else { continue };
            if !matches!(push.opcode.get(), op::PUSH0..=op::PUSH32) {
                continue;
            }
            let target = usize::try_from(U256::from_be_slice(push.immediate))
                .ok()
                .and_then(|pc| self.block_starting_at(pc))
                .filter(|&b| self.is_jumpdest(b));
            let target = target.map_or(JumpTarget::Invalid, JumpTarget::Block);
            if let Terminator::Jump(old) | Terminator::JumpI(old) =
                &mut self.blocks[block].terminator
            {
                *old = target;
            }
        }
    }
}

impl fmt::Display for BasicBlocks<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let code_len = self.insts.last().map_or(0, Instruction::end_pc);
        let width = (usize::BITS - code_len.leading_zeros()).div_ceil(4).max(4) as usize;
        for (i, block) in self.blocks.iter().enumerate() {
            if i > 0 {
                f.write_str("\n")?;
            }
            write!(f, "bb{i}:")?;
            for inst in self.block_instructions(i) {
                write!(f, "\n  {:0width$x}  {}", inst.pc, inst.display(self.spec_id))?;
            }
            match &block.terminator {
                Terminator::Jump(target) => write!(f, "  ; -> {target}")?,
                Terminator::JumpI(target) => {
                    write!(f, "  ; -> {target}")?;
                    if i + 1 < self.blocks.len() {
                        write!(f, ", else bb{}", i + 1)?;
                    }
                }
                Terminator::FallThrough | Terminator::Halt => {}
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::{string::ToString, vec};

    const SPEC: SpecId = SpecId::OSAKA;

    fn starts(blocks: &BasicBlocks<'_>) -> Vec<usize> {
        blocks.blocks().iter().map(|block| block.pc_range().start).collect()
    }

    fn terminators(blocks: &BasicBlocks<'_>) -> Vec<Terminator> {
        blocks.blocks().iter().map(|block| block.terminator().clone()).collect()
    }

    #[test]
    fn empty() {
        let blocks = BasicBlocks::new(&[], SPEC);
        assert!(blocks.blocks().is_empty());
        assert!(blocks.reachable().is_empty());
        assert_eq!(blocks.to_string(), "");
    }

    #[test]
    fn leaders() {
        // 0: PUSH1 0x08, 2: JUMPI, 3: PUSH0, 4: PUSH0, 5: RETURN, 6: ADD, 7: ADD, 8: JUMPDEST,
        // 9: JUMPDEST, 10: STOP.
        let code = [
            op::PUSH1,
            0x08,
            op::JUMPI,
            op::PUSH0,
            op::PUSH0,
            op::RETURN,
            op::ADD,
            op::ADD,
            op::JUMPDEST,
            op::JUMPDEST,
            op::STOP,
        ];
        let blocks = BasicBlocks::new(&code, SPEC);
        assert_eq!(starts(&blocks), [0, 3, 6, 8, 9]);
        assert_eq!(
            terminators(&blocks),
            [
                Terminator::JumpI(JumpTarget::Block(3)),
                Terminator::Halt,
                Terminator::FallThrough,
                Terminator::FallThrough,
                Terminator::Halt,
            ]
        );
        assert_eq!(blocks.blocks()[1].inst_range(), 2..5);
    }

    #[test]
    fn jumpdest_in_push_data_is_not_a_leader() {
        // PUSH2 0x5b5b, JUMPDEST, STOP.
        let code = [op::PUSH2, op::JUMPDEST, op::JUMPDEST, op::JUMPDEST, op::STOP];
        let blocks = BasicBlocks::new(&code, SPEC);
        assert_eq!(starts(&blocks), [0, 3]);
        assert_eq!(blocks.block_at(1), Some(0));
    }

    #[test]
    fn truncated_push() {
        let code = [op::JUMPDEST, op::PUSH4, 0x01, 0x02];
        let blocks = BasicBlocks::new(&code, SPEC);
        assert_eq!(starts(&blocks), [0]);
        assert_eq!(blocks.blocks()[0].pc_range(), 0..4);
        assert_eq!(blocks.instruction_at(1).unwrap().immediate, [0x01, 0x02]);
        assert_eq!(terminators(&blocks), [Terminator::Halt]);
    }

    #[test]
    fn eip8024_immediates_depend_on_spec() {
        let code = [op::DUPN, 0x00, op::JUMPDEST, op::STOP];
        // Amsterdam: DUPN takes 0x00, which is not an instruction.
        let amsterdam = BasicBlocks::new(&code, SpecId::AMSTERDAM);
        assert_eq!(amsterdam.instructions()[0].immediate, [0x00]);
        assert_eq!(starts(&amsterdam), [0, 2]);
        assert_eq!(terminators(&amsterdam), [Terminator::FallThrough, Terminator::Halt]);
        // Osaka: DUPN is not enabled, so it halts, and the immediate is a STOP.
        let osaka = BasicBlocks::new(&code, SpecId::OSAKA);
        assert!(osaka.instructions()[0].immediate.is_empty());
        assert_eq!(starts(&osaka), [0, 1, 2]);
        assert_eq!(terminators(&osaka), [Terminator::Halt, Terminator::Halt, Terminator::Halt]);
    }

    #[test]
    fn invalid_eip8024_immediate_halts_and_keeps_the_jumpdest() {
        // 0x5b is no valid immediate, so DUPN halts and the JUMPDEST is a valid target.
        let code = [op::PUSH1, 0x04, op::JUMP, op::DUPN, op::JUMPDEST, op::STOP];
        let blocks = BasicBlocks::new(&code, SpecId::AMSTERDAM);
        assert_eq!(starts(&blocks), [0, 3, 4]);
        assert_eq!(
            terminators(&blocks),
            [Terminator::Jump(JumpTarget::Block(2)), Terminator::Halt, Terminator::Halt]
        );
    }

    #[test]
    fn unknown_and_disabled_opcodes_halt() {
        // 0x0c is unknown, PUSH0 is not enabled before Shanghai.
        let code = [0x0c, op::JUMPDEST, op::PUSH0, op::JUMPDEST];
        let blocks = BasicBlocks::new(&code, SpecId::MERGE);
        assert_eq!(starts(&blocks), [0, 1, 3]);
        assert_eq!(terminators(&blocks), [Terminator::Halt, Terminator::Halt, Terminator::Halt]);
        let blocks = BasicBlocks::new(&code, SpecId::SHANGHAI);
        assert_eq!(starts(&blocks), [0, 1, 3]);
        assert_eq!(
            terminators(&blocks),
            [Terminator::Halt, Terminator::FallThrough, Terminator::Halt]
        );
    }

    #[test]
    fn static_jump_targets() {
        // 0: PUSH1 0x07, 2: JUMP, 3: PUSH1 0x0a, 5: JUMP, 6: INVALID,
        // 7: JUMPDEST, 8: PUSH1 0x06, 10: JUMP.
        let code = [
            op::PUSH1,
            0x07,
            op::JUMP,
            op::PUSH1,
            0x0a,
            op::JUMP,
            op::INVALID,
            op::JUMPDEST,
            op::PUSH1,
            0x06,
            op::JUMP,
        ];
        let blocks = BasicBlocks::new(&code, SPEC);
        assert_eq!(starts(&blocks), [0, 3, 6, 7]);
        assert_eq!(
            terminators(&blocks),
            [
                // Valid.
                Terminator::Jump(JumpTarget::Block(3)),
                // The middle of a block.
                Terminator::Jump(JumpTarget::Invalid),
                Terminator::Halt,
                // Not a JUMPDEST.
                Terminator::Jump(JumpTarget::Invalid),
            ]
        );
    }

    #[test]
    fn static_jump_targets_edge_cases() {
        let jump = |code: &[u8]| {
            let blocks = BasicBlocks::new(code, SPEC);
            terminators(&blocks)[0].clone()
        };
        // A target that is the middle of PUSH data is not a destination, even if it holds 0x5b.
        assert_eq!(
            jump(&[op::PUSH1, 0x04, op::JUMP, op::PUSH2, op::JUMPDEST, 0x00]),
            Terminator::Jump(JumpTarget::Invalid)
        );
        // A target wider than usize is invalid.
        let mut wide = vec![op::PUSH32, 1];
        wide.extend([0; 31]);
        wide.push(op::JUMP);
        assert_eq!(jump(&wide), Terminator::Jump(JumpTarget::Invalid));
        // Leading zeros are fine.
        let mut padded = vec![op::PUSH32];
        padded.extend([0; 31]);
        padded.extend([0x22, op::JUMP, op::JUMPDEST]);
        assert_eq!(jump(&padded), Terminator::Jump(JumpTarget::Block(1)));
        // Beyond the end of the code.
        assert_eq!(jump(&[op::PUSH1, 0xff, op::JUMP]), Terminator::Jump(JumpTarget::Invalid));
        // PUSH0 goes to pc 0.
        assert_eq!(jump(&[op::PUSH0, op::JUMP]), Terminator::Jump(JumpTarget::Invalid));
        assert_eq!(
            jump(&[op::JUMPDEST, op::PUSH0, op::JUMP]),
            Terminator::Jump(JumpTarget::Block(0))
        );
        // A JUMP that does not follow a PUSH is unknown.
        assert_eq!(
            jump(&[op::PUSH1, 0, op::DUP1, op::JUMP]),
            Terminator::Jump(JumpTarget::Unknown)
        );
        assert_eq!(jump(&[op::JUMP]), Terminator::Jump(JumpTarget::Unknown));
        assert_eq!(
            jump(&[op::PUSH1, 0x04, op::JUMPI, op::STOP, op::JUMPDEST]),
            Terminator::JumpI(JumpTarget::Block(2))
        );
    }

    #[test]
    fn jump_does_not_use_push_of_previous_block() {
        // The JUMPDEST starts a block, so the PUSH is in the previous one.
        let code = [op::PUSH1, 0x03, op::JUMPDEST, op::JUMP];
        let blocks = BasicBlocks::new(&code, SPEC);
        assert_eq!(starts(&blocks), [0, 2]);
        assert_eq!(terminators(&blocks)[1], Terminator::Jump(JumpTarget::Unknown));
    }

    #[test]
    fn set_jump_target() {
        // 0: PUSH0, 1: MLOAD, 2: JUMP, 3: JUMPDEST, 4: STOP.
        let code = [op::PUSH0, op::MLOAD, op::JUMP, op::JUMPDEST, op::STOP];
        let mut blocks = BasicBlocks::new(&code, SPEC);
        assert_eq!(terminators(&blocks)[0], Terminator::Jump(JumpTarget::Unknown));
        // Not a JUMPDEST block, out of range, and not a jump.
        assert!(!blocks.set_jump_target(0, JumpTarget::Block(0)));
        assert!(!blocks.set_jump_target(0, JumpTarget::Block(9)));
        assert!(!blocks.set_jump_target(1, JumpTarget::Block(1)));
        assert_eq!(terminators(&blocks)[0], Terminator::Jump(JumpTarget::Unknown));
        assert!(blocks.set_jump_target(0, JumpTarget::Block(1)));
        assert_eq!(terminators(&blocks)[0], Terminator::Jump(JumpTarget::Block(1)));
    }

    #[test]
    fn reachability() {
        // 0: PUSH1 0x08, 1: JUMP, 3: JUMPDEST (dead), 4: STOP, 5: PUSH1 0x0a, 7: JUMPI,
        // 8: JUMPDEST, 9: STOP, 10: JUMPDEST, 11: STOP.
        let code = [
            op::PUSH1,
            0x08,
            op::JUMP,
            op::JUMPDEST,
            op::STOP,
            op::PUSH1,
            0x0a,
            op::JUMPI,
            op::JUMPDEST,
            op::STOP,
            op::JUMPDEST,
            op::STOP,
        ];
        let blocks = BasicBlocks::new(&code, SPEC);
        assert_eq!(starts(&blocks), [0, 3, 5, 8, 10]);
        assert_eq!(blocks.successors(0).collect::<Vec<_>>(), [3]);
        // Block 2 jumps to block 4 and falls through to block 3.
        assert_eq!(blocks.successors(2).collect::<Vec<_>>(), [4, 3]);
        assert_eq!(blocks.reachable(), [true, false, false, true, false]);
    }

    #[test]
    fn unknown_jump_reaches_every_jumpdest() {
        // 0: PUSH0, 1: MLOAD, 2: JUMP, 3: STOP, 4: JUMPDEST, 5: STOP, 6: ADD, 7: JUMPDEST, 8: STOP.
        let code = [
            op::PUSH0,
            op::MLOAD,
            op::JUMP,
            op::STOP,
            op::JUMPDEST,
            op::STOP,
            op::ADD,
            op::JUMPDEST,
            op::STOP,
        ];
        let mut blocks = BasicBlocks::new(&code, SPEC);
        assert_eq!(starts(&blocks), [0, 3, 4, 6, 7]);
        assert_eq!(blocks.reachable(), [true, false, true, false, true]);
        assert!(blocks.set_jump_target(0, JumpTarget::Block(4)));
        assert_eq!(blocks.reachable(), [true, false, false, false, true]);
        assert!(blocks.set_jump_target(0, JumpTarget::Invalid));
        assert_eq!(blocks.reachable(), [true, false, false, false, false]);
        assert!(blocks.set_jump_target(0, JumpTarget::Blocks([2, 4].into())));
        assert_eq!(blocks.reachable(), [true, false, true, false, true]);
    }

    #[test]
    fn unknown_jump_behind_dead_code_does_not_reach_jumpdests() {
        // The unknown JUMP is dead: after a STOP, no JUMPDEST.
        let code = [op::STOP, op::PUSH0, op::MLOAD, op::JUMP, op::JUMPDEST, op::STOP];
        let blocks = BasicBlocks::new(&code, SPEC);
        assert_eq!(blocks.reachable(), [true, false, false]);
    }

    #[test]
    fn lookup_by_pc() {
        let code = [op::PUSH2, 0x00, 0x04, op::STOP, op::JUMPDEST];
        let blocks = BasicBlocks::new(&code, SPEC);
        assert_eq!(blocks.block_at(0), Some(0));
        assert_eq!(blocks.block_at(2), Some(0));
        assert_eq!(blocks.block_at(3), Some(0));
        assert_eq!(blocks.block_at(4), Some(1));
        assert_eq!(blocks.block_at(5), None);
        assert_eq!(blocks.instruction_at(0).unwrap().opcode, op::PUSH2);
        assert_eq!(blocks.instruction_at(1), None);
        assert_eq!(blocks.instruction_at(3).unwrap().opcode, op::STOP);
        assert_eq!(blocks.instruction_at(5), None);
    }

    #[test]
    fn display() {
        // 0: PUSH1 0x0c, 2: JUMPI, 3: PUSH2 0x000d, 6: JUMP, 7: UNKNOWN, 8: JUMPDEST,
        // 9: PUSH0, a: MLOAD, b: JUMP, c: JUMPDEST, d: JUMPDEST, e: STOP.
        let code = [
            op::PUSH1,
            0x0c,
            op::JUMPI,
            op::PUSH2,
            0x00,
            0x0d,
            op::JUMP,
            0x0c,
            op::JUMPDEST,
            op::PUSH0,
            op::MLOAD,
            op::JUMP,
            op::JUMPDEST,
            op::JUMPDEST,
            op::STOP,
        ];
        let blocks = BasicBlocks::new(&code, SPEC);
        assert_eq!(
            blocks.to_string(),
            "\
bb0:
  0000  PUSH1 0x0c
  0002  JUMPI  ; -> bb4, else bb1
bb1:
  0003  PUSH2 0x000d
  0006  JUMP  ; -> bb5
bb2:
  0007  UNKNOWN(0x0c)
bb3:
  0008  JUMPDEST
  0009  PUSH0
  000a  MLOAD
  000b  JUMP  ; -> ?
bb4:
  000c  JUMPDEST
bb5:
  000d  JUMPDEST
  000e  STOP"
        );
    }

    #[test]
    fn display_names_opcodes_by_spec() {
        // DIFFICULTY, DUPN 0x80, EXCHANGE 0x8e, STOP.
        let code = [op::DIFFICULTY, op::DUPN, 0x80, op::EXCHANGE, 0x8e, op::STOP];
        assert_eq!(
            BasicBlocks::new(&code, SpecId::AMSTERDAM).to_string(),
            "\
bb0:
  0000  PREVRANDAO
  0001  DUPN 17
  0003  EXCHANGE 1, 2
  0005  STOP"
        );
        // Before Paris the opcode is DIFFICULTY, and DUPN and EXCHANGE are not enabled.
        assert_eq!(
            BasicBlocks::new(&code, SpecId::LONDON).to_string(),
            "\
bb0:
  0000  DIFFICULTY
  0001  DUPN
bb1:
  0002  DUP1
  0003  EXCHANGE
bb2:
  0004  DUP15
  0005  STOP"
        );
    }
}
