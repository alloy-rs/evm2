//! Stack superinstructions.
//!
//! A superinstruction runs a fixed sequence of ordinary instructions with one dispatch. It works on
//! the operand stack like the sequence does, charges the sequence's static gas, and reads the
//! sequence's immediates where they are in the code, so the code keeps its layout: every program
//! counter, jump destination and `PC` value is unchanged.
//!
//! [`translate()`] makes a copy of the code in which the first byte of each match of [`PATTERNS`]
//! holds the pattern's [`Sx::slot`], an opcode the configuration leaves undefined. The interpreter
//! runs that copy on a table whose slot entries run the sequences; the original code, which
//! `CODECOPY` and inspectors see, is unchanged.
//!
//! Superinstructions are internal: translation writes a spare undefined opcode over every
//! instruction of the original code whose byte is a slot in use, so such code halts exactly as
//! before. A sequence is fused only when every opcode in it is defined and unhooked, so translated
//! code never runs an opcode the configuration does not have. Translation follows instruction
//! boundaries as jump destination analysis does, with PUSH as the only instruction with an
//! immediate, so a configuration that defines another one (EIP-8024's DUPN, SWAPN and EXCHANGE)
//! fuses nothing.
//!
//! A superinstruction runs each opcode through the [`InstructionLogic`] of its configuration's
//! instruction, on words it keeps in registers, so custom instructions fuse too. PUSH, DUP, SWAP
//! and POP only rename words, and a final jump to a constant that translation checked skips the
//! run-time check. The dispatch charges the summed static gas up front, and stack bounds are
//! checked once for the whole sequence. Charging and checking up front only changes which
//! exceptional halt a failing sequence ends with, and every exceptional halt consumes all gas.
//!
//! [`InstructionLogic`]: super::private::InstructionLogic

mod patterns;
mod translate;

mod arch {
    #[cfg(target_arch = "x86_64")]
    pub(super) mod x86;
}

use super::{
    InstrStop, InterpreterState, Pc, Result, StackMut, Word, op,
    opcode::OpCode,
    private::{Instruction, InstructionFlags, split_gas_state},
};
use crate::{EvmConfig, EvmTypesHost, OpcodeConfig, OpcodeSet, SpecId, constants::STACK_LIMIT};
use core::{hint::cold_path, marker::PhantomData};
use patterns::{PATTERNS, Sx};
pub(crate) use translate::{MAX_LEN, Matcher, Ops, translate};

macro_rules! for_each_pattern {
    ($m:ident) => {
        $m!(
            0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29
            30 31 32 33 34 35 36 37 38 39 40 41 42 43 44 45 46 47 48 49 50 51 52 53 54 55 56 57 58 59
            60 61 62 63 64 65 66 67 68 69 70 71 72 73 74 75 76 77 78 79 80 81 82 83 84 85 86 87 88 89
            90 91 92 93 94 95 96 97 98 99 100 101 102 103 104
        )
    };
}

/// Configuration `C` with the superinstructions that apply to it.
pub(crate) struct SxConfig<C>(PhantomData<fn() -> C>);

impl<T: EvmTypesHost, C: EvmConfig<T>> EvmConfig<T> for SxConfig<C> {
    const BASE_SPEC_ID: SpecId = C::BASE_SPEC_ID;
    const OPCODE_CONFIG: &'static OpcodeConfig<T> = &opcode_config::<T, C>();
}

/// Returns whether superinstructions run `op` themselves rather than through its logic.
const fn is_structural(op: u8) -> bool {
    matches!(
        op,
        op::POP
            | op::JUMP
            | op::JUMPI
            | op::JUMPDEST
            | op::PUSH0..=op::PUSH32
            | op::DUP1..=op::DUP16
            | op::SWAP1..=op::SWAP16
    )
}

/// Returns the opcodes of `C` as translation sees them. An opcode is fusable when it is defined and
/// unhooked, and either superinstructions run it themselves and `C` keeps its base instruction, or
/// its instruction has logic with the opcode's stack inputs and outputs.
pub(crate) const fn ops<T: EvmTypesHost, C: EvmConfig<T>>() -> Ops {
    let config = C::OPCODE_CONFIG;
    let base = OpcodeConfig::<T>::base::<C>();
    let mut ops = Ops { fusable: OpcodeSet::EMPTY, free: OpcodeSet::EMPTY };
    let mut i = 0;
    while i < 256 {
        let op = i as u8;
        i += 1;
        if config.is_unknown_opcode(op) {
            ops.free.insert(op);
            continue;
        }
        if config.instruction(op).flags.contains(InstructionFlags::HOOKED) {
            continue;
        }
        let fusable = if is_structural(op) {
            !config.is_overridden(&base, op)
        } else if let Some(logic) = config.instruction_logic(op) {
            let info = OpCode::new_or_unknown(op);
            logic.inputs == info.inputs() && logic.outputs == info.outputs()
        } else {
            false
        };
        if fusable {
            ops.fusable.insert(op);
        }
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
                config.set_instruction::<SxInstr<C, $i>>(sx.slot, gas);
            }
        )*};
    }
    for_each_pattern!(set);
    config
}

/// Superinstruction `I` of [`PATTERNS`], running `C`'s instructions.
pub(crate) struct SxInstr<C, const I: usize>(PhantomData<fn() -> C>);

impl<T: EvmTypesHost, C: EvmConfig<T>, const I: usize> Instruction<T> for SxInstr<C, I> {
    const FLAGS: InstructionFlags = flags::<T, C>(PATTERNS[I]);

    #[inline(always)]
    fn execute(
        pc: &mut Pc,
        stack: StackMut<'_>,
        state: &mut InterpreterState<'_, '_, T>,
    ) -> Result {
        run::<T, C, I>(pc, stack, state)
    }
}

const fn flags<T: EvmTypesHost, C: EvmConfig<T>>(sx: Sx) -> InstructionFlags {
    let mut flags = InstructionFlags::SETS_PC;
    // A jump charges the JUMPDEST it skips.
    if matches!(sx.op(sx.len() - 1), op::JUMP | op::JUMPI) {
        flags = flags.union(InstructionFlags::DYNAMIC_GAS);
    }
    let mut i = 0;
    while i < sx.len() {
        if !is_structural(sx.op(i)) {
            let own = C::OPCODE_CONFIG.instruction(sx.op(i)).flags;
            flags = flags.union(own.intersection(InstructionFlags::DYNAMIC_GAS));
        }
        i += 1;
    }
    flags
}

/// No value.
const NONE: u8 = u8::MAX;

/// Values a plan may hold: the input words, then one per opcode that produces a word.
const VALS: usize = 16;

/// One opcode of a plan: the values it reads, the top first, the value it writes, and the code
/// offset of a PUSH immediate.
#[derive(Clone, Copy)]
struct Step {
    inputs: [u8; 2],
    output: u8,
    imm: u8,
}

/// A superinstruction compiled to straight-line code over numbered values.
///
/// The stack words it reads are values `0..input`, value `k` being the word `k + 1` deep. PUSH,
/// DUP, SWAP and POP only move value numbers; every other opcode makes a new value. When the
/// sequence ends, `out` holds the values of the stack slots from `input` deep up, and only slots
/// whose value changed are written.
#[derive(Clone, Copy)]
struct Plan {
    input: usize,
    steps: [Step; 8],
    out: [u8; VALS],
    out_len: usize,
    /// The final JUMP or JUMPI's target and condition.
    jump: [u8; 2],
}

const fn plan(sx: Sx) -> Plan {
    let (input, growth) = sx.stack_bounds();
    let offsets = sx.offsets();
    assert!(input + growth <= VALS);
    let mut stack = [NONE; VALS];
    let mut len = 0;
    while len < input {
        stack[len] = (input - 1 - len) as u8;
        len += 1;
    }
    let mut vals = input;
    let none = Step { inputs: [NONE; 2], output: NONE, imm: 0 };
    let mut p = Plan { input, steps: [none; 8], out: [NONE; VALS], out_len: 0, jump: [NONE; 2] };
    let mut i = 0;
    while i < sx.len() {
        let op = sx.op(i);
        let mut s = none;
        match op {
            op::JUMPDEST => {}
            op::POP => len -= 1,
            op::DUP1..=op::DUP16 => {
                stack[len] = stack[len - (op - op::DUP1) as usize - 1];
                len += 1;
            }
            op::SWAP1..=op::SWAP16 => {
                stack.swap(len - 1, len - (op - op::SWAP1) as usize - 2);
            }
            _ => {
                let info = OpCode::new_or_unknown(op);
                let (inputs, outputs) = (info.inputs() as usize, info.outputs() as usize);
                assert!(inputs <= 2 && outputs <= 1, "unsupported opcode in a pattern");
                let mut k = 0;
                while k < inputs {
                    s.inputs[k] = stack[len - 1 - k];
                    k += 1;
                }
                len -= inputs;
                if matches!(op, op::JUMP | op::JUMPI) {
                    p.jump = s.inputs;
                }
                if outputs == 1 {
                    s.output = vals as u8;
                    stack[len] = s.output;
                    len += 1;
                    vals += 1;
                }
                if op >= op::PUSH1 && op <= op::PUSH32 {
                    s.imm = offsets[i] as u8 + 1;
                }
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

/// Runs superinstruction `I` with `C`'s instructions.
#[inline(always)]
fn run<T: EvmTypesHost, C: EvmConfig<T>, const I: usize>(
    pc: &mut Pc,
    stack: StackMut<'_>,
    state: &mut InterpreterState<'_, '_, T>,
) -> Result {
    let input = const { plan(PATTERNS[I]).input };
    let growth = const { PATTERNS[I].stack_bounds().1 };
    let out_len = const { plan(PATTERNS[I]).out_len };
    let len = const { PATTERNS[I].len() };
    let span = const { PATTERNS[I].offsets()[PATTERNS[I].len()] };

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
    inputs!(0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15);

    macro_rules! steps {
        ($($i:literal)*) => {$(
            if $i < len {
                let op = const { PATTERNS[I].op($i) };
                let step = const { plan(PATTERNS[I]).steps[$i] };
                let [a, b, out] = [step.inputs[0], step.inputs[1], step.output].map(|k| k as usize % VALS);
                if let op::PUSH0..=op::PUSH32 = op {
                    // SAFETY: The immediate is in the padded code.
                    v[out] = unsafe { immediate(code.add(step.imm as usize), (op - op::PUSH0) as usize) };
                } else if !is_structural(op) {
                    let Some(logic) = (const { C::OPCODE_CONFIG.instruction_logic(PATTERNS[I].op($i)) }) else {
                        unreachable!()
                    };
                    // The opcode's stack slots, deepest first.
                    let mut slots = match logic.inputs {
                        2 => [v[b], v[a]],
                        1 => [v[a], Word::ZERO],
                        _ => [Word::ZERO; 2],
                    };
                    let used = logic.inputs.max(logic.outputs) as usize;
                    (logic.run)(pc, state, &mut slots[..used])?;
                    if logic.outputs == 1 {
                        v[out] = slots[0];
                    }
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

    let [target, cond] = const { plan(PATTERNS[I]).jump };
    let taken = target != NONE && (cond == NONE || !v[cond as usize % VALS].is_zero());
    if !taken {
        if target != NONE {
            cold_path();
        }
        // SAFETY: The sequence is in the code.
        unsafe { pc.advance_unchecked(span) };
        return Ok(());
    }
    match const { PATTERNS[I].static_jump() } {
        // Translation fused this jump only because its constant target is a jump destination.
        Some(n) if n <= 8 => {
            let at = const { PATTERNS[I].offsets()[PATTERNS[I].len() - 2] + 1 };
            // SAFETY: The immediate is in the padded code.
            let target = (unsafe {
                u64::from_be_bytes(code.add(at).cast::<[u8; 8]>().read_unaligned()) >> (8 * (8 - n))
            }) as usize;
            // SAFETY: `target` is a jump destination.
            unsafe { pc.set_unchecked(state.bytecode(), target) };
        }
        _ => {
            // The jump's own logic checks the target and moves `pc`; the condition holds.
            let Some(jump) = (const {
                C::OPCODE_CONFIG.instruction_logic(PATTERNS[I].op(PATTERNS[I].len() - 1))
            }) else {
                unreachable!()
            };
            let mut slots = [v[cond as usize % VALS], v[target as usize % VALS]];
            let slots = if cond == NONE { &mut slots[1..] } else { &mut slots[..] };
            (jump.run)(pc, state, slots)?;
        }
    }
    // Run the JUMPDEST the jump lands on, unless translation fused it with what follows, and
    // continue after it: the table backends then save a hard-to-predict dispatch.
    // SAFETY: `pc` is at a jump destination, in the code.
    let skip = (unsafe { *pc.as_ptr() } == op::JUMPDEST) as usize;
    // SAFETY: `state` is valid for the call.
    let (gas, _) = unsafe { split_gas_state(&raw mut *state) };
    // JUMPDEST costs 1 gas in every spec.
    gas.spend(skip as u64)?;
    // SAFETY: Past a JUMPDEST is the next instruction.
    unsafe { pc.advance_unchecked(skip) };
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{PATTERNS, patterns::tests::sx};
    use crate::{
        BaseEvmConfig, BaseEvmConfigSelector, EvmConfig, ExecutionConfig, SpecId,
        interpreter::{InstrStop, Interpreter, Message, Word, op},
        test_utils::{TestHost, TestTypes, legacy_bytecode, test_message},
        version::OpcodeConfig,
    };
    use alloc::vec::Vec;
    use evm2_macros::instruction;

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

    fn run(
        code: &[u8],
        gas_limit: u64,
        mut config: ExecutionConfig<TestTypes>,
        sx: bool,
    ) -> Outcome {
        // Call data and a value, for CALLDATALOAD, CALLDATASIZE and CALLVALUE.
        let input: Vec<u8> = (0..68u8).map(|i| i.wrapping_mul(37) ^ 0xA5).collect();
        let message = Message::<TestTypes> {
            code: legacy_bytecode(code.to_vec()),
            gas_limit,
            input: alloy_primitives::Bytes::from(input).into(),
            value: Word::from(0x1234_5678u64),
            ..test_message()
        };
        let mut interpreter = Interpreter::<TestTypes>::new(&message);
        let mut host = TestHost { spec_id: config.base_spec_id(), ..TestHost::default() };
        config.set_superinstructions(sx);
        config.analyze(&message.code);
        host.execution_config = config;
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

    fn spec_config(spec: SpecId) -> ExecutionConfig<TestTypes> {
        ExecutionConfig::for_base_spec::<BaseEvmConfigSelector>(spec)
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

    fn osaka_matcher() -> super::Matcher {
        super::Matcher::new(super::ops::<TestTypes, BaseEvmConfig<{ SpecId::OSAKA as u32 }>>())
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
        let matcher = osaka_matcher();
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
            let regular = run(&code, gas, spec_config(spec), false);
            let sx = run(&code, gas, spec_config(spec), true);
            assert_eq!(regular, sx, "round {round} spec {spec:?} gas {gas} code {code:02x?}");
            if spec == SpecId::OSAKA {
                let bytecode = legacy_bytecode(code.clone());
                let jumps = bytecode.jump_table().as_slice();
                let out = super::translate(bytecode.bytes_slice(), code.len(), &matcher, jumps);
                fused += (out[..code.len()] != code[..]) as usize;
            }
        }
        assert!(fused > 5_000, "only {fused} programs fused");
    }

    /// ADD, plus one, for 7 more gas.
    #[instruction(dynamic_gas)]
    fn add_plus_one(cx: _, [a, b]: [Word]) -> Result<out> {
        cx.gas.spend(7)?;
        *out = a.wrapping_add(*b).wrapping_add(Word::from(1));
    }

    struct PlusOne;

    impl EvmConfig<TestTypes> for PlusOne {
        const BASE_SPEC_ID: SpecId = SpecId::OSAKA;
        const OPCODE_CONFIG: &'static OpcodeConfig<TestTypes> = &plus_one();
    }

    const fn plus_one() -> OpcodeConfig<TestTypes> {
        let mut config =
            OpcodeConfig::<TestTypes>::base::<BaseEvmConfig<{ SpecId::OSAKA as u32 }>>();
        config.set_instruction::<add_plus_one<TestTypes>>(op::ADD, 3);
        config
    }

    #[test]
    fn fuses_custom_instructions() {
        use op::*;
        assert!(super::ops::<TestTypes, PlusOne>().fusable.contains(ADD));
        // PUSH1 7 PUSH1 5 | DUP2 ADD: the second superinstruction runs the custom ADD.
        let code = [PUSH1, 7, PUSH1, 5, DUP2, ADD, STOP];
        let config = ExecutionConfig::<TestTypes>::for_config::<PlusOne>;
        let regular = run(&code, 100, config(), false);
        let fused = run(&code, 100, config(), true);
        assert_eq!(regular, fused);
        let Outcome::Halted { stack, gas, .. } = fused else { panic!("halted") };
        assert_eq!((stack, gas), (alloc::vec![Word::from(7), Word::from(13)], 100 - 9 - 3 - 7));
        let matcher = super::Matcher::new(super::ops::<TestTypes, PlusOne>());
        let bytecode = legacy_bytecode(code.to_vec());
        let out = bytecode.sx_code(&matcher, true).unwrap();
        assert_eq!(out[4], sx(&[DUP2, ADD]).slot);
    }

    #[test]
    fn translates_after_entries() {
        let code = legacy_bytecode(alloc::vec![op::PUSH2, 0, 4, op::JUMP, op::JUMPDEST]);
        let matcher = osaka_matcher();
        for _ in 1..crate::bytecode::SX_TRANSLATE_AFTER {
            assert!(code.sx_code(&matcher, false).is_none());
        }
        assert_eq!(code.sx_code(&matcher, false).unwrap()[0], sx(&[op::PUSH2, op::JUMP]).slot);
        // Translated once, for good.
        assert!(code.sx_code(&matcher, false).is_some());
    }

    #[test]
    fn applies_per_spec() {
        let on = |spec| {
            let mut config = spec_config(spec);
            config.set_superinstructions(true);
            config.sx_table().is_some()
        };
        assert!(on(SpecId::FRONTIER));
        assert!(on(SpecId::OSAKA));
        // EIP-8024 gives DUPN, SWAPN and EXCHANGE immediates, which translation does not follow.
        assert!(!on(SpecId::AMSTERDAM));
    }
}
