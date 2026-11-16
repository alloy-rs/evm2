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

const _: () = assert!(PATTERNS.len() == 48, "update `for_each_pattern!`");

macro_rules! for_each_pattern {
    ($m:ident) => {
        $m!(
            0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23
            24 25 26 27 28 29 30 31 32 33 34 35 36 37 38 39 40 41 42 43 44 45 46 47
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
        if matches!(sx.op(i), op::MLOAD | op::MSTORE) {
            flags = flags.union(InstructionFlags::DYNAMIC_GAS);
        }
        i += 1;
    }
    flags
}

/// Returns the code offset of each opcode of `sx` from its first, and the sequence length last.
const fn offsets(sx: Sx) -> [usize; 9] {
    let mut offsets = [0; 9];
    let mut i = 0;
    while i < sx.len() {
        offsets[i + 1] = offsets[i] + evm2_sx::instruction_len(sx.op(i));
        i += 1;
    }
    let mut j = sx.len() + 1;
    while j < 9 {
        offsets[j] = offsets[sx.len()];
        j += 1;
    }
    offsets
}

/// The words a superinstruction pushed and has not yet stored, above the stack in memory.
///
/// Every opcode of a sequence is known at compile time, so the number of cached words is known at
/// each step and the cache lives in registers: a word pushed and popped within the sequence never
/// reaches memory.
struct Top {
    cache: [Word; 4],
    n: usize,
    /// One past the topmost word in memory.
    sp: *mut Word,
}

impl Top {
    /// Returns the word `depth` words from the top, 1 being the top.
    #[inline(always)]
    const unsafe fn get(&self, depth: usize) -> Word {
        if depth <= self.n {
            self.cache[self.n - depth]
        } else {
            unsafe { self.sp.sub(depth - self.n).read() }
        }
    }

    #[inline(always)]
    const unsafe fn set(&mut self, depth: usize, value: Word) {
        if depth <= self.n {
            self.cache[self.n - depth] = value;
        } else {
            unsafe { self.sp.sub(depth - self.n).write(value) }
        }
    }

    #[inline(always)]
    const unsafe fn pop(&mut self) -> Word {
        if self.n > 0 {
            self.n -= 1;
            self.cache[self.n]
        } else {
            unsafe {
                self.sp = self.sp.sub(1);
                self.sp.read()
            }
        }
    }

    #[inline(always)]
    const unsafe fn push(&mut self, value: Word) {
        if self.n == self.cache.len() {
            unsafe {
                self.sp.write(self.cache[0]);
                self.sp = self.sp.add(1);
            }
            self.cache = [self.cache[1], self.cache[2], self.cache[3], value];
        } else {
            self.cache[self.n] = value;
            self.n += 1;
        }
    }

    /// Stores the cached words and returns one past the top of the stack.
    #[inline(always)]
    unsafe fn flush(self) -> *mut Word {
        unsafe {
            for i in 0..self.n {
                self.sp.add(i).write(self.cache[i]);
            }
            self.sp.add(self.n)
        }
    }
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

#[inline(always)]
fn word(b: bool) -> Word {
    Word::from(b as u64)
}

/// Runs superinstruction `I`.
#[inline(always)]
fn run<T: EvmTypesHost, const I: usize>(
    pc: &mut Pc,
    stack: StackMut<'_>,
    state: &mut InterpreterState<'_, '_, T>,
) -> Result {
    let len = const { PATTERNS[I].len() };
    let (input, growth) = const { PATTERNS[I].stack_bounds() };
    let offsets = const { offsets(PATTERNS[I]) };

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
    let mut top = Top { cache: [Word::ZERO; 4], n: 0, sp: unsafe { base.add(height) } };
    let code = pc.as_ptr();
    // A final JUMP or JUMPI: the target, and the condition for JUMPI.
    let mut jump: Option<(Word, Option<Word>)> = None;

    macro_rules! steps {
        ($($i:literal)*) => {$(
            if $i < len {
                let op = const { PATTERNS[I].op($i) };
                // SAFETY: The checks above cover the stack accesses, and the immediate is in the
                // padded code.
                unsafe {
                    match op {
                        op::JUMPDEST => {}
                        op::PUSH0..=op::PUSH32 => {
                            let n = (op - op::PUSH0) as usize;
                            top.push(immediate(code.add(offsets[$i] + 1), n));
                        }
                        op::POP => {
                            let _ = top.pop();
                        }
                        op::DUP1..=op::DUP4 => {
                            let v = top.get((op - op::DUP1) as usize + 1);
                            top.push(v);
                        }
                        op::SWAP1..=op::SWAP4 => {
                            let k = (op - op::SWAP1) as usize + 2;
                            let (a, b) = (top.get(1), top.get(k));
                            top.set(1, b);
                            top.set(k, a);
                        }
                        op::ISZERO => {
                            let a = top.pop();
                            top.push(word(a.is_zero()));
                        }
                        op::ADD | op::MUL | op::SUB | op::LT | op::GT | op::SGT | op::EQ
                        | op::AND | op::SHL | op::SHR => {
                            let a = top.pop();
                            let b = top.pop();
                            top.push(match op {
                                op::ADD => a.wrapping_add(b),
                                op::MUL => a.wrapping_mul(b),
                                op::SUB => a.wrapping_sub(b),
                                op::LT => word(a < b),
                                op::GT => word(a > b),
                                op::SGT => word(i256_lt(&b, &a)),
                                op::EQ => word(a == b),
                                op::AND => a & b,
                                op::SHL => {
                                    let shift = a.to_usize_saturated();
                                    if shift < 256 { b << shift } else { Word::ZERO }
                                }
                                _ => {
                                    let shift = a.to_usize_saturated();
                                    if shift < 256 { b >> shift } else { Word::ZERO }
                                }
                            });
                        }
                        op::MLOAD => {
                            let offset = top.pop().to_usize()?;
                            let (gas, state) = split_gas_state(&raw mut *state);
                            state.resize_memory(gas, offset, 32)?;
                            top.push(state.memory().get_word(offset));
                        }
                        op::MSTORE => {
                            let offset = top.pop().to_usize()?;
                            let value = top.pop();
                            let (gas, state) = split_gas_state(&raw mut *state);
                            state.resize_memory(gas, offset, 32)?;
                            state.memory().set(offset, &value.to_be_bytes::<32>());
                        }
                        op::JUMP => jump = Some((top.pop(), None)),
                        op::JUMPI => {
                            let target = top.pop();
                            jump = Some((target, Some(top.pop())));
                        }
                        _ => unreachable!(),
                    }
                }
            }
        )*};
    }
    steps!(0 1 2 3 4 5 6 7);

    // SAFETY: The final height is within the checked bounds.
    *stack_len = unsafe { top.flush().offset_from(base) } as usize;
    if let Some((target, cond)) = jump
        && cond.is_none_or(|cond| !cond.is_zero())
    {
        let target = target.to_usize_saturated();
        let bytecode = state.bytecode();
        if !bytecode.is_valid_jumpdest(target) {
            cold_path();
            return Err(InstrStop::InvalidJump);
        }
        // SAFETY: `target` is a valid jump destination of the running code.
        unsafe { pc.set_unchecked(bytecode, target) };
    } else {
        // SAFETY: The sequence is in the code.
        unsafe { pc.advance_unchecked(offsets[len]) };
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
