//! Instruction dispatch tables.

use crate::{
    BaseEvmConfigSelector, EvmConfig, EvmConfigSelector, EvmTypesHost, OpcodeConfig,
    constants::STACK_LIMIT,
    evm::config::SelectorOpcodeConfig,
    interpreter::{
        InstrStop, Interpreter, InterpreterState, Pc, RawStack, Result, StackMut, Word, op,
    },
    trustme,
    utils::WordExt,
};
use core::hint::cold_path;

core::cfg_select! {
    tco => {
        mod tco;
        use tco as imp;
    }
    _ => {
        mod table;
        use table as imp;
    }
}

pub(in crate::interpreter) use imp::run;

#[inline(always)]
fn run_state<'a, 'frame, 'host, T: EvmTypesHost>(
    interpreter: &'a mut Interpreter<'frame, 'host, T>,
) -> (&'a mut InterpreterState<'frame, 'host, T>, Pc, RawStack<'a>) {
    let pc = Pc::new(interpreter.pc);
    let stack = RawStack::new(&raw mut *interpreter.stack, interpreter.stack_len);
    // SAFETY: Only the active interpreter lifetime is erased; this stays as a raw pointer so
    // the dispatch loop does not create an extra `&mut` alias for `interpreter`.
    let raw =
        unsafe { trustme::decouple_lt_mut_ptr(interpreter as *mut Interpreter<'frame, 'host, T>) };
    // SAFETY: Instruction methods must not access the stack through `InterpreterState` while
    // the separate stack view is live.
    let state = InterpreterState::wrap_mut(unsafe { &mut *raw });
    (state, pc, stack)
}

/// Jumps to a valid destination and skips its `JUMPDEST` when gas allows.
#[inline(always)]
fn jump_and_fuse_jumpdest<T: EvmTypesHost, C: EvmConfig<T>>(
    pc: &mut Pc,
    state: &mut InterpreterState<'_, '_, T>,
    target: usize,
    err_pc_advance: usize,
    try_spend: &mut impl FnMut(&mut InterpreterState<'_, '_, T>, u64) -> bool,
) -> Result {
    if !state.bytecode().is_valid_jumpdest(target) {
        cold_path();
        unsafe { pc.advance_unchecked(err_pc_advance) };
        return Err(InstrStop::InvalidJump);
    }
    unsafe { pc.set_unchecked(state.bytecode(), target) };
    if C::OPCODE_CONFIG.is_unoverridden_base(op::JUMPDEST)
        && try_spend(state, C::OPCODE_CONFIG.static_gas(op::JUMPDEST) as u64)
    {
        unsafe { pc.advance_unchecked(1) };
    }
    Ok(())
}

#[inline(always)]
const unsafe fn read_u16_target(pc: Pc, offset: usize) -> usize {
    let bytes = unsafe { pc.read_bytes_offset_unchecked(offset, 2) };
    u16::from_be_bytes([bytes[0], bytes[1]]) as usize
}

/// Attempts to fuse `PUSH2` followed by `JUMP` or `JUMPI` (and its target `JUMPDEST`).
#[inline(always)]
fn try_fuse_push2_jump<T: EvmTypesHost, C: EvmConfig<T>>(
    pc: &mut Pc,
    stack: &mut StackMut<'_>,
    state: &mut InterpreterState<'_, '_, T>,
    mut try_spend: impl FnMut(&mut InterpreterState<'_, '_, T>, u64) -> bool,
) -> Option<Result> {
    // SAFETY: Legacy bytecode is padded with at least 33 zero bytes.
    let next_op = unsafe { *pc.as_ptr().add(3) };
    if (next_op & !1) != op::JUMP {
        return None;
    }
    if next_op == op::JUMP {
        if *stack.len >= STACK_LIMIT {
            cold_path();
            return Some(Err(InstrStop::StackOverflow));
        }
        if !try_spend(state, C::OPCODE_CONFIG.static_gas(op::JUMP) as u64) {
            return None;
        }
        let target = unsafe { read_u16_target(*pc, 1) };
        return Some(jump_and_fuse_jumpdest::<T, C>(pc, state, target, 3, &mut try_spend));
    }
    let len = *stack.len;
    if len >= STACK_LIMIT {
        cold_path();
        return Some(Err(InstrStop::StackOverflow));
    }
    if len == 0 {
        return None;
    }
    if !try_spend(state, C::OPCODE_CONFIG.static_gas(op::JUMPI) as u64) {
        return None;
    }
    // SAFETY: `len >= 1`.
    let is_zero = unsafe {
        let new_len = len - 1;
        *stack.len = new_len;
        (*stack.stack.0.as_ptr().add(new_len).cast::<Word>()).is_zero()
    };
    if is_zero {
        unsafe { pc.advance_unchecked(4) };
        return Some(Ok(()));
    }
    let target = unsafe { read_u16_target(*pc, 1) };
    Some(jump_and_fuse_jumpdest::<T, C>(pc, state, target, 3, &mut try_spend))
}

/// Attempts to fuse `ISZERO` followed by `PUSH2 + JUMPI` (and its target `JUMPDEST`).
#[inline(always)]
fn try_fuse_iszero_push2_jumpi<T: EvmTypesHost, C: EvmConfig<T>>(
    pc: &mut Pc,
    stack: &mut StackMut<'_>,
    state: &mut InterpreterState<'_, '_, T>,
    mut try_spend: impl FnMut(&mut InterpreterState<'_, '_, T>, u64) -> bool,
) -> Option<Result> {
    // SAFETY: Legacy bytecode is padded with at least 33 zero bytes.
    if unsafe { *pc.as_ptr().add(1) } != op::PUSH2 || unsafe { *pc.as_ptr().add(4) } != op::JUMPI {
        return None;
    }
    let len = *stack.len;
    if len == 0 {
        cold_path();
        return Some(Err(InstrStop::StackUnderflow));
    }
    if len >= STACK_LIMIT {
        return None;
    }
    let cost = C::OPCODE_CONFIG.static_gas(op::PUSH2) as u64
        + C::OPCODE_CONFIG.static_gas(op::JUMPI) as u64;
    if !try_spend(state, cost) {
        return None;
    }
    // SAFETY: `len >= 1`.
    let is_zero = unsafe {
        let new_len = len - 1;
        *stack.len = new_len;
        (*stack.stack.0.as_ptr().add(new_len).cast::<Word>()).is_zero()
    };
    if !is_zero {
        unsafe { pc.advance_unchecked(5) };
        return Some(Ok(()));
    }
    let target = unsafe { read_u16_target(*pc, 2) };
    Some(jump_and_fuse_jumpdest::<T, C>(pc, state, target, 4, &mut try_spend))
}

/// Attempts to fuse a binary comparison (`EQ`/`LT`/`GT`) followed by `PUSH2 + JUMPI`.
#[inline(always)]
fn try_fuse_cmp_push2_jumpi<T: EvmTypesHost, C: EvmConfig<T>, const OP: u8>(
    pc: &mut Pc,
    stack: &mut StackMut<'_>,
    state: &mut InterpreterState<'_, '_, T>,
    mut try_spend: impl FnMut(&mut InterpreterState<'_, '_, T>, u64) -> bool,
) -> Option<Result> {
    // SAFETY: Legacy bytecode is padded with at least 33 zero bytes.
    if unsafe { *pc.as_ptr().add(1) } != op::PUSH2 || unsafe { *pc.as_ptr().add(4) } != op::JUMPI {
        return None;
    }
    let len = *stack.len;
    if len < 2 {
        cold_path();
        return Some(Err(InstrStop::StackUnderflow));
    }
    // After popping 2 and pushing 1, `len - 1 < STACK_LIMIT`, so `PUSH2` cannot overflow.
    let cost = C::OPCODE_CONFIG.static_gas(op::PUSH2) as u64
        + C::OPCODE_CONFIG.static_gas(op::JUMPI) as u64;
    if !try_spend(state, cost) {
        return None;
    }
    // SAFETY: `len >= 2`. Top of stack is `a`, second is `b`.
    let cond = unsafe {
        let new_len = len - 2;
        *stack.len = new_len;
        let ptr = stack.stack.0.as_ptr().add(new_len).cast::<Word>();
        let b = &*ptr;
        let a = &*ptr.add(1);
        match OP {
            op::EQ => a == b,
            op::LT => a < b,
            op::GT => a > b,
            _ => unreachable!(),
        }
    };
    if !cond {
        unsafe { pc.advance_unchecked(5) };
        return Some(Ok(()));
    }
    let target = unsafe { read_u16_target(*pc, 2) };
    Some(jump_and_fuse_jumpdest::<T, C>(pc, state, target, 4, &mut try_spend))
}

/// Attempts to fuse `SWAP1` followed by `JUMP` (and its target `JUMPDEST`).
#[inline(always)]
fn try_fuse_swap1_jump<T: EvmTypesHost, C: EvmConfig<T>>(
    pc: &mut Pc,
    stack: &mut StackMut<'_>,
    state: &mut InterpreterState<'_, '_, T>,
    mut try_spend: impl FnMut(&mut InterpreterState<'_, '_, T>, u64) -> bool,
) -> Option<Result> {
    // SAFETY: Legacy bytecode is padded with at least 33 zero bytes.
    if unsafe { *pc.as_ptr().add(1) } != op::JUMP {
        return None;
    }
    let len = *stack.len;
    if len < 2 {
        cold_path();
        return Some(Err(InstrStop::StackUnderflow));
    }
    if !try_spend(state, C::OPCODE_CONFIG.static_gas(op::JUMP) as u64) {
        return None;
    }
    // SAFETY: `len >= 2`. Move top into `len - 2` after reading the jump target.
    let target = unsafe {
        let dst = stack.stack.0.as_mut_ptr().add(len - 2).cast::<Word>();
        let target = (*dst).to_usize_saturated();
        *dst = *dst.add(1);
        *stack.len = len - 1;
        target
    };
    Some(jump_and_fuse_jumpdest::<T, C>(pc, state, target, 1, &mut try_spend))
}

/// Attempts to fuse `PUSH1 <imm>` followed by `ADD`, `MLOAD`, or `MSTORE`.
#[inline(always)]
fn try_fuse_push1_op<T: EvmTypesHost, C: EvmConfig<T>>(
    pc: &mut Pc,
    stack: &mut StackMut<'_>,
    state: &mut InterpreterState<'_, '_, T>,
    mut try_spend: impl FnMut(&mut InterpreterState<'_, '_, T>, u64) -> bool,
) -> Option<Result> {
    // SAFETY: Legacy bytecode is padded with at least 33 zero bytes.
    let next_op = unsafe { *pc.as_ptr().add(2) };
    match next_op {
        op::ADD if C::OPCODE_CONFIG.is_unoverridden_base(op::ADD) => {
            let len = *stack.len;
            if len >= STACK_LIMIT {
                cold_path();
                return Some(Err(InstrStop::StackOverflow));
            }
            if len == 0 || !try_spend(state, C::OPCODE_CONFIG.static_gas(op::ADD) as u64) {
                return None;
            }
            let imm = unsafe { *pc.as_ptr().add(1) };
            // SAFETY: `len >= 1`.
            unsafe {
                let slot = stack.stack.0.as_mut_ptr().add(len - 1).cast::<Word>();
                (*slot).write((*slot).wrapping_add(Word::from(imm)));
            }
            unsafe { pc.advance_unchecked(3) };
            Some(Ok(()))
        }
        op::MLOAD if C::OPCODE_CONFIG.is_unoverridden_base(op::MLOAD) => {
            let len = *stack.len;
            if len >= STACK_LIMIT {
                cold_path();
                return Some(Err(InstrStop::StackOverflow));
            }
            let imm = unsafe { *pc.as_ptr().add(1) } as usize;
            if imm + 32 > state.0.gas().memory().words_num.wrapping_mul(32) {
                return None;
            }
            if !try_spend(state, C::OPCODE_CONFIG.static_gas(op::MLOAD) as u64) {
                return None;
            }
            let word = state.memory().get_word(imm);
            // SAFETY: `len < STACK_LIMIT`.
            unsafe {
                *stack.stack.0.as_mut_ptr().add(len).cast::<Word>() = word;
                *stack.len = len + 1;
            }
            unsafe { pc.advance_unchecked(3) };
            Some(Ok(()))
        }
        op::MSTORE if C::OPCODE_CONFIG.is_unoverridden_base(op::MSTORE) => {
            let len = *stack.len;
            if len >= STACK_LIMIT {
                cold_path();
                return Some(Err(InstrStop::StackOverflow));
            }
            if len == 0 {
                return None;
            }
            let imm = unsafe { *pc.as_ptr().add(1) } as usize;
            if imm + 32 > state.0.gas().memory().words_num.wrapping_mul(32) {
                return None;
            }
            if !try_spend(state, C::OPCODE_CONFIG.static_gas(op::MSTORE) as u64) {
                return None;
            }
            // SAFETY: `len >= 1`.
            let bytes = unsafe {
                let new_len = len - 1;
                *stack.len = new_len;
                (*stack.stack.0.as_ptr().add(new_len).cast::<Word>()).to_be_bytes::<32>()
            };
            state.memory().set(imm, &bytes);
            unsafe { pc.advance_unchecked(3) };
            Some(Ok(()))
        }
        _ => None,
    }
}

/// Attempts to fuse consecutive `POP` opcodes and an optional trailing `JUMP` (+ `JUMPDEST`).
#[inline(always)]
fn try_fuse_pop<T: EvmTypesHost, C: EvmConfig<T>>(
    pc: &mut Pc,
    stack: &mut StackMut<'_>,
    state: &mut InterpreterState<'_, '_, T>,
    mut try_spend: impl FnMut(&mut InterpreterState<'_, '_, T>, u64) -> bool,
) -> Option<Result> {
    // SAFETY: Legacy bytecode is padded with at least 33 zero bytes.
    let first_next = unsafe { *pc.as_ptr().add(1) };
    if first_next != op::POP
        && !(first_next == op::JUMP && C::OPCODE_CONFIG.is_unoverridden_base(op::JUMP))
    {
        return None;
    }
    let mut len = *stack.len;
    if len == 0 {
        cold_path();
        return Some(Err(InstrStop::StackUnderflow));
    }
    len -= 1;
    let pop_gas = C::OPCODE_CONFIG.static_gas(op::POP) as u64;
    let mut count = 1usize;
    while len > 0 && unsafe { *pc.as_ptr().add(count) } == op::POP && try_spend(state, pop_gas) {
        len -= 1;
        count += 1;
    }
    if len > 0
        && C::OPCODE_CONFIG.is_unoverridden_base(op::JUMP)
        && unsafe { *pc.as_ptr().add(count) } == op::JUMP
        && try_spend(state, C::OPCODE_CONFIG.static_gas(op::JUMP) as u64)
    {
        let new_len = len - 1;
        *stack.len = new_len;
        // SAFETY: `len >= 1`.
        let target =
            unsafe { (*stack.stack.0.as_ptr().add(new_len).cast::<Word>()).to_usize_saturated() };
        return Some(jump_and_fuse_jumpdest::<T, C>(pc, state, target, count, &mut try_spend));
    }
    *stack.len = len;
    unsafe { pc.advance_unchecked(count) };
    Some(Ok(()))
}

/// Attempts to fuse the current opcode with a following control-flow sequence.
#[inline(always)]
fn try_fuse<T: EvmTypesHost, C: EvmConfig<T>, M: InspectMode<T>>(
    op: u8,
    pc: &mut Pc,
    stack: &mut StackMut<'_>,
    state: &mut InterpreterState<'_, '_, T>,
    try_spend: impl FnMut(&mut InterpreterState<'_, '_, T>, u64) -> bool,
) -> Option<Result> {
    if M::INSPECT {
        return None;
    }
    if op == op::PUSH2
        && C::OPCODE_CONFIG.is_unoverridden_base(op::PUSH2)
        && C::OPCODE_CONFIG.is_unoverridden_base(op::JUMP)
        && C::OPCODE_CONFIG.is_unoverridden_base(op::JUMPI)
    {
        return try_fuse_push2_jump::<T, C>(pc, stack, state, try_spend);
    }
    if op == op::ISZERO
        && C::OPCODE_CONFIG.is_unoverridden_base(op::ISZERO)
        && C::OPCODE_CONFIG.is_unoverridden_base(op::PUSH2)
        && C::OPCODE_CONFIG.is_unoverridden_base(op::JUMPI)
    {
        return try_fuse_iszero_push2_jumpi::<T, C>(pc, stack, state, try_spend);
    }
    if op == op::EQ
        && C::OPCODE_CONFIG.is_unoverridden_base(op::EQ)
        && C::OPCODE_CONFIG.is_unoverridden_base(op::PUSH2)
        && C::OPCODE_CONFIG.is_unoverridden_base(op::JUMPI)
    {
        return try_fuse_cmp_push2_jumpi::<T, C, { op::EQ }>(pc, stack, state, try_spend);
    }
    if op == op::LT
        && C::OPCODE_CONFIG.is_unoverridden_base(op::LT)
        && C::OPCODE_CONFIG.is_unoverridden_base(op::PUSH2)
        && C::OPCODE_CONFIG.is_unoverridden_base(op::JUMPI)
    {
        return try_fuse_cmp_push2_jumpi::<T, C, { op::LT }>(pc, stack, state, try_spend);
    }
    if op == op::GT
        && C::OPCODE_CONFIG.is_unoverridden_base(op::GT)
        && C::OPCODE_CONFIG.is_unoverridden_base(op::PUSH2)
        && C::OPCODE_CONFIG.is_unoverridden_base(op::JUMPI)
    {
        return try_fuse_cmp_push2_jumpi::<T, C, { op::GT }>(pc, stack, state, try_spend);
    }
    if op == op::SWAP1
        && C::OPCODE_CONFIG.is_unoverridden_base(op::SWAP1)
        && C::OPCODE_CONFIG.is_unoverridden_base(op::JUMP)
    {
        return try_fuse_swap1_jump::<T, C>(pc, stack, state, try_spend);
    }
    if op == op::PUSH1 && C::OPCODE_CONFIG.is_unoverridden_base(op::PUSH1) {
        return try_fuse_push1_op::<T, C>(pc, stack, state, try_spend);
    }
    if op == op::POP && C::OPCODE_CONFIG.is_unoverridden_base(op::POP) {
        return try_fuse_pop::<T, C>(pc, stack, state, try_spend);
    }
    None
}

#[inline(always)]
const fn inc_pc(pc: &mut Pc, op: u8) {
    unsafe { pc.advance_unchecked(instruction_len(op)) };
}

#[inline(always)]
pub(crate) const fn instruction_len(op: u8) -> usize {
    match op {
        op::JUMP | op::JUMPI => 0, // Set inside.
        op::PUSH1..=op::PUSH32 => (op - op::PUSH1 + 2) as usize,
        op::DUPN | op::SWAPN | op::EXCHANGE => 2,
        _ => 1,
    }
}

/// Instruction dispatch table.
pub(crate) type InstrTable<T> = imp::RawInstrTable<T>;

const fn make_table<T, C, M>(
    previous: Option<&InstrTable<T>>,
    previous_opcode_config: Option<&OpcodeConfig<T>>,
) -> InstrTable<T>
where
    T: EvmTypesHost,
    C: EvmConfig<T>,
    M: InspectMode<T>,
{
    let mut table = match previous {
        Some(previous) => *previous,
        None => [imp::dispatch::<T, C, M, { op::INVALID }> as imp::RawInstrFn<T>; 256],
    };
    let vt = C::OPCODE_CONFIG;

    macro_rules! assign_instruction_table_entries {
        ($($op:literal,)*) => {
            $(
                if instruction_changed(vt, previous_opcode_config, $op) && !vt.is_unknown_opcode($op) {
                    table[$op] = imp::dispatch::<T, C, M, $op> as imp::RawInstrFn<T>;
                }
            )*
        };
    }

    macro_rules! for_each_opcode_value {
        ($m:ident) => {
            $m! {
                0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07,
                0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F,
                0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17,
                0x18, 0x19, 0x1A, 0x1B, 0x1C, 0x1D, 0x1E, 0x1F,
                0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27,
                0x28, 0x29, 0x2A, 0x2B, 0x2C, 0x2D, 0x2E, 0x2F,
                0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37,
                0x38, 0x39, 0x3A, 0x3B, 0x3C, 0x3D, 0x3E, 0x3F,
                0x40, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47,
                0x48, 0x49, 0x4A, 0x4B, 0x4C, 0x4D, 0x4E, 0x4F,
                0x50, 0x51, 0x52, 0x53, 0x54, 0x55, 0x56, 0x57,
                0x58, 0x59, 0x5A, 0x5B, 0x5C, 0x5D, 0x5E, 0x5F,
                0x60, 0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x67,
                0x68, 0x69, 0x6A, 0x6B, 0x6C, 0x6D, 0x6E, 0x6F,
                0x70, 0x71, 0x72, 0x73, 0x74, 0x75, 0x76, 0x77,
                0x78, 0x79, 0x7A, 0x7B, 0x7C, 0x7D, 0x7E, 0x7F,
                0x80, 0x81, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87,
                0x88, 0x89, 0x8A, 0x8B, 0x8C, 0x8D, 0x8E, 0x8F,
                0x90, 0x91, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97,
                0x98, 0x99, 0x9A, 0x9B, 0x9C, 0x9D, 0x9E, 0x9F,
                0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7,
                0xA8, 0xA9, 0xAA, 0xAB, 0xAC, 0xAD, 0xAE, 0xAF,
                0xB0, 0xB1, 0xB2, 0xB3, 0xB4, 0xB5, 0xB6, 0xB7,
                0xB8, 0xB9, 0xBA, 0xBB, 0xBC, 0xBD, 0xBE, 0xBF,
                0xC0, 0xC1, 0xC2, 0xC3, 0xC4, 0xC5, 0xC6, 0xC7,
                0xC8, 0xC9, 0xCA, 0xCB, 0xCC, 0xCD, 0xCE, 0xCF,
                0xD0, 0xD1, 0xD2, 0xD3, 0xD4, 0xD5, 0xD6, 0xD7,
                0xD8, 0xD9, 0xDA, 0xDB, 0xDC, 0xDD, 0xDE, 0xDF,
                0xE0, 0xE1, 0xE2, 0xE3, 0xE4, 0xE5, 0xE6, 0xE7,
                0xE8, 0xE9, 0xEA, 0xEB, 0xEC, 0xED, 0xEE, 0xEF,
                0xF0, 0xF1, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7,
                0xF8, 0xF9, 0xFA, 0xFB, 0xFC, 0xFD, 0xFE, 0xFF,
            }
        };
    }

    for_each_opcode_value!(assign_instruction_table_entries);
    table
}

const fn make_selector_tables<T, F, M, const CUSTOM_SPEC_ID: u32>()
-> [InstrTable<T>; crate::SpecId::COUNT]
where
    T: EvmTypesHost,
    F: EvmConfigSelector<T>,
    M: InspectMode<T>,
{
    macro_rules! make_selector_tables {
        ([$($extra:tt)*] $($spec:ident $name:ident,)*) => {{
            make_selector_tables!(@build [] [none]; $($spec $name,)*)
        }};
        (@build [$($tables:ident,)*] [$($previous_table:tt)*]; $spec:ident $name:ident, $($rest:ident $rest_name:ident,)*) => {{
            let spec = crate::SpecId::$spec;
            let previous = spec.prev();
            let $name = make_table::<T, F::Config<{ crate::SpecId::$spec as u32 }, CUSTOM_SPEC_ID>, M>(
                make_selector_tables!(@previous_table [$($previous_table)*]),
                match previous {
                    Some(previous) => {
                        Some(crate::evm::config::SelectorOpcodeConfig::<T, F, CUSTOM_SPEC_ID>::OPCODE_CONFIG[previous as usize])
                    }
                    None => None,
                },
            );
            make_selector_tables!(@build [$($tables,)* $name,] [some $name]; $($rest $rest_name,)*)
        }};
        (@build [$($tables:ident,)*] [$($previous_table:tt)*];) => {
            [$($tables,)*]
        };
        (@previous_table [none]) => {
            None
        };
        (@previous_table [some $previous_table:ident]) => {
            Some(&$previous_table)
        };
    }

    crate::for_each_spec!([] make_selector_tables)
}

pub(crate) struct ConfigInstrTables<T, C>(core::marker::PhantomData<fn() -> (T, C)>);

impl<T, C> ConfigInstrTables<T, C>
where
    T: EvmTypesHost,
    C: EvmConfig<T>,
{
    pub(crate) const INSTRUCTIONS: &'static InstrTable<T> = &make_table::<T, C, NoInspector>(
        Some(
            &SelectorInstrTables::<T, BaseEvmConfigSelector, { u32::MAX }>::INSTRUCTIONS
                [C::BASE_SPEC_ID as usize],
        ),
        Some(
            SelectorOpcodeConfig::<T, BaseEvmConfigSelector, { u32::MAX }>::OPCODE_CONFIG
                [C::BASE_SPEC_ID as usize],
        ),
    );
    pub(crate) const INSPECT_INSTRUCTIONS: &'static InstrTable<T> = &make_table::<T, C, DynInspector>(
        Some(
            &SelectorInstrTables::<T, BaseEvmConfigSelector, { u32::MAX }>::INSPECT_INSTRUCTIONS
                [C::BASE_SPEC_ID as usize],
        ),
        Some(
            SelectorOpcodeConfig::<T, BaseEvmConfigSelector, { u32::MAX }>::OPCODE_CONFIG
                [C::BASE_SPEC_ID as usize],
        ),
    );
}

pub(crate) struct SelectorInstrTables<T, F, const CUSTOM_SPEC_ID: u32>(
    core::marker::PhantomData<fn() -> (T, F)>,
);

impl<T, F, const CUSTOM_SPEC_ID: u32> SelectorInstrTables<T, F, CUSTOM_SPEC_ID>
where
    T: EvmTypesHost,
    F: EvmConfigSelector<T>,
{
    pub(crate) const INSTRUCTIONS: &'static [InstrTable<T>; crate::SpecId::COUNT] =
        &make_selector_tables::<T, F, NoInspector, CUSTOM_SPEC_ID>();
    pub(crate) const INSPECT_INSTRUCTIONS: &'static [InstrTable<T>; crate::SpecId::COUNT] =
        &make_selector_tables::<T, F, DynInspector, CUSTOM_SPEC_ID>();
}

const fn instruction_changed<T: EvmTypesHost>(
    opcode_config: &OpcodeConfig<T>,
    previous_opcode_config: Option<&OpcodeConfig<T>>,
    op: u8,
) -> bool {
    let Some(previous_opcode_config) = previous_opcode_config else {
        return true;
    };
    opcode_config.revision(op) != previous_opcode_config.revision(op)
}

trait InspectMode<T: EvmTypesHost> {
    /// Whether the dispatch calls the inspector's step hooks: from the handlers on the tail-call
    /// backend, and from the loop on the others, where a hook must not run while a handler holds
    /// the stack borrow.
    const INSPECT: bool;

    /// Whether the inspector steps every opcode, so the dispatch loop need not look at the set.
    #[cfg(not(tco))]
    const ALL_STEPS: bool = false;

    fn step(state: &mut InterpreterState<'_, '_, T>, pc: Pc, stack_len: usize);

    fn step_end(state: &mut InterpreterState<'_, '_, T>, pc: Pc, stack_len: usize);
}

struct NoInspector;

impl<T: EvmTypesHost> InspectMode<T> for NoInspector {
    const INSPECT: bool = false;

    #[inline(always)]
    fn step(_state: &mut InterpreterState<'_, '_, T>, _pc: Pc, _stack_len: usize) {}

    #[inline(always)]
    fn step_end(_state: &mut InterpreterState<'_, '_, T>, _pc: Pc, _stack_len: usize) {}
}

/// Mode of the inspect table's handlers. `ALL` is the loop mode of an inspector that steps every
/// opcode.
struct DynInspector<const ALL: bool = false>;

impl<T: EvmTypesHost, const ALL: bool> InspectMode<T> for DynInspector<ALL> {
    const INSPECT: bool = true;
    #[cfg(not(tco))]
    const ALL_STEPS: bool = ALL;

    #[inline(always)]
    fn step(state: &mut InterpreterState<'_, '_, T>, pc: Pc, stack_len: usize) {
        state.inspect_step(pc, stack_len);
    }

    #[inline(always)]
    fn step_end(state: &mut InterpreterState<'_, '_, T>, pc: Pc, stack_len: usize) {
        state.inspect_step_end(pc, stack_len);
    }
}
