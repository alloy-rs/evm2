//! Opcode fusion.
//!
//! Fusion translates legacy bytecode into a buffer with the same layout as the padded original,
//! where the first byte of a common opcode sequence becomes a synthetic opcode. Program counters,
//! jump destinations, and immediates are unchanged, so the interpreter runs the translated buffer
//! with the same dispatch loop and a table that adds handlers for the synthetic opcodes.
//!
//! Synthetic opcodes reuse bytes that no specification defines. A fused handler mirrors the stack
//! checks of its sequence in order, and dispatch runs only the first opcode of the sequence when
//! the combined static gas is not available, so every halt reason matches unfused execution. The
//! stack and remaining gas of a frame that halts exceptionally may differ, since they are
//! discarded.
//!
//! Translation costs a pass over the whole bytecode, while fusion saves time per executed
//! instruction, so bytecode is translated only after frames have spent enough gas running it (see
//! [`Bytecode::fused_bytes`]). Inspected execution always runs the original bytecode.
//!
//! [`Bytecode::fused_bytes`]: crate::bytecode::Bytecode::fused_bytes

use crate::{
    EvmConfig, EvmTypesHost, OpcodeConfig,
    bytecode::JumpTable,
    constants::STACK_LIMIT,
    interpreter::{InstrStop, Pc, StackMut, Word, op},
    utils::WordExt,
};
use alloc::boxed::Box;
use core::hint::cold_path;
use evm2_macros::instruction;

/// Placeholder for original instructions that collide with a synthetic opcode. It is an unknown
/// opcode, like the byte it replaces.
const UNKNOWN: u8 = 0xB0;
/// `PUSH2 target JUMP` to a valid `JUMPDEST`, which is consumed too.
pub(crate) const PUSH2_JUMP: u8 = 0xB1;
/// `PUSH2 target JUMPI` to a valid `JUMPDEST`.
pub(crate) const PUSH2_JUMPI: u8 = 0xB2;
/// `ISZERO PUSH2 target JUMPI` to a valid `JUMPDEST`.
pub(crate) const ISZERO_PUSH2_JUMPI: u8 = 0xB3;
/// `PUSH1 target JUMP` to a valid `JUMPDEST`, which is consumed too.
pub(crate) const PUSH1_JUMP: u8 = 0xB4;
/// `PUSH1 target JUMPI` to a valid `JUMPDEST`.
pub(crate) const PUSH1_JUMPI: u8 = 0xB5;
/// `POP POP`.
pub(crate) const POP_POP: u8 = 0xB6;
/// `SWAP1 POP`.
pub(crate) const SWAP1_POP: u8 = 0xB7;
/// `SWAP2 POP`.
pub(crate) const SWAP2_POP: u8 = 0xB8;
/// `PUSH1 x ADD`.
pub(crate) const PUSH1_ADD: u8 = 0xB9;

/// First synthetic opcode.
pub(crate) const FIRST: u8 = PUSH2_JUMP;
/// Last synthetic opcode.
pub(crate) const LAST: u8 = PUSH1_ADD;

/// Returns the opcodes of the sequence that synthetic opcode `fused` replaces.
const fn sequence(fused: u8) -> &'static [u8] {
    match fused {
        PUSH2_JUMP => &[op::PUSH2, op::JUMP, op::JUMPDEST],
        PUSH2_JUMPI => &[op::PUSH2, op::JUMPI],
        ISZERO_PUSH2_JUMPI => &[op::ISZERO, op::PUSH2, op::JUMPI],
        PUSH1_JUMP => &[op::PUSH1, op::JUMP, op::JUMPDEST],
        PUSH1_JUMPI => &[op::PUSH1, op::JUMPI],
        POP_POP => &[op::POP, op::POP],
        SWAP1_POP => &[op::SWAP1, op::POP],
        SWAP2_POP => &[op::SWAP2, op::POP],
        PUSH1_ADD => &[op::PUSH1, op::ADD],
        _ => &[],
    }
}

/// Returns the first opcode of the sequence that `opcode` replaces, if it is synthetic.
#[inline(always)]
pub(crate) const fn unfused_op(opcode: u8) -> Option<u8> {
    match sequence(opcode) {
        [first, ..] => Some(*first),
        [] => None,
    }
}

/// Returns whether config `C` can run fused bytecode.
///
/// Translation assumes the base instructions and their immediates, so every opcode must keep its
/// base implementation and static gas, and every reserved byte must be unknown.
pub(crate) const fn is_enabled<T: EvmTypesHost, C: EvmConfig<T>>() -> bool {
    let config = C::OPCODE_CONFIG;
    let base = &OpcodeConfig::<T>::base::<C>();
    let mut opcode = 0;
    loop {
        if config.revision(opcode) != base.revision(opcode)
            || (matches!(opcode, UNKNOWN..=LAST) && !config.is_unknown_opcode(opcode))
        {
            return false;
        }
        if opcode == u8::MAX {
            return true;
        }
        opcode += 1;
    }
}

/// Returns the opcode config of `C` extended with the synthetic opcodes.
pub(crate) const fn opcode_config<T: EvmTypesHost, C: EvmConfig<T>>() -> OpcodeConfig<T> {
    let mut config = C::OPCODE_CONFIG.const_clone();
    macro_rules! set {
        ($($opcode:ident => $instr:ty,)*) => {$(
            config.set_instruction::<$instr>($opcode, static_gas::<T, C>($opcode));
        )*};
    }
    set! {
        PUSH2_JUMP => push_jump<T, 2>,
        PUSH2_JUMPI => push_jumpi<T, 2>,
        ISZERO_PUSH2_JUMPI => iszero_push2_jumpi<T>,
        PUSH1_JUMP => push_jump<T, 1>,
        PUSH1_JUMPI => push_jumpi<T, 1>,
        POP_POP => pop_pop<T>,
        SWAP1_POP => swap_pop<T, 1>,
        SWAP2_POP => swap_pop<T, 2>,
        PUSH1_ADD => push1_add<T>,
    }
    config
}

/// Returns the combined static gas of the sequence that synthetic opcode `fused` replaces.
const fn static_gas<T: EvmTypesHost, C: EvmConfig<T>>(fused: u8) -> u16 {
    let sequence = sequence(fused);
    let mut gas = 0;
    let mut i = 0;
    while i < sequence.len() {
        gas += C::OPCODE_CONFIG.static_gas(sequence[i]);
        i += 1;
    }
    gas
}

/// Translates legacy bytecode into its fused form.
pub(crate) fn fuse(code: &[u8], jump_table: &JumpTable) -> Box<[u8]> {
    let mut out = Box::<[u8]>::from(code);
    let is_jumpdest = |target| jump_table.is_valid(target) && code[target] == op::JUMPDEST;
    // Patterns read up to 4 bytes past their first opcode.
    let end = code.len().saturating_sub(4);
    let mut i = 0;
    while i < end {
        let opcode = code[i];
        let (fused, len) = match opcode {
            op::PUSH1 => match code[i + 2] {
                op::JUMP if is_jumpdest(code[i + 1] as usize) => (PUSH1_JUMP, 3),
                op::JUMPI if is_jumpdest(code[i + 1] as usize) => (PUSH1_JUMPI, 3),
                op::ADD => (PUSH1_ADD, 3),
                _ => (opcode, 2),
            },
            op::PUSH2 => {
                let target = read_u16(&code[i + 1..]);
                match code[i + 3] {
                    op::JUMP if is_jumpdest(target) => (PUSH2_JUMP, 4),
                    op::JUMPI if is_jumpdest(target) => (PUSH2_JUMPI, 4),
                    _ => (opcode, 3),
                }
            }
            op::ISZERO
                if code[i + 1] == op::PUSH2
                    && code[i + 4] == op::JUMPI
                    && is_jumpdest(read_u16(&code[i + 2..])) =>
            {
                (ISZERO_PUSH2_JUMPI, 5)
            }
            op::POP if code[i + 1] == op::POP => (POP_POP, 2),
            op::SWAP1 if code[i + 1] == op::POP => (SWAP1_POP, 2),
            op::SWAP2 if code[i + 1] == op::POP => (SWAP2_POP, 2),
            UNKNOWN..=LAST => (UNKNOWN, 1),
            // The next byte is an EIP-8024 immediate, which jump analysis treats as an
            // instruction. Before Amsterdam it can only be reached as a jump destination, which
            // is never rewritten.
            op::DUPN | op::SWAPN | op::EXCHANGE => (opcode, 1 + instruction_len(code[i + 1])),
            _ => (opcode, instruction_len(opcode)),
        };
        out[i] = fused;
        i += len;
    }
    // The tail has no room for patterns.
    while i < code.len() {
        let opcode = code[i];
        i += match opcode {
            UNKNOWN..=LAST => {
                out[i] = UNKNOWN;
                1
            }
            op::DUPN | op::SWAPN | op::EXCHANGE => {
                1 + code.get(i + 1).map_or(1, |&next| instruction_len(next))
            }
            _ => instruction_len(opcode),
        };
    }
    out
}

const fn instruction_len(opcode: u8) -> usize {
    match opcode {
        op::PUSH1..=op::PUSH32 => (opcode - op::PUSH1) as usize + 2,
        _ => 1,
    }
}

/// Checks that the stack holds `min..=max` words, failing like the unfused sequence otherwise.
///
/// A sequence underflows below `min` and overflows above `max`, and no length does both, so the
/// order of the sequence's own checks does not matter.
#[inline(always)]
const fn check_len(stack: &StackMut<'_>, min: usize, max: usize) -> crate::interpreter::Result {
    let len = stack.len();
    if len.wrapping_sub(min) > max - min {
        cold_path();
        return Err(if len < min { InstrStop::StackUnderflow } else { InstrStop::StackOverflow });
    }
    Ok(())
}

#[inline(always)]
const fn read_u16(bytes: &[u8]) -> usize {
    u16::from_be_bytes([bytes[0], bytes[1]]) as usize
}

/// Reads the big-endian `PUSH<N>` immediate at `pc + offset` as a jump target.
#[inline(always)]
fn read_target<const N: usize>(pc: &Pc, offset: usize) -> usize {
    let bytes = unsafe { pc.read_bytes_offset_unchecked(offset, N) };
    bytes.iter().fold(0, |target, &byte| (target << 8) | byte as usize)
}

#[instruction(no_stack_preamble)]
fn push_jump<const N: usize>(cx: _) -> Result {
    check_len(&stack, 0, STACK_LIMIT - 1)?;
    let target = read_target::<N>(cx.pc, 1);
    debug_assert!(cx.state.bytecode().is_valid_jumpdest(target));
    unsafe { cx.pc.set_unchecked(cx.state.bytecode(), target + 1) };
    Ok(())
}

#[instruction(no_stack_preamble)]
fn push_jumpi<const N: usize>(cx: _) -> Result {
    check_len(&stack, 1, STACK_LIMIT - 1)?;
    if unsafe { stack.top_unchecked() }.is_zero() {
        unsafe { cx.pc.advance_unchecked(N + 2) };
    } else {
        let target = read_target::<N>(cx.pc, 1);
        debug_assert!(cx.state.bytecode().is_valid_jumpdest(target));
        unsafe { cx.pc.set_unchecked(cx.state.bytecode(), target) };
    }
    unsafe { stack.pop_unchecked() };
    Ok(())
}

#[instruction(no_stack_preamble)]
fn iszero_push2_jumpi(cx: _) -> Result {
    check_len(&stack, 1, STACK_LIMIT - 1)?;
    if unsafe { stack.top_unchecked() }.is_zero() {
        let target = read_target::<2>(cx.pc, 2);
        debug_assert!(cx.state.bytecode().is_valid_jumpdest(target));
        unsafe { cx.pc.set_unchecked(cx.state.bytecode(), target) };
    } else {
        unsafe { cx.pc.advance_unchecked(5) };
    }
    unsafe { stack.pop_unchecked() };
    Ok(())
}

#[instruction(no_stack_preamble)]
fn pop_pop(cx: _) -> Result {
    stack.popn::<2>()?;
    unsafe { cx.pc.advance_unchecked(2) };
    Ok(())
}

#[instruction(no_stack_preamble)]
fn swap_pop<const N: usize>(cx: _) -> Result {
    check_len(&stack, N + 1, STACK_LIMIT)?;
    let top = unsafe { stack.pop_unchecked() };
    let len = stack.len();
    stack.as_slice_mut()[len - N] = top;
    unsafe { cx.pc.advance_unchecked(2) };
    Ok(())
}

#[instruction(no_stack_preamble)]
fn push1_add(cx: _) -> Result {
    check_len(&stack, 1, STACK_LIMIT - 1)?;
    let x = unsafe { cx.pc.read_bytes_offset_unchecked(1, 1)[0] };
    let top = unsafe { stack.top_unchecked() };
    let sum = top.wrapping_add(Word::from(x));
    top.write(sum);
    unsafe { cx.pc.advance_unchecked(3) };
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BaseEvmConfigSelector, ExecutionConfig, SpecId,
        env::TxEnvExt,
        interpreter::{Interpreter, Message, MessageExt},
        test_utils::{RunConfig, TestHost, TestInterpreter, TestTypes, legacy_bytecode, run},
    };
    use alloc::{format, vec::Vec};

    fn run_both(code: &[u8], spec: SpecId, gas_limit: u64) -> (TestInterpreter, TestInterpreter) {
        let config = || RunConfig::new(code).spec(spec).gas_limit(gas_limit);
        (run(config()), run(config().inspect()))
    }

    /// Checks that fused and unfused execution agree on every gas limit around the full cost.
    fn assert_same(code: &[u8], spec: SpecId) {
        let used = {
            let (_, unfused) = run_both(code, spec, 100_000);
            100_000 - unfused.gas_remaining()
        };
        let low = if used <= 100 { 0 } else { used - 40 };
        for gas_limit in (0..8).chain(low..=used + 1) {
            let (fused, unfused) = run_both(code, spec, gas_limit);
            let msg = format!("code={code:02x?} spec={spec:?} gas_limit={gas_limit}");
            assert_eq!(fused.err, unfused.err, "{msg}");
            // An exceptional halt discards the frame's stack and remaining gas.
            if fused.err == InstrStop::Stop {
                assert_eq!(fused.gas_remaining(), unfused.gas_remaining(), "{msg}");
                assert_eq!(fused.stack(), unfused.stack(), "{msg}");
            }
        }
    }

    fn with_stack(n: usize, tail: &[u8]) -> Vec<u8> {
        let mut code = Vec::from_iter((0..n).flat_map(|i| [op::PUSH1, i as u8]));
        code.extend_from_slice(tail);
        code
    }

    #[test]
    fn enabled_for_base_specs() {
        for spec in core::iter::successors(Some(SpecId::MIN), |spec| spec.next()) {
            let config = ExecutionConfig::<TestTypes>::for_base_spec::<BaseEvmConfigSelector>(spec);
            assert!(config.fused_instructions.is_some(), "{spec:?}");
        }
    }

    #[test]
    fn fused_matches_unfused() {
        let jumps = |push: &[u8], jump: u8, target: u8| {
            let mut tail = Vec::from(push);
            tail.extend([target, jump, op::PUSH1, 0xAA, op::STOP, op::JUMPDEST, op::PUSH1, 0xBB]);
            tail
        };
        let mut tails = Vec::new();
        for (push, len) in [(&[op::PUSH2, 0][..], 4), (&[op::PUSH1][..], 3)] {
            for jump in [op::JUMP, op::JUMPI] {
                // `target` is the JUMPDEST; `target - 1` is a STOP, which is not fused.
                for target in [len + 3, len + 2] {
                    tails.push(jumps(push, jump, target as u8));
                }
            }
        }
        let iszero = |target: u8| {
            let code = [op::ISZERO, op::PUSH2, 0, target, op::JUMPI, op::STOP, op::JUMPDEST];
            Vec::from(code)
        };
        tails.push(iszero(6));
        tails.push(iszero(5));
        tails.push(Vec::from([op::POP, op::POP]));
        tails.push(Vec::from([op::SWAP1, op::POP]));
        tails.push(Vec::from([op::SWAP2, op::POP]));
        tails.push(Vec::from([op::PUSH1, 7, op::ADD]));
        tails.push(
            [op::PUSH32].into_iter().chain([0xFF; 32]).chain([op::PUSH1, 1, op::ADD]).collect(),
        );
        // Bytes that collide with synthetic opcodes, as instructions and as immediates.
        tails.push(Vec::from([op::PUSH1, PUSH2_JUMP, op::PUSH1, UNKNOWN, op::ADD]));
        tails.push(Vec::from([PUSH2_JUMP]));
        tails.push(Vec::from([UNKNOWN]));
        for tail in &tails {
            for stack in [0, 1, 2, 3, 1023, 1024] {
                assert_same(&with_stack(stack, tail), SpecId::OSAKA);
            }
        }
    }

    #[test]
    fn eip8024_immediates_are_not_fused() {
        for spec in [SpecId::OSAKA, SpecId::AMSTERDAM] {
            for imm in [op::POP, op::SWAP1, UNKNOWN, PUSH2_JUMP, PUSH1_ADD, 0x80] {
                for opcode in [op::DUPN, op::SWAPN, op::EXCHANGE] {
                    let tail = [opcode, imm, op::POP, op::PUSH1, 1, op::ADD, op::STOP];
                    assert_same(&with_stack(70, &tail), spec);
                }
            }
        }
    }

    #[test]
    fn entry_inside_immediate_is_not_fused() {
        let code = [op::PUSH3, PUSH2_JUMP, 0xFF, 0xFF, op::STOP];
        let message = Message::<TestTypes> {
            code: legacy_bytecode(code),
            gas_limit: 10_000,
            ..MessageExt::default()
        };
        let tx_env = TxEnvExt::default();
        let mut interpreter = Interpreter::<TestTypes>::new(&tx_env, &message);
        interpreter.set_pc(1);
        let mut host = TestHost {
            execution_config: ExecutionConfig::for_base_spec::<BaseEvmConfigSelector>(
                SpecId::OSAKA,
            ),
            ..TestHost::default()
        };
        assert_eq!(interpreter.run(&mut host), Ok(InstrStop::OpcodeNotFound));
    }
}
