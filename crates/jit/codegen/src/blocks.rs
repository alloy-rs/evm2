//! Basic blocks with the jump targets that the compiler resolves.

use crate::bytecode::{Bytecode, InstFlags};
use evm2::{
    SpecId,
    bytecode::{BasicBlocks, JumpTarget},
};

/// Splits `code` into basic blocks and resolves jump targets like the compiler does.
///
/// [`BasicBlocks::new`] only resolves a `PUSH` directly before a `JUMP` or `JUMPI`. This also
/// runs the compiler's jump resolution, which interprets the stack of each block and follows it
/// across blocks, so it finds the targets of jumps through internal functions, jump tables and
/// the like. The result is the same as the one the compiler works with, and needs no backend.
///
/// A jump with [`JumpTarget::Invalid`] has no valid target, or is not reachable.
pub fn analyze_blocks(code: &[u8], spec_id: SpecId) -> BasicBlocks<'_> {
    let mut blocks = BasicBlocks::new(code, spec_id);
    let mut bytecode = Bytecode::new(code, spec_id, None, None);
    bytecode.analyze_jumps();

    // The blocks of `blocks` are the blocks of the compiler, up to the unreachable ones, so match
    // by program counter. A jump that has no counterpart keeps its target.
    for (inst, data) in bytecode.iter_all_insts() {
        if !data.is_static_jump() {
            continue;
        }
        let block_at = |inst| blocks.block_starting_at(bytecode.pc(inst) as usize);
        let target = if data.flags.contains(InstFlags::INVALID_JUMP) {
            Some(JumpTarget::Invalid)
        } else if data.flags.contains(InstFlags::MULTI_JUMP) {
            let targets = bytecode.multi_jump_targets(inst).unwrap_or_default();
            targets
                .iter()
                .map(|&target| block_at(target))
                .collect::<Option<_>>()
                .map(JumpTarget::Blocks)
        } else {
            let target = data.static_jump_target();
            // The compiler resolves a `JUMPI` with a constant false condition to the instruction
            // after it, which is no jump target.
            let false_condition =
                data.flags.contains(InstFlags::CONST_JUMP_CONDITION) && target == inst + 1;
            block_at(target).filter(|_| !false_condition).map(JumpTarget::Block)
        };
        // The decoders can disagree after an invalid `DUPN`, `SWAPN` or `EXCHANGE` immediate, but
        // only about code that does not run.
        let jump_pc = bytecode.pc(inst) as usize;
        if let Some(target) = target
            && let Some(block) = blocks.block_at(jump_pc)
            && blocks.block_instructions(block).last().is_some_and(|last| last.pc == jump_pc)
        {
            blocks.set_jump_target(block, target);
        }
    }
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_asm;
    use evm2::{bytecode::Terminator, interpreter::op};

    fn targets(blocks: &BasicBlocks<'_>) -> Vec<Terminator> {
        blocks.blocks().iter().map(|block| block.terminator().clone()).collect()
    }

    #[test]
    fn internal_function_returns_to_both_call_sites() {
        let code = parse_asm(
            "
            PUSH %ret1
            PUSH %func
            JUMP
        ret1:
            JUMPDEST
            POP
            PUSH %ret2
            PUSH %func
            JUMP
        ret2:
            JUMPDEST
            POP
            STOP
        func:
            JUMPDEST
            PUSH1 0x42
            SWAP1
            JUMP
            ",
        )
        .unwrap();

        // The adjacent `PUSH` analysis cannot resolve the return jump.
        let adjacent = BasicBlocks::new(&code, SpecId::OSAKA);
        let ret = adjacent.blocks().len() - 1;
        assert_eq!(adjacent.blocks()[ret].terminator(), &Terminator::Jump(JumpTarget::Unknown));
        assert!(adjacent.reachable().iter().all(|&reachable| reachable));

        let blocks = analyze_blocks(&code, SpecId::OSAKA);
        // Blocks: call 1, ret1, ret2, func.
        assert_eq!(blocks.blocks().len(), 4);
        assert_eq!(
            targets(&blocks),
            [
                Terminator::Jump(JumpTarget::Block(3)),
                Terminator::Jump(JumpTarget::Block(3)),
                Terminator::Halt,
                Terminator::Jump(JumpTarget::Blocks([1, 2].into())),
            ]
        );
        assert_eq!(blocks.reachable(), [true; 4]);
        assert_eq!(
            blocks.to_string(),
            "\
bb0:
  0000  PUSH1 0x05
  0002  PUSH1 0x0f
  0004  JUMP  ; -> bb3
bb1:
  0005  JUMPDEST
  0006  POP
  0007  PUSH1 0x0c
  0009  PUSH1 0x0f
  000b  JUMP  ; -> bb3
bb2:
  000c  JUMPDEST
  000d  POP
  000e  STOP
bb3:
  000f  JUMPDEST
  0010  PUSH1 0x42
  0012  SWAP1
  0013  JUMP  ; -> bb1, bb2"
        );
    }

    #[test]
    fn jumpi_with_false_condition_keeps_its_target() {
        // PUSH0 PUSH1 6 JUMPI JUMPDEST STOP: the target 6 is beyond the code.
        let code = [op::PUSH0, op::PUSH1, 6, op::JUMPI, op::JUMPDEST, op::STOP];
        let blocks = analyze_blocks(&code, SpecId::OSAKA);
        assert_eq!(targets(&blocks), [Terminator::JumpI(JumpTarget::Invalid), Terminator::Halt]);
    }
}
