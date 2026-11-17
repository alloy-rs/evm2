//! Superinstructions: the sequences of [`evm2_sx::PATTERNS`] as instructions.
//!
//! A superinstruction runs its sequence with the stack effect and errors of the base instructions
//! it replaces. The dispatch charges the sequence's summed static gas up front; memory expansion is
//! charged where the sequence's `MLOAD` or `MSTORE` runs. Stack bounds are checked once, for the
//! deepest word the sequence reads and the highest it grows to. Charging and checking up front only
//! changes which exceptional halt a failing sequence ends with, and each consumes all gas.
//!
//! Translated code keeps its layout, so a superinstruction reads its immediates where they are and
//! advances the program counter past the whole sequence, or jumps.

use super::{
    InstrStop, InterpreterState, Pc, Result, StackMut, Word,
    i256::i256_lt,
    private::{Instruction, InstructionFlags, split_gas_state},
};
use crate::{
    EvmConfig, EvmTypesHost, OpcodeConfig, SpecId, constants::STACK_LIMIT, utils::WordExt,
};
use core::{hint::cold_path, marker::PhantomData};
use evm2_sx::{OpSet, Ops, PATTERNS, Sx, op};

const _: () = assert!(PATTERNS.len() == 43, "update `for_each_pattern!`");

macro_rules! for_each_pattern {
    ($m:ident) => {
        $m!(
            0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23
            24 25 26 27 28 29 30 31 32 33 34 35 36 37 38 39 40 41 42
        )
    };
}

/// Configuration `C` with the superinstructions that apply to it.
pub(crate) struct SxConfig<C>(PhantomData<fn() -> C>);

impl<T: EvmTypesHost, C: EvmConfig<T>> EvmConfig<T> for SxConfig<C> {
    const BASE_SPEC_ID: SpecId = C::BASE_SPEC_ID;
    const OPCODE_CONFIG: &'static OpcodeConfig<T> = &opcode_config::<T, C>();
}

/// Returns the opcodes of `C` as translation sees them: an opcode is fusable when it is defined,
/// is the base instruction of `C`'s spec and needs no hooks.
pub(crate) const fn ops<T: EvmTypesHost, C: EvmConfig<T>>() -> Ops {
    let config = C::OPCODE_CONFIG;
    let base = OpcodeConfig::<T>::base::<C>();
    let mut ops = Ops { fusable: OpSet::EMPTY, free: OpSet::EMPTY };
    let mut i = 0;
    while i < 256 {
        let op = i as u8;
        if config.is_unknown_opcode(op) {
            ops.free.insert(op);
        } else if !config.is_overridden(&base, op)
            && !config.instruction(op).flags.contains(InstructionFlags::HOOKED)
        {
            ops.fusable.insert(op);
        }
        i += 1;
    }
    ops
}

/// Returns `C`'s opcode config with an instruction for the slot of each superinstruction that
/// applies, charging the static gas of the sequence it runs.
const fn opcode_config<T: EvmTypesHost, C: EvmConfig<T>>() -> OpcodeConfig<T> {
    let ops = ops::<T, C>();
    let mut config = C::OPCODE_CONFIG.copied();
    macro_rules! set {
        ($($i:literal)*) => {$(
            let sx = PATTERNS[$i];
            if ops.is_active(sx) {
                let mut gas = 0;
                let mut j = 0;
                while j < sx.len() {
                    gas += C::OPCODE_CONFIG.static_gas(sx.op(j));
                    j += 1;
                }
                config.set_instruction::<SxInstr<$i>>(sx.slot, gas);
            }
        )*};
    }
    for_each_pattern!(set);
    config
}

/// Superinstruction `I` of [`PATTERNS`].
pub(crate) struct SxInstr<const I: usize>;

impl<T: EvmTypesHost, const I: usize> Instruction<T> for SxInstr<I> {
    const FLAGS: InstructionFlags = flags(PATTERNS[I]);

    #[inline(always)]
    fn execute(
        pc: &mut Pc,
        stack: StackMut<'_>,
        state: &mut InterpreterState<'_, '_, T>,
    ) -> Result {
        run::<T, I>(pc, stack, state)
    }
}

const fn flags(sx: Sx) -> InstructionFlags {
    let mut flags = InstructionFlags::SETS_PC;
    let mut i = 0;
    while i < sx.len() {
        // MLOAD and MSTORE charge memory expansion; a jump charges the JUMPDEST it skips.
        if matches!(sx.op(i), op::MLOAD | op::MSTORE | op::JUMP | op::JUMPI) {
            flags = flags.union(InstructionFlags::DYNAMIC_GAS);
        }
        i += 1;
    }
    flags
}

/// No value.
const NONE: u8 = u8::MAX;

/// Values a plan may hold: the input words, then one per opcode that produces a word.
const VALS: usize = 16;

/// One opcode of a plan: the values it reads (`a` the top) and writes, and the code offset of a
/// PUSH immediate.
#[derive(Clone, Copy)]
struct Step {
    a: u8,
    b: u8,
    out: u8,
    imm: u8,
}

/// A superinstruction compiled to straight-line code over numbered values.
///
/// The stack words it reads are values `0..input`, value `k` being the word `k + 1` deep. DUP, SWAP
/// and POP only move value numbers, so they cost nothing at run time; every other opcode makes a new
/// value. When the sequence ends, `out` holds the values of the stack slots from `input` deep up, and
/// only slots whose value changed are written.
#[derive(Clone, Copy)]
struct Plan {
    input: usize,
    steps: [Step; 8],
    out: [u8; VALS],
    out_len: usize,
    /// The target and condition of a final JUMP or JUMPI.
    jump: u8,
    cond: u8,
}

const fn plan(sx: Sx) -> Plan {
    let (input, growth) = sx.stack_bounds();
    let offsets = evm2_sx::offsets(sx);
    assert!(input + growth <= VALS);
    let mut stack = [NONE; VALS];
    let mut len = 0;
    while len < input {
        stack[len] = (input - 1 - len) as u8;
        len += 1;
    }
    let mut vals = input;
    let none = Step { a: NONE, b: NONE, out: NONE, imm: 0 };
    let mut p = Plan { input, steps: [none; 8], out: [NONE; VALS], out_len: 0, jump: NONE, cond: NONE };
    let mut i = 0;
    while i < sx.len() {
        let op = sx.op(i);
        let mut s = none;
        match op {
            op::JUMPDEST => {}
            op::PUSH0..=op::PUSH32 => {
                s.out = vals as u8;
                s.imm = offsets[i] as u8 + 1;
                stack[len] = s.out;
                len += 1;
                vals += 1;
            }
            op::POP => len -= 1,
            op::DUP1..=op::DUP4 => {
                stack[len] = stack[len - (op - op::DUP1) as usize - 1];
                len += 1;
            }
            op::SWAP1..=op::SWAP4 => {
                let other = len - (op - op::SWAP1) as usize - 2;
                let top = stack[len - 1];
                stack[len - 1] = stack[other];
                stack[other] = top;
            }
            op::ISZERO | op::MLOAD => {
                s.a = stack[len - 1];
                s.out = vals as u8;
                stack[len - 1] = s.out;
                vals += 1;
            }
            op::MSTORE => {
                s.a = stack[len - 1];
                s.b = stack[len - 2];
                len -= 2;
            }
            op::JUMP => {
                p.jump = stack[len - 1];
                len -= 1;
            }
            op::JUMPI => {
                p.jump = stack[len - 1];
                p.cond = stack[len - 2];
                len -= 2;
            }
            _ => {
                s.a = stack[len - 1];
                s.b = stack[len - 2];
                s.out = vals as u8;
                stack[len - 2] = s.out;
                len -= 1;
                vals += 1;
            }
        }
        p.steps[i] = s;
        i += 1;
    }
    assert!(vals <= VALS);
    p.out = stack;
    p.out_len = len;
    p
}

/// Reads the `n`-byte big-endian immediate at `p`. Code is padded past its end, so reading up to
/// 32 bytes past an instruction stays in bounds.
#[inline(always)]
unsafe fn immediate(p: *const u8, n: usize) -> Word {
    if n == 0 {
        return Word::ZERO;
    }
    let be = |p: *const u8| u64::from_be_bytes(unsafe { p.cast::<[u8; 8]>().read_unaligned() });
    if n <= 8 {
        return Word::from(be(p) >> (8 * (8 - n)));
    }
    let mut limbs = [0; 4];
    let mut rest = n;
    let mut i = 0;
    while rest >= 8 {
        limbs[i] = be(unsafe { p.add(rest - 8) });
        rest -= 8;
        i += 1;
    }
    if rest > 0 {
        limbs[i] = be(p) >> (8 * (8 - rest));
    }
    Word::from_limbs(limbs)
}

/// A hint that keeps a branch a branch; nightly only, where the tail-call backend needs it anyway.
#[cfg(tco)]
use core::hint::likely;
#[cfg(not(tco))]
#[inline(always)]
const fn likely(b: bool) -> bool {
    b
}

#[inline(always)]
fn word(b: bool) -> Word {
    Word::from(b as u64)
}

// Scalar limb arithmetic, so values stay in general registers: ruint's comparisons load words as
// vectors, which keeps them in memory.
#[inline(always)]
fn is_zero(w: &Word) -> bool {
    let l = w.as_limbs();
    (l[0] | l[1] | l[2] | l[3]) == 0
}

#[inline(always)]
fn eq(a: &Word, b: &Word) -> bool {
    let (a, b) = (a.as_limbs(), b.as_limbs());
    ((a[0] ^ b[0]) | (a[1] ^ b[1]) | (a[2] ^ b[2]) | (a[3] ^ b[3])) == 0
}

/// Returns whether `a < b`.
#[inline(always)]
fn lt(a: &Word, b: &Word) -> bool {
    let (a, b) = (a.as_limbs(), b.as_limbs());
    // Subtract with borrow; the final borrow is the answer.
    let (_, c0) = a[0].overflowing_sub(b[0]);
    let (_, c1) = a[1].borrowing_sub(b[1], c0);
    let (_, c2) = a[2].borrowing_sub(b[2], c1);
    let (_, c3) = a[3].borrowing_sub(b[3], c2);
    c3
}

/// Static gas of the JUMPDEST a jump skips; 1 in every spec.
const JUMPDEST_GAS: u64 = 1;

/// Runs superinstruction `I`.
#[inline(always)]
fn run<T: EvmTypesHost, const I: usize>(
    pc: &mut Pc,
    stack: StackMut<'_>,
    state: &mut InterpreterState<'_, '_, T>,
) -> Result {
    let input = const { plan(PATTERNS[I]).input };
    let growth = const { PATTERNS[I].stack_bounds().1 };
    let out_len = const { plan(PATTERNS[I]).out_len };
    let len = const { PATTERNS[I].len() };
    let span = const { evm2_sx::offsets(PATTERNS[I])[PATTERNS[I].len()] };

    // SAFETY: Words below the length are initialized, and the checks keep every access in bounds.
    let (base, stack_len) = unsafe { stack.into_raw_parts() };
    let height = *stack_len;
    if height < input {
        cold_path();
        return Err(InstrStop::StackUnderflow);
    }
    if height + growth > STACK_LIMIT {
        cold_path();
        return Err(InstrStop::StackOverflow);
    }
    // The stack slots the sequence reads and writes, from `input` deep.
    let window = unsafe { base.add(height - input) };
    let code = pc.as_ptr();
    let mut v = [Word::ZERO; VALS];

    macro_rules! inputs {
        ($($k:literal)*) => {$(
            if $k < input {
                // SAFETY: The value is within the stack's length.
                v[$k] = unsafe { window.add(input - 1 - $k).read() };
            }
        )*};
    }
    inputs!(0 1 2 3 4 5 6 7);

    macro_rules! steps {
        ($($i:literal)*) => {$(
            if $i < len {
                let op = const { PATTERNS[I].op($i) };
                let a = const { plan(PATTERNS[I]).steps[$i].a as usize % VALS };
                let b = const { plan(PATTERNS[I]).steps[$i].b as usize % VALS };
                let out = const { plan(PATTERNS[I]).steps[$i].out as usize % VALS };
                let imm = const { plan(PATTERNS[I]).steps[$i].imm as usize };
                match op {
                    op::PUSH0..=op::PUSH32 => {
                        // SAFETY: The immediate is in the padded code.
                        v[out] = unsafe { immediate(code.add(imm), (op - op::PUSH0) as usize) };
                    }
                    op::ISZERO => v[out] = word(is_zero(&v[a])),
                    op::ADD => v[out] = v[a].wrapping_add(v[b]),
                    op::MUL => v[out] = v[a].wrapping_mul(v[b]),
                    op::SUB => v[out] = v[a].wrapping_sub(v[b]),
                    op::LT => v[out] = word(lt(&v[a], &v[b])),
                    op::GT => v[out] = word(lt(&v[b], &v[a])),
                    op::SGT => v[out] = word(i256_lt(&v[b], &v[a])),
                    op::EQ => v[out] = word(eq(&v[a], &v[b])),
                    op::AND => v[out] = v[a] & v[b],
                    op::SHL => {
                        let shift = v[a].to_usize_saturated();
                        v[out] = if shift < 256 { v[b] << shift } else { Word::ZERO };
                    }
                    op::SHR => {
                        let shift = v[a].to_usize_saturated();
                        v[out] = if shift < 256 { v[b] >> shift } else { Word::ZERO };
                    }
                    op::MLOAD => {
                        let offset = v[a].to_usize()?;
                        // SAFETY: `state` is valid for the call.
                        let (gas, state) = unsafe { split_gas_state(&raw mut *state) };
                        state.resize_memory(gas, offset, 32)?;
                        v[out] = state.memory().get_word(offset);
                    }
                    op::MSTORE => {
                        let offset = v[a].to_usize()?;
                        // SAFETY: `state` is valid for the call.
                        let (gas, state) = unsafe { split_gas_state(&raw mut *state) };
                        state.resize_memory(gas, offset, 32)?;
                        state.memory().set(offset, &v[b].to_be_bytes::<32>());
                    }
                    _ => {}
                }
            }
        )*};
    }
    steps!(0 1 2 3 4 5 6 7);

    macro_rules! outputs {
        ($($k:literal)*) => {$(
            if $k < out_len {
                let id = const { plan(PATTERNS[I]).out[$k] as usize % VALS };
                // A slot keeps its word when it holds the input that was there.
                if !(id < input && input - 1 - id == $k) {
                    // SAFETY: The slot is below the checked limit.
                    unsafe { window.add($k).write(v[id]) };
                }
            }
        )*};
    }
    outputs!(0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15);
    *stack_len = height - input + out_len;

    let jump = const { plan(PATTERNS[I]).jump };
    let cond = const { plan(PATTERNS[I]).cond };
    let taken = jump != NONE && (cond == NONE || !is_zero(&v[cond as usize % VALS]));
    // A branch rather than a select: a predicted branch lets the next dispatch start before the
    // condition is known.
    if likely(taken) {
        let bytecode = state.bytecode();
        let target = match const { PATTERNS[I].static_jump() } {
            Some(n) if n <= 8 => {
                // Translation fused this jump only because its constant target is a jump
                // destination.
                let at = const { evm2_sx::offsets(PATTERNS[I])[PATTERNS[I].len() - 2] + 1 };
                // SAFETY: The immediate is in the padded code.
                (unsafe {
                    u64::from_be_bytes(code.add(at).cast::<[u8; 8]>().read_unaligned())
                        >> (8 * (8 - n))
                }) as usize
            }
            _ => {
                let target = v[jump as usize % VALS].to_usize_saturated();
                if !bytecode.is_valid_jumpdest(target) {
                    cold_path();
                    return Err(InstrStop::InvalidJump);
                }
                target
            }
        };
        // Run the JUMPDEST the jump lands on here, unless translation fused it with what follows:
        // charge its gas and continue after it, saving its dispatch.
        // SAFETY: `target` is a jump destination, so it and the byte after it are in the code.
        let lands_on_jumpdest = unsafe { *bytecode.as_ptr().add(target) } == op::JUMPDEST;
        let skip = lands_on_jumpdest as usize;
        // SAFETY: `state` is valid for the call.
        let (gas, state) = unsafe { split_gas_state(&raw mut *state) };
        gas.spend(JUMPDEST_GAS * skip as u64)?;
        // SAFETY: `target` is a jump destination; past a JUMPDEST is the next instruction.
        unsafe { pc.set_unchecked(state.bytecode(), target + skip) };
    } else {
        // SAFETY: The sequence is in the code.
        unsafe { pc.advance_unchecked(span) };
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::{
        BaseEvmConfigSelector, ExecutionConfig, SpecId,
        interpreter::{InstrStop, Interpreter, Message, Word},
        test_utils::{TestHost, TestTypes, legacy_bytecode, test_message},
    };
    use alloc::vec::Vec;
    use evm2_sx::{PATTERNS, op};

    /// What a frame's caller can observe, with every exceptional halt alike.
    #[derive(Debug, PartialEq, Eq)]
    enum Outcome {
        Halted {
            stop: InstrStop,
            gas: u64,
            refund: i64,
            stack: Vec<Word>,
            memory: Vec<u8>,
            output: Vec<u8>,
        },
        Exceptional,
    }

    fn run(code: &[u8], gas_limit: u64, spec: SpecId, sx: bool) -> Outcome {
        let message = Message::<TestTypes> {
            code: legacy_bytecode(code.to_vec()),
            gas_limit,
            ..test_message()
        };
        let mut interpreter = Interpreter::<TestTypes>::new(&message);
        let mut host = TestHost { spec_id: spec, ..TestHost::default() };
        host.execution_config = ExecutionConfig::for_base_spec::<BaseEvmConfigSelector>(spec);
        host.execution_config.set_superinstructions(sx);
        host.execution_config.analyze(&message.code);
        let stop = interpreter.run(&mut host).expect("no host errors");
        let output = interpreter.output().to_vec();
        let (stack, stack_len, gas, memory, _) = interpreter.into_parts();
        if !(stop.is_success() || stop.is_revert()) {
            return Outcome::Exceptional;
        }
        // SAFETY: The first `stack_len` words are initialized.
        let stack =
            unsafe { core::slice::from_raw_parts(stack.0.as_ptr().cast::<Word>(), stack_len) };
        Outcome::Halted {
            stop,
            gas: gas.remaining(),
            refund: gas.refunded(),
            stack: stack.to_vec(),
            memory: memory.slice(0, memory.len()).to_vec(),
            output,
        }
    }

    /// Random code made mostly of pattern opcodes, with jumps to its own JUMPDESTs.
    fn random_code(next: &mut impl FnMut() -> u64) -> Vec<u8> {
        let alphabet: Vec<u8> =
            PATTERNS.iter().flat_map(|p| (0..p.len()).map(move |i| p.op(i))).collect();
        let n = 4 + (next() % 120) as usize;
        let mut code = Vec::new();
        let mut targets = Vec::new();
        for _ in 0..n {
            let r = next();
            let op = match r % 16 {
                0 => op::JUMPDEST,
                1 => (r >> 8) as u8,
                _ => alphabet[(r >> 8) as usize % alphabet.len()],
            };
            if op == op::JUMPDEST {
                targets.push(code.len());
            }
            code.push(op);
            if (op::PUSH1..=op::PUSH32).contains(&op) {
                let len = (op - op::PUSH1 + 1) as usize;
                let small = !(r >> 20).is_multiple_of(4);
                for i in 0..len {
                    let byte = if small && i + 2 < len { 0 } else { (next() >> 13) as u8 };
                    code.push(if small && i + 1 == len { byte & 0x7F } else { byte });
                }
            }
        }
        // Point some PUSH2 immediates at JUMPDESTs.
        let mut pc = 0;
        while pc < code.len() {
            let op = code[pc];
            if op == op::PUSH2
                && pc + 2 < code.len()
                && !targets.is_empty()
                && next().is_multiple_of(2)
            {
                let t = targets[(next() as usize) % targets.len()] as u16;
                code[pc + 1..pc + 3].copy_from_slice(&t.to_be_bytes());
            }
            pc += if (op::PUSH1..=op::PUSH32).contains(&op) {
                (op - op::PUSH1) as usize + 2
            } else {
                1
            };
        }
        code
    }

    #[test]
    fn matches_regular_dispatch() {
        let mut s = 0x853C49E6748FEA9Bu64;
        let mut next = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        let mut fused = 0;
        for round in 0..30_000 {
            let spec = match round % 4 {
                0 | 1 => SpecId::OSAKA,
                2 => SpecId::BYZANTIUM,
                _ => SpecId::AMSTERDAM,
            };
            let mut code = random_code(&mut next);
            // A prefix of pushes, so sequences see a stack.
            let mut prefix = Vec::new();
            for _ in 0..(next() % 6) {
                prefix.extend([op::PUSH1, (next() >> 7) as u8 & 0x3F]);
            }
            prefix.append(&mut code);
            let code = prefix;
            let gas = match next() % 4 {
                0 => next() % 64,
                1 => next() % 400,
                _ => next() % 30_000,
            };
            let regular = run(&code, gas, spec, false);
            let sx = run(&code, gas, spec, true);
            assert_eq!(regular, sx, "round {round} spec {spec:?} gas {gas} code {code:02x?}");
            if spec == SpecId::OSAKA
                && evm2_sx::translate(
                    &{
                        let mut c = code.clone();
                        c.extend([0; 33]);
                        c
                    },
                    code.len(),
                    &evm2_sx::Matcher::new(super::ops::<
                        TestTypes,
                        crate::BaseEvmConfig<{ SpecId::OSAKA as u32 }>,
                    >()),
                    None,
                )[..code.len()]
                    != code[..]
            {
                fused += 1;
            }
        }
        assert!(fused > 5_000, "only {fused} programs fused");
    }

    #[test]
    fn translates_after_entries() {
        let code = legacy_bytecode(alloc::vec![op::PUSH2, 0, 4, op::JUMP, op::JUMPDEST]);
        let matcher = evm2_sx::Matcher::new(super::ops::<
            TestTypes,
            crate::BaseEvmConfig<{ SpecId::OSAKA as u32 }>,
        >());
        for _ in 1..crate::bytecode::SX_TRANSLATE_AFTER {
            assert!(code.sx_code(&matcher, false).is_none());
        }
        assert_eq!(code.sx_code(&matcher, false).unwrap()[0], PATTERNS[2].slot);
        // Translated once, for good.
        assert!(code.sx_code(&matcher, false).is_some());
    }

    #[test]
    fn applies_per_spec() {
        let on = |spec| {
            let mut config =
                ExecutionConfig::<TestTypes>::for_base_spec::<BaseEvmConfigSelector>(spec);
            config.set_superinstructions(true);
            config.superinstructions()
        };
        assert!(on(SpecId::FRONTIER));
        assert!(on(SpecId::OSAKA));
        // EIP-8024 gives DUPN, SWAPN and EXCHANGE immediates, which translation does not follow.
        assert!(!on(SpecId::AMSTERDAM));
    }
}
