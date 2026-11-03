//! Threaded execution of linked programs.
//!
//! Each record links to a handler specialized for its kind and operand modes. Handlers run on
//! the interpreter's own stack, addressing slots relative to the stack top at block entry, and
//! pass control to the next record with a tail call on nightly compilers, or by returning it to
//! a dispatch loop otherwise.

use crate::program::{
    Block, CMP_NZ, Halt, INVALID_BLOCK, Kind, MAX_UNROLLED_MOVES, Move, NO_MOVES, NO_SECTION,
    Operand, Program, Section, cond, eval1, eval2,
};
use evm2::{
    EvmTypesHost, OpcodeConfig,
    constants::STACK_LIMIT,
    interpreter::{ExternalInstruction, Gas, Host, InstrStop, Interpreter, Word, op},
    utils::WordExt,
    version::GasParams,
};

/// What a handler hands back to the dispatch loop: the next record and stack top, or nothing
/// once the frame stopped.
#[cfg(not(tco))]
pub(crate) type Next<T> = Option<(*const Rec<T>, *mut Word)>;

/// A record handler.
#[cfg(tco)]
pub(crate) type HandlerFn<T> =
    for<'c, 'f, 'h> extern "rust-preserve-none" fn(*const Rec<T>, *mut Word, &'c mut Cx<'f, 'h, T>);

/// A record handler.
#[cfg(not(tco))]
pub(crate) type HandlerFn<T> =
    for<'c, 'f, 'h> fn(*const Rec<T>, *mut Word, &'c mut Cx<'f, 'h, T>) -> Next<T>;

/// A linked record: an [`Inst`](crate::program::Inst) with its handler resolved.
#[repr(C)]
pub(crate) struct Rec<T: EvmTypesHost> {
    f: HandlerFn<T>,
    a: u32,
    b: u32,
    c: u32,
    d: u32,
    e: u32,
    g: u32,
}

/// A program linked for host type `T`.
///
/// Static jumps and fallthroughs carry their target's entry in the record: `a` is the target's
/// first record, `b` its entry gas, and `g` the entry stack lengths that need no other checks, as
/// `min_len | span << 16`. Host instructions carry their resolved implementation's index in `g`.
pub(crate) struct Linked<T: EvmTypesHost> {
    pub(crate) program: Program,
    recs: Vec<Rec<T>>,
    host_instrs: Vec<ExternalInstruction<T>>,
}

impl<T: EvmTypesHost> Linked<T> {
    /// Returns the number of records.
    pub(crate) const fn records(&self) -> usize {
        self.recs.len()
    }
}

impl<T: EvmTypesHost> core::fmt::Debug for Linked<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Linked").field("records", &self.recs.len()).finish_non_exhaustive()
    }
}

/// Execution context shared by the handlers of one frame.
pub(crate) struct Cx<'f, 'h, T: EvmTypesHost> {
    interp: *mut Interpreter<'f, 'h, T>,
    host: *mut T::Host<'h>,
    base: *mut Word,
    recs: *const Rec<T>,
    consts: *const Word,
    blocks: *const Block,
    sections: *const Section,
    moves: *const Move,
    jump_map: *const u32,
    host_instrs: *const ExternalInstruction<T>,
    code_len: usize,
    gas_params: *const GasParams,
    stop: InstrStop,
    bailed: bool,
}

macro_rules! handler {
    ($(#[$attr:meta])* fn $name:ident[$($g:tt)*]($rec:ident, $top:ident, $cx:ident) $body:block) => {
        #[cfg(tco)]
        $(#[$attr])*
        #[allow(clippy::missing_const_for_fn)]
        extern "rust-preserve-none" fn $name<$($g)*>(
            $rec: *const Rec<T>,
            $top: *mut Word,
            $cx: &mut Cx<'_, '_, T>,
        ) $body

        #[cfg(not(tco))]
        $(#[$attr])*
        #[allow(clippy::missing_const_for_fn)]
        fn $name<$($g)*>($rec: *const Rec<T>, $top: *mut Word, $cx: &mut Cx<'_, '_, T>) -> Next<T>
            $body
    };
}

#[cfg(tco)]
macro_rules! next {
    ($rec:expr, $top:expr, $cx:expr) => {{
        let rec = $rec;
        // SAFETY: Records come from a linked program that outlives the frame.
        let f = unsafe { (*rec).f };
        become f(rec, $top, $cx)
    }};
}

#[cfg(not(tco))]
macro_rules! next {
    ($rec:expr, $top:expr, $cx:expr) => {
        return Some(($rec, $top))
    };
}

#[cfg(tco)]
macro_rules! exit {
    () => {
        return
    };
}

#[cfg(not(tco))]
macro_rules! exit {
    () => {
        return None
    };
}

/// Stops the frame with `$stop`.
macro_rules! stop {
    ($cx:expr, $stop:expr) => {{
        $cx.stop = $stop;
        exit!()
    }};
}

/// Enters block `$block` with stack top `$top`, or stops.
macro_rules! goto {
    ($cx:expr, $block:expr, $top:expr) => {{
        let top = $top;
        // SAFETY: Block IDs come from translation of the running program.
        match unsafe { enter($cx, $block, top) } {
            Some(rec) => next!(rec, top, $cx),
            None => exit!(),
        }
    }};
}

/// Links `program` for host type `T` and its opcode table.
pub(crate) fn link<T: EvmTypesHost>(program: Program, opcodes: &OpcodeConfig<T>) -> Linked<T> {
    let mut host_instrs = Vec::new();
    let recs = program
        .insts
        .iter()
        .map(|inst| {
            let mut rec = Rec {
                f: handler_for::<T>(inst.kind),
                a: inst.a,
                b: inst.b,
                c: inst.c,
                d: inst.d,
                e: inst.e,
                g: inst.g,
            };
            match inst.kind {
                Kind::Jump { .. } | Kind::Fall { .. } => {
                    let target = &program.blocks[inst.c as usize];
                    rec.a = target.first;
                    match u32::try_from(target.gas) {
                        Ok(gas) if target.min_len <= 0xffff && target.span <= 0xffff => {
                            rec.b = gas;
                            rec.g = target.min_len | (target.span << 16);
                        }
                        // Always take the checked path.
                        _ => rec.g = 0xffff,
                    }
                }
                Kind::Generic => {
                    rec.g = host_instrs.len() as u32;
                    host_instrs.push(opcodes.external_instruction((inst.b >> 16) as u8));
                }
                _ => {}
            }
            rec
        })
        .collect();
    Linked { program, recs, host_instrs }
}

/// Runs `interp`'s frame on `program`, returning how it stopped and whether it handed off to the
/// interpreter.
pub(crate) fn run<'f, 'h, T: EvmTypesHost>(
    program: &Linked<T>,
    interp: &mut Interpreter<'f, 'h, T>,
    host: &mut T::Host<'h>,
) -> (InstrStop, bool) {
    let gas_params: *const GasParams = &host.execution_config().version().gas_params;
    interp.prepare_run(host);
    // SAFETY: The stack view is dropped before anything else touches the stack.
    let (base, len) = unsafe { interp.stack_mut().into_raw_parts() };
    let len = *len;
    let prog = &program.program;
    let mut cx = Cx {
        interp,
        host,
        base,
        recs: program.recs.as_ptr(),
        consts: prog.consts.as_ptr(),
        blocks: prog.blocks.as_ptr(),
        sections: prog.sections.as_ptr(),
        moves: prog.moves.as_ptr(),
        jump_map: prog.jump_map.as_ptr(),
        host_instrs: program.host_instrs.as_ptr(),
        code_len: prog.jump_map.len(),
        gas_params,
        stop: InstrStop::Stop,
        bailed: false,
    };
    // SAFETY: `len` is the interpreter's own stack length.
    let top = unsafe { base.add(len) };
    // SAFETY: Block 0 starts at pc 0, where every frame starts.
    if let Some(rec) = unsafe { enter(&mut cx, 0, top) } {
        // SAFETY: The context points at the live frame and program.
        unsafe { dispatch(rec, top, &mut cx) };
    }
    (cx.stop, cx.bailed)
}

#[cfg(tco)]
#[inline(always)]
unsafe fn dispatch<T: EvmTypesHost>(rec: *const Rec<T>, top: *mut Word, cx: &mut Cx<'_, '_, T>) {
    // SAFETY: Upheld by the caller.
    unsafe { ((*rec).f)(rec, top, cx) }
}

#[cfg(not(tco))]
#[inline(always)]
unsafe fn dispatch<T: EvmTypesHost>(
    mut rec: *const Rec<T>,
    mut top: *mut Word,
    cx: &mut Cx<'_, '_, T>,
) {
    // SAFETY: Upheld by the caller.
    while let Some((next, next_top)) = unsafe { ((*rec).f)(rec, top, cx) } {
        rec = next;
        top = next_top;
    }
}

/// Charges block `block`'s entry section and returns its first record, or stops.
///
/// A block whose slots and temporaries do not fit above `top` runs on the interpreter instead.
#[inline(always)]
unsafe fn enter<T: EvmTypesHost>(
    cx: &mut Cx<'_, '_, T>,
    block: u32,
    top: *mut Word,
) -> Option<*const Rec<T>> {
    // SAFETY: `block` comes from translation, and `top` is in the stack.
    unsafe {
        let b = &*cx.blocks.add(block as usize);
        let len = top.offset_from(cx.base) as u32;
        if len.wrapping_sub(b.min_len) > b.span {
            return enter_checked(cx, b, len as usize);
        }
        if let Err(stop) = (*cx.interp).gas_mut().spend(b.gas) {
            cx.stop = stop;
            return None;
        }
        Some(cx.recs.add(b.first as usize))
    }
}

/// [`enter`] for an entry stack length that may fail the entry section's checks.
#[cold]
#[inline(never)]
unsafe fn enter_checked<T: EvmTypesHost>(
    cx: &mut Cx<'_, '_, T>,
    b: &Block,
    len: usize,
) -> Option<*const Rec<T>> {
    // SAFETY: The section comes from translation.
    unsafe {
        let s = &*cx.sections.add(b.section as usize);
        if len + s.need as usize > STACK_LIMIT {
            bail_to_interpreter(cx, b.pc, len);
            return None;
        }
        if let Err(stop) = charge(cx, s, len) {
            cx.stop = stop;
            return None;
        }
        Some(cx.recs.add(b.first as usize))
    }
}

/// [`enter`] for a static target whose entry the record `r` carries.
#[inline(always)]
unsafe fn enter_static<T: EvmTypesHost>(
    cx: &mut Cx<'_, '_, T>,
    r: &Rec<T>,
    top: *mut Word,
) -> Option<*const Rec<T>> {
    // SAFETY: The record and its target come from translation, and `top` is in the stack.
    unsafe {
        let len = top.offset_from(cx.base) as u32;
        if len.wrapping_sub(r.g & 0xffff) > r.g >> 16 {
            return enter_checked(cx, &*cx.blocks.add(r.c as usize), len as usize);
        }
        if let Err(stop) = (*cx.interp).gas_mut().spend(r.b as u64) {
            cx.stop = stop;
            return None;
        }
        Some(cx.recs.add(r.a as usize))
    }
}

/// Charges section `s`'s gas and checks its stack bounds, for a block entered at height `len`.
#[inline(always)]
unsafe fn charge<T: EvmTypesHost>(
    cx: &mut Cx<'_, '_, T>,
    s: &Section,
    len: usize,
) -> Result<(), InstrStop> {
    // SAFETY: The interpreter outlives the frame.
    unsafe { (*cx.interp).gas_mut().spend(s.gas)? };
    let height = len as i64 + s.base as i64;
    if height < s.inputs as i64 {
        return Err(InstrStop::StackUnderflow);
    }
    if height + s.max_growth as i64 > STACK_LIMIT as i64 {
        return Err(InstrStop::StackOverflow);
    }
    Ok(())
}

/// Runs the rest of the frame on the interpreter from `pc`, with `len` words on the stack.
#[cold]
#[inline(never)]
unsafe fn bail_to_interpreter<T: EvmTypesHost>(cx: &mut Cx<'_, '_, T>, pc: u32, len: usize) {
    // SAFETY: The interpreter and host outlive the frame.
    unsafe {
        let interp = &mut *cx.interp;
        *interp.stack_mut().into_raw_parts().1 = len;
        interp.set_pc(pc as usize);
        cx.stop = match interp.run(&mut *cx.host) {
            Ok(stop) => stop,
            Err(error) => interp.fail(error),
        };
    }
    cx.bailed = true;
}

#[inline(always)]
unsafe fn operand<'a, T: EvmTypesHost, const IMM: bool>(
    cx: &Cx<'_, '_, T>,
    top: *mut Word,
    raw: u32,
) -> &'a Word {
    // SAFETY: Translation only emits in-bounds constants and slots.
    unsafe {
        if IMM {
            &*cx.consts.cast::<u8>().add(Operand::imm_bytes(raw)).cast::<Word>()
        } else {
            &*top.cast::<u8>().offset(Operand::slot_bytes(raw)).cast::<Word>()
        }
    }
}

#[inline(always)]
unsafe fn operand_dyn<'a, T: EvmTypesHost>(
    cx: &Cx<'_, '_, T>,
    top: *mut Word,
    raw: u32,
) -> &'a Word {
    // SAFETY: Upheld by the caller.
    unsafe {
        if Operand::is_imm(raw) {
            operand::<T, true>(cx, top, raw)
        } else {
            operand::<T, false>(cx, top, raw)
        }
    }
}

#[inline(always)]
unsafe fn slot<'a>(top: *mut Word, raw: u32) -> &'a mut Word {
    // SAFETY: Translation only emits in-bounds slots.
    unsafe { &mut *top.cast::<u8>().offset(Operand::slot_bytes(raw)).cast::<Word>() }
}

/// Runs the moves of an exit or host instruction, starting at `start`.
#[inline(always)]
unsafe fn moves<T: EvmTypesHost>(cx: &Cx<'_, '_, T>, top: *mut Word, start: u32) {
    if start == NO_MOVES {
        return;
    }
    // SAFETY: Moves come from translation and address the block's slots and constants.
    unsafe {
        let count = (*cx.moves.add(start as usize)).dst as usize;
        for i in 0..count {
            run_move(cx, top, &*cx.moves.add(start as usize + 1 + i));
        }
    }
}

/// Runs exactly `N` moves starting at `start`, unrolled.
#[inline(always)]
unsafe fn moves_n<T: EvmTypesHost, const N: u8>(cx: &Cx<'_, '_, T>, top: *mut Word, start: u32) {
    if N > MAX_UNROLLED_MOVES {
        // SAFETY: Upheld by the caller.
        return unsafe { moves(cx, top, start) };
    }
    for i in 0..N as usize {
        // SAFETY: An exit with `N` moves has them after its header.
        unsafe { run_move(cx, top, &*cx.moves.add(start as usize + 1 + i)) };
    }
}

#[inline(always)]
unsafe fn run_move<T: EvmTypesHost>(cx: &Cx<'_, '_, T>, top: *mut Word, m: &Move) {
    // SAFETY: Upheld by the caller.
    unsafe {
        let value = *operand_dyn(cx, top, m.src);
        top.cast::<u8>().offset(m.dst as isize).cast::<Word>().write(value);
    }
}

/// Returns the stack top an exit with height field `e` leaves.
#[inline(always)]
const unsafe fn exit_top(top: *mut Word, e: u32) -> *mut Word {
    // SAFETY: Exit heights stay within the stack.
    unsafe { top.offset(e as i32 as isize) }
}

/// Returns the block a dynamic jump to `dest` enters, if `dest` is a valid jump destination.
#[inline(always)]
fn jump_block<T: EvmTypesHost>(cx: &Cx<'_, '_, T>, dest: &Word) -> Option<u32> {
    let pc = usize::try_from(*dest).ok().filter(|&pc| pc < cx.code_len)?;
    // SAFETY: `pc` is in bounds of the jump map.
    let block = unsafe { *cx.jump_map.add(pc) };
    (block != INVALID_BLOCK).then_some(block)
}

#[inline(always)]
fn address_word(address: &alloy_primitives::Address) -> Word {
    Word::from_be_bytes(address.into_word().0)
}

/// Resizes memory for an access of `len` bytes at `offset`.
#[inline(always)]
unsafe fn resize<T: EvmTypesHost>(
    cx: &mut Cx<'_, '_, T>,
    offset: &Word,
    len: usize,
) -> Result<usize, InstrStop> {
    let offset = offset.to_usize()?;
    // SAFETY: The interpreter outlives the frame; gas and memory are disjoint fields.
    unsafe {
        let interp = &mut *cx.interp;
        let gas: *mut Gas = interp.gas_mut();
        interp.memory_mut().resize_evm(&mut *gas, &*cx.gas_params, offset, len)?;
    }
    Ok(offset)
}

handler! {
    fn alu2[T: EvmTypesHost, const OP: u8, const NEG: u8, const AI: bool, const BI: bool](rec, top, cx) {
        // SAFETY: Records and slots come from translation.
        unsafe {
            let r = &*rec;
            let value = eval2(OP, NEG, operand::<T, AI>(cx, top, r.a), operand::<T, BI>(cx, top, r.b));
            slot(top, r.c).write(value);
        }
        next!(unsafe { rec.add(1) }, top, cx)
    }
}

handler! {
    fn alu1[T: EvmTypesHost, const OP: u8, const NEG: u8](rec, top, cx) {
        // SAFETY: Records and slots come from translation.
        unsafe {
            let r = &*rec;
            let value = eval1(OP, NEG, operand::<T, false>(cx, top, r.a));
            slot(top, r.c).write(value);
        }
        next!(unsafe { rec.add(1) }, top, cx)
    }
}

handler! {
    fn mload[T: EvmTypesHost, const AI: bool](rec, top, cx) {
        // SAFETY: Records and slots come from translation.
        unsafe {
            let r = &*rec;
            let offset = match resize(cx, operand::<T, AI>(cx, top, r.a), 32) {
                Ok(offset) => offset,
                Err(stop) => stop!(cx, stop),
            };
            let value = (*cx.interp).memory().get_word(offset);
            *slot(top, r.c) = value;
        }
        next!(unsafe { rec.add(1) }, top, cx)
    }
}

handler! {
    fn mstore[T: EvmTypesHost, const OP: u8, const AI: bool, const BI: bool](rec, top, cx) {
        // SAFETY: Records and slots come from translation.
        unsafe {
            let r = &*rec;
            let value = *operand::<T, BI>(cx, top, r.b);
            let len = if OP == op::MSTORE8 { 1 } else { 32 };
            let offset = match resize(cx, operand::<T, AI>(cx, top, r.a), len) {
                Ok(offset) => offset,
                Err(stop) => stop!(cx, stop),
            };
            let memory = (*cx.interp).memory_mut();
            if OP == op::MSTORE8 {
                memory.set(offset, &[value.byte(0)]);
            } else {
                memory.set(offset, &value.to_be_bytes::<32>());
            }
        }
        next!(unsafe { rec.add(1) }, top, cx)
    }
}

handler! {
    fn env[T: EvmTypesHost, const OP: u8](rec, top, cx) {
        // SAFETY: Records and slots come from translation, and the frame's message is live.
        unsafe {
            let r = &*rec;
            let interp = &*cx.interp;
            let message = interp.message();
            let value = match OP {
                op::ADDRESS => address_word(&message.destination),
                op::CALLER => address_word(&message.caller),
                op::CALLVALUE => message.value,
                op::CALLDATASIZE => Word::from(message.input.len()),
                op::RETURNDATASIZE => Word::from(interp.return_data().len()),
                _ => unreachable!(),
            };
            slot(top, r.c).write(value);
        }
        next!(unsafe { rec.add(1) }, top, cx)
    }
}

handler! {
    fn calldataload[T: EvmTypesHost, const AI: bool](rec, top, cx) {
        // SAFETY: Records and slots come from translation, and the frame's message is live.
        unsafe {
            let r = &*rec;
            let offset = operand::<T, AI>(cx, top, r.a);
            let input = &(*cx.interp).message().input;
            let mut word = [0u8; 32];
            if let Ok(offset) = usize::try_from(*offset)
                && offset < input.len()
            {
                let len = (input.len() - offset).min(32);
                word[..len].copy_from_slice(&input[offset..offset + len]);
            }
            *slot(top, r.c) = Word::from_be_bytes(word);
        }
        next!(unsafe { rec.add(1) }, top, cx)
    }
}

handler! {
    fn jump[T: EvmTypesHost, const N: u8](rec, top, cx) {
        // SAFETY: Records come from translation.
        let (next, top) = unsafe {
            let r = &*rec;
            moves_n::<T, N>(cx, top, r.d);
            let top = exit_top(top, r.e);
            (enter_static(cx, r, top), top)
        };
        match next {
            Some(rec) => next!(rec, top, cx),
            None => exit!(),
        }
    }
}

handler! {
    fn jump_dyn[T: EvmTypesHost](rec, top, cx) {
        // SAFETY: Records come from translation.
        let (dest, top) = unsafe {
            let r = &*rec;
            let dest = *operand_dyn(cx, top, r.a);
            moves(cx, top, r.d);
            (dest, exit_top(top, r.e))
        };
        match jump_block(cx, &dest) {
            Some(block) => goto!(cx, block, top),
            None => stop!(cx, InstrStop::InvalidJump),
        }
    }
}

handler! {
    fn br[
        T: EvmTypesHost,
        const CMP: u8,
        const NEG: u8,
        const CONT: bool,
        const AI: bool,
        const BI: bool,
    ](rec, top, cx) {
        // SAFETY: Records come from translation.
        let (taken, r) = unsafe {
            let r = &*rec;
            (cond(CMP, NEG, operand::<T, AI>(cx, top, r.a), operand::<T, BI>(cx, top, r.b)), r)
        };
        if taken {
            // SAFETY: As above.
            let top = unsafe {
                moves(cx, top, r.d);
                exit_top(top, r.e)
            };
            if r.c == INVALID_BLOCK {
                stop!(cx, InstrStop::InvalidJump);
            }
            goto!(cx, r.c, top)
        }
        if CONT {
            if r.g != NO_SECTION {
                // SAFETY: Sections come from translation.
                let result = unsafe {
                    let s = &*cx.sections.add(r.g as usize);
                    charge(cx, s, top.offset_from(cx.base) as usize)
                };
                if let Err(stop) = result {
                    stop!(cx, stop);
                }
            }
            next!(unsafe { rec.add(1) }, top, cx)
        }
        // SAFETY: As above.
        let top = unsafe {
            moves(cx, top, r.d);
            exit_top(top, r.e)
        };
        goto!(cx, r.g, top)
    }
}

handler! {
    fn br_dyn[T: EvmTypesHost](rec, top, cx) {
        // SAFETY: Records come from translation.
        let (taken, dest, fall, top) = unsafe {
            let r = &*rec;
            let taken = !operand_dyn(cx, top, r.a).is_zero();
            let dest = *operand_dyn(cx, top, r.b);
            moves(cx, top, r.d);
            (taken, dest, r.g, exit_top(top, r.e))
        };
        if taken {
            match jump_block(cx, &dest) {
                Some(block) => goto!(cx, block, top),
                None => stop!(cx, InstrStop::InvalidJump),
            }
        }
        goto!(cx, fall, top)
    }
}

handler! {
    fn generic[T: EvmTypesHost](rec, top, cx) {
        // SAFETY: Records come from translation, and the interpreter outlives the frame.
        unsafe {
            let r = &*rec;
            moves(cx, top, r.d);
            let len = exit_top(top, r.e).offset_from(cx.base) as usize;
            let interp = &mut *cx.interp;
            let instr = *cx.host_instrs.add(r.g as usize);
            match interp.run_resolved_instruction(instr, r.a as usize, len) {
                Ok(new_len) => {
                    let (inputs, outputs) = ((r.b >> 8 & 0xff) as usize, (r.b & 0xff) as usize);
                    let expected = len.wrapping_sub(inputs) + outputs;
                    if new_len != expected {
                        let stop = interp.fail_host_instruction(
                            r.a as usize,
                            format_args!(
                                "left {new_len} stack items, but the superinstruction backend \
                                 expects {expected} from the opcode's fixed inputs and outputs"
                            ),
                        );
                        stop!(cx, stop);
                    }
                }
                Err(stop) => stop!(cx, stop),
            }
            if r.c != NO_SECTION {
                let s = &*cx.sections.add(r.c as usize);
                if let Err(stop) = charge(cx, s, top.offset_from(cx.base) as usize) {
                    stop!(cx, stop);
                }
            }
        }
        next!(unsafe { rec.add(1) }, top, cx)
    }
}

handler! {
    fn halt[T: EvmTypesHost](rec, top, cx) {
        let _ = top;
        // SAFETY: Records come from translation.
        let halt = unsafe { (*rec).a };
        let stop = match halt {
            h if h == Halt::InvalidJump as u32 => InstrStop::InvalidJump,
            h if h == Halt::InvalidFE as u32 => InstrStop::InvalidFEOpcode,
            _ => InstrStop::Stop,
        };
        stop!(cx, stop)
    }
}

handler! {
    fn bail[T: EvmTypesHost](rec, top, cx) {
        // SAFETY: Records come from translation.
        unsafe {
            let r = &*rec;
            moves(cx, top, r.d);
            let len = exit_top(top, r.e).offset_from(cx.base) as usize;
            bail_to_interpreter(cx, r.a, len);
        }
        exit!()
    }
}

macro_rules! modes {
    ($f:ident, $T:ty, [$($c:tt)*], $a:expr, $b:expr) => {
        match ($a, $b) {
            (false, false) => $f::<$T, $($c)* false, false> as HandlerFn<$T>,
            (false, true) => $f::<$T, $($c)* false, true> as HandlerFn<$T>,
            (true, false) => $f::<$T, $($c)* true, false> as HandlerFn<$T>,
            (true, true) => $f::<$T, $($c)* true, true> as HandlerFn<$T>,
        }
    };
}

fn handler_for<T: EvmTypesHost>(kind: Kind) -> HandlerFn<T> {
    match kind {
        Kind::Alu2 { op, neg, a_imm, b_imm } => alu2_fn::<T>(op, neg, a_imm, b_imm),
        Kind::Alu1 { op: op::ISZERO, neg: 0 } => alu1::<T, { op::ISZERO }, 0>,
        Kind::Alu1 { op: op::ISZERO, .. } => alu1::<T, { op::ISZERO }, 1>,
        Kind::Alu1 { .. } => alu1::<T, { op::NOT }, 0>,
        Kind::Mload { a_imm: false } => mload::<T, false>,
        Kind::Mload { a_imm: true } => mload::<T, true>,
        Kind::Mstore { op: op::MSTORE8, a_imm, b_imm } => {
            modes!(mstore, T, [{ op::MSTORE8 },], a_imm, b_imm)
        }
        Kind::Mstore { a_imm, b_imm, .. } => modes!(mstore, T, [{ op::MSTORE },], a_imm, b_imm),
        Kind::Env { op: op::ADDRESS } => env::<T, { op::ADDRESS }>,
        Kind::Env { op: op::CALLER } => env::<T, { op::CALLER }>,
        Kind::Env { op: op::CALLVALUE } => env::<T, { op::CALLVALUE }>,
        Kind::Env { op: op::CALLDATASIZE } => env::<T, { op::CALLDATASIZE }>,
        Kind::Env { .. } => env::<T, { op::RETURNDATASIZE }>,
        Kind::CallDataLoad { a_imm: false } => calldataload::<T, false>,
        Kind::CallDataLoad { a_imm: true } => calldataload::<T, true>,
        Kind::Jump { moves } | Kind::Fall { moves } => match moves {
            0 => jump::<T, 0>,
            1 => jump::<T, 1>,
            2 => jump::<T, 2>,
            3 => jump::<T, 3>,
            4 => jump::<T, 4>,
            5 => jump::<T, 5>,
            6 => jump::<T, 6>,
            7 => jump::<T, 7>,
            8 => jump::<T, 8>,
            _ => jump::<T, { MAX_UNROLLED_MOVES + 1 }>,
        },
        Kind::JumpDyn => jump_dyn::<T>,
        Kind::Br { cmp, neg, cont, a_imm, b_imm } => br_fn::<T>(cmp, neg, cont, a_imm, b_imm),
        Kind::BrDyn => br_dyn::<T>,
        Kind::Generic => generic::<T>,
        Kind::Halt => halt::<T>,
        Kind::Bail => bail::<T>,
    }
}

fn alu2_fn<T: EvmTypesHost>(opcode: u8, neg: u8, a: bool, b: bool) -> HandlerFn<T> {
    macro_rules! select {
        (plain: [$($p:ident),*], cond: [$($c:ident),*], and: [$and:ident]) => {
            match (opcode, neg) {
                $( (op::$p, _) => modes!(alu2, T, [{ op::$p }, 0,], a, b), )*
                $(
                    (op::$c, 0) => modes!(alu2, T, [{ op::$c }, 0,], a, b),
                    (op::$c, _) => modes!(alu2, T, [{ op::$c }, 1,], a, b),
                )*
                (op::$and, 0) => modes!(alu2, T, [{ op::$and }, 0,], a, b),
                (op::$and, 1) => modes!(alu2, T, [{ op::$and }, 1,], a, b),
                (op::$and, _) => modes!(alu2, T, [{ op::$and }, 2,], a, b),
                _ => unreachable!("not a binary superinstruction: {opcode:#x}"),
            }
        };
    }
    select!(
        plain: [ADD, MUL, SUB, DIV, SDIV, MOD, SMOD, SIGNEXTEND, OR, XOR, BYTE, SHL, SHR, SAR],
        cond: [LT, GT, SLT, SGT, EQ],
        and: [AND]
    )
}

fn br_fn<T: EvmTypesHost>(cmp: u8, neg: u8, cont: bool, a: bool, b: bool) -> HandlerFn<T> {
    // For branching, `AND`'s non-zero form (2) is its plain form.
    let neg = (neg == 1) as u8;
    macro_rules! select {
        ($($c:path),*) => {
            match (cmp, neg, cont) {
                $(
                    ($c, 0, false) => modes!(br, T, [{ $c }, 0, false,], a, b),
                    ($c, 0, true) => modes!(br, T, [{ $c }, 0, true,], a, b),
                    ($c, _, false) => modes!(br, T, [{ $c }, 1, false,], a, b),
                    ($c, _, true) => modes!(br, T, [{ $c }, 1, true,], a, b),
                )*
                _ => unreachable!("not a branch compare: {cmp:#x}"),
            }
        };
    }
    select!(op::EQ, op::LT, op::GT, op::SLT, op::SGT, op::AND, CMP_NZ)
}
