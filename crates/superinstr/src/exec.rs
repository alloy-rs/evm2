//! Threaded execution of linked programs.
//!
//! Each record links to a handler specialized for its kind and operand modes. Handlers run on
//! the interpreter's own stack, addressing slots relative to the stack top at block entry, and
//! pass control to the next record with a tail call on nightly compilers, or by returning it to
//! a dispatch loop otherwise.
//!
//! Handlers do not check bounds on the hot paths: records, operands and moves come from
//! translating the running program, which only emits in-bounds ones, and entering a block checks
//! that its slots fit the stack. [`Cx`], [`RecPtr`] and [`Top`] hold that invariant, and their
//! methods are the only code that relies on it. In debug builds they check it.

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
pub(crate) type Next<T> = Option<(RecPtr<T>, Top)>;

/// A record handler.
#[cfg(tco)]
pub(crate) type HandlerFn<T> =
    for<'c, 'p, 'f, 'h> extern "rust-preserve-none" fn(RecPtr<T>, Top, &'c mut Cx<'p, 'f, 'h, T>);

/// A record handler.
#[cfg(not(tco))]
pub(crate) type HandlerFn<T> =
    for<'c, 'p, 'f, 'h> fn(RecPtr<T>, Top, &'c mut Cx<'p, 'f, 'h, T>) -> Next<T>;

/// Points at a record of the running program.
///
/// Only [`Cx`] makes these, from the program's records. Translation ends every block with a record
/// that leaves it, so [`Self::next`] of a record that continues is a record too.
#[repr(transparent)]
pub(crate) struct RecPtr<T: EvmTypesHost>(*const Rec<T>);

impl<T: EvmTypesHost> Clone for RecPtr<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: EvmTypesHost> Copy for RecPtr<T> {}

impl<T: EvmTypesHost> RecPtr<T> {
    /// Returns the record after this one.
    #[inline(always)]
    const fn next(self) -> Self {
        Self(self.0.wrapping_add(1))
    }
}

/// The stack top at block entry, which slot operands are relative to.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub(crate) struct Top(*mut Word);

impl Top {
    /// Returns the top an exit with height field `e` leaves.
    #[inline(always)]
    const fn exit(self, e: u32) -> Self {
        Self(self.0.wrapping_offset(e as i32 as isize))
    }
}

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
///
/// # Invariant
///
/// For as long as the context lives:
///
/// - `interp` and `host` are the running frame's interpreter and host, which [`run`] lends the
///   context mutably. Only [`Self::interp`] and [`Self::bail`] reach them, for the length of one
///   call, as the interpreter's own dispatch loop does.
/// - `base` is the interpreter's stack. Every slot a record addresses, relative to a [`Top`] that
///   entering its block produced, lies within the stack and holds an initialized word when read:
///   translation only emits slots within the block's entry inputs and its `need`, and
///   [`Self::enter`] checks the entry length against both.
/// - [`RecPtr`]s point into `recs`, and records' constant and move indices into `consts` and
///   `moves`.
pub(crate) struct Cx<'p, 'f, 'h, T: EvmTypesHost> {
    interp: *mut Interpreter<'f, 'h, T>,
    host: *mut T::Host<'h>,
    base: *mut Word,
    recs: &'p [Rec<T>],
    consts: &'p [Word],
    blocks: &'p [Block],
    sections: &'p [Section],
    moves: &'p [Move],
    jump_map: &'p [u32],
    host_instrs: &'p [ExternalInstruction<T>],
    gas_params: *const GasParams,
    stop: InstrStop,
    bailed: bool,
}

impl<'p, 'f, 'h, T: EvmTypesHost> Cx<'p, 'f, 'h, T> {
    /// Returns the record `p` points at.
    #[inline(always)]
    fn rec(&self, p: RecPtr<T>) -> &'p Rec<T> {
        debug_assert!(self.recs.as_ptr_range().contains(&p.0), "record out of the program");
        // SAFETY: Record pointers point into `recs` (see the invariant).
        unsafe { &*p.0 }
    }

    /// Returns a pointer to record `index`.
    #[inline(always)]
    const fn first(&self, index: u32) -> RecPtr<T> {
        RecPtr(self.recs.as_ptr().wrapping_add(index as usize))
    }

    /// Returns the stack length at `top`.
    #[inline(always)]
    fn len(&self, top: Top) -> usize {
        top.0.addr().wrapping_sub(self.base.addr()) / size_of::<Word>()
    }

    #[inline(always)]
    fn slot_ptr(&self, top: Top, raw: u32) -> *mut Word {
        let p = top.0.cast::<u8>().wrapping_offset(Operand::slot_bytes(raw)).cast::<Word>();
        debug_assert!(
            p >= self.base && p < self.base.wrapping_add(STACK_LIMIT),
            "slot out of the stack"
        );
        p
    }

    /// Returns operand `raw`, an immediate if `IMM`, else a slot.
    #[inline(always)]
    fn operand<const IMM: bool>(&self, top: Top, raw: u32) -> &Word {
        if IMM {
            debug_assert!(
                Operand::imm_bytes(raw) / 32 < self.consts.len(),
                "constant out of range"
            );
            // SAFETY: Immediates index `consts` (see the invariant).
            unsafe {
                &*self.consts.as_ptr().cast::<u8>().add(Operand::imm_bytes(raw)).cast::<Word>()
            }
        } else {
            // SAFETY: The slot is in the stack and initialized (see the invariant). Writes take
            // `&mut self`, so no write overlaps the returned borrow.
            unsafe { &*self.slot_ptr(top, raw) }
        }
    }

    /// Returns operand `raw`, of either kind.
    #[inline(always)]
    fn operand_dyn(&self, top: Top, raw: u32) -> &Word {
        if Operand::is_imm(raw) {
            self.operand::<true>(top, raw)
        } else {
            self.operand::<false>(top, raw)
        }
    }

    /// Writes a computed `value` to slot `raw`, with a single store (see [`WordExt::write`]).
    #[inline(always)]
    fn write(&mut self, top: Top, raw: u32, value: Word) {
        // SAFETY: The slot is in the stack (see the invariant), and `&mut self` excludes live
        // borrows of it.
        unsafe { (*self.slot_ptr(top, raw)).write(value) }
    }

    /// Writes a copied `value` to slot `raw`.
    #[inline(always)]
    fn store(&mut self, top: Top, raw: u32, value: Word) {
        // SAFETY: As in [`Self::write`].
        unsafe { self.slot_ptr(top, raw).write(value) }
    }

    #[inline(always)]
    fn move_at(&self, index: usize) -> Move {
        debug_assert!(index < self.moves.len(), "move out of range");
        // SAFETY: Exits index `moves` (see the invariant).
        unsafe { *self.moves.get_unchecked(index) }
    }

    /// Runs the moves of an exit or host instruction, starting at `start`.
    #[inline(always)]
    fn moves(&mut self, top: Top, start: u32) {
        if start == NO_MOVES {
            return;
        }
        let count = self.move_at(start as usize).dst as usize;
        for i in 0..count {
            self.run_move(top, start as usize + 1 + i);
        }
    }

    /// Runs exactly `N` moves starting at `start`, unrolled.
    #[inline(always)]
    fn moves_n<const N: u8>(&mut self, top: Top, start: u32) {
        if N > MAX_UNROLLED_MOVES {
            return self.moves(top, start);
        }
        for i in 0..N as usize {
            self.run_move(top, start as usize + 1 + i);
        }
    }

    #[inline(always)]
    fn run_move(&mut self, top: Top, index: usize) {
        let m = self.move_at(index);
        let value = *self.operand_dyn(top, m.src);
        self.store(top, m.dst as u32, value);
    }

    /// Returns the frame's interpreter.
    #[inline(always)]
    fn interp(&mut self) -> &mut Interpreter<'f, 'h, T> {
        // SAFETY: [`run`] lends the context the interpreter, and the result borrows `self`.
        unsafe { &mut *self.interp }
    }

    /// Resizes memory for an access of `len` bytes at `offset`.
    #[inline(always)]
    fn resize(&mut self, offset: Word, len: usize) -> Result<usize, InstrStop> {
        let offset = offset.to_usize()?;
        let gas_params = self.gas_params;
        let interp = self.interp();
        let gas: *mut Gas = interp.gas_mut();
        // SAFETY: Gas and memory are disjoint fields of the interpreter, as in its own memory
        // instructions. The gas parameters belong to the host's execution config, which no
        // instruction changes.
        unsafe { interp.memory_mut().resize_evm(&mut *gas, &*gas_params, offset, len)? };
        Ok(offset)
    }

    /// Charges block `block`'s entry section and returns its first record, or stops.
    ///
    /// A block whose slots and temporaries do not fit above `top` runs on the interpreter instead.
    #[inline(always)]
    fn enter(&mut self, block: u32, top: Top) -> Option<RecPtr<T>> {
        let blocks = self.blocks;
        let b = &blocks[block as usize];
        let len = self.len(top) as u32;
        if len.wrapping_sub(b.min_len) > b.span {
            return self.enter_checked(b, len as usize);
        }
        if let Err(stop) = self.interp().gas_mut().spend(b.gas) {
            self.stop = stop;
            return None;
        }
        Some(self.first(b.first))
    }

    /// [`Self::enter`] for an entry stack length that may fail the entry section's checks.
    #[cold]
    #[inline(never)]
    fn enter_checked(&mut self, b: &Block, len: usize) -> Option<RecPtr<T>> {
        let sections = self.sections;
        let s = &sections[b.section as usize];
        if len + s.need as usize > STACK_LIMIT {
            self.bail(b.pc, len);
            return None;
        }
        if let Err(stop) = self.charge(s, len) {
            self.stop = stop;
            return None;
        }
        Some(self.first(b.first))
    }

    /// [`Self::enter`] for a static target whose entry the record `r` carries.
    #[inline(always)]
    fn enter_static(&mut self, r: &Rec<T>, top: Top) -> Option<RecPtr<T>> {
        let len = self.len(top) as u32;
        if len.wrapping_sub(r.g & 0xffff) > r.g >> 16 {
            let blocks = self.blocks;
            return self.enter_checked(&blocks[r.c as usize], len as usize);
        }
        if let Err(stop) = self.interp().gas_mut().spend(r.b as u64) {
            self.stop = stop;
            return None;
        }
        Some(self.first(r.a))
    }

    /// Charges section `s`'s gas and checks its stack bounds, for a block entered at height `len`.
    #[inline(always)]
    fn charge(&mut self, s: &Section, len: usize) -> Result<(), InstrStop> {
        self.interp().gas_mut().spend(s.gas)?;
        let height = len as i64 + s.base as i64;
        if height < s.inputs as i64 {
            return Err(InstrStop::StackUnderflow);
        }
        if height + s.max_growth as i64 > STACK_LIMIT as i64 {
            return Err(InstrStop::StackOverflow);
        }
        Ok(())
    }

    /// Charges section `index` for the block entered at `top`.
    #[inline(always)]
    fn charge_section(&mut self, index: u32, top: Top) -> Result<(), InstrStop> {
        let sections = self.sections;
        self.charge(&sections[index as usize], self.len(top))
    }

    /// Returns the block a dynamic jump to `dest` enters, if `dest` is a valid jump destination.
    #[inline(always)]
    fn jump_block(&self, dest: &Word) -> Option<u32> {
        let &block = self.jump_map.get(usize::try_from(*dest).ok()?)?;
        (block != INVALID_BLOCK).then_some(block)
    }

    /// Runs the host instruction of record `r` on the canonical stack.
    #[inline(always)]
    fn host_call(&mut self, r: &Rec<T>, top: Top) -> Result<(), InstrStop> {
        self.moves(top, r.d);
        let len = self.len(top.exit(r.e));
        let instr = self.host_instrs[r.g as usize];
        let interp = self.interp();
        // SAFETY: Translation emits host instructions only for in-bounds pcs, linking resolved
        // `instr` from the opcode at `r.a`, and after the moves the stack holds `len` initialized
        // words.
        let new_len = unsafe { interp.run_resolved_instruction(instr, r.a as usize, len) }?;
        let (inputs, outputs) = ((r.b >> 8 & 0xff) as usize, (r.b & 0xff) as usize);
        let expected = len.wrapping_sub(inputs) + outputs;
        if new_len != expected {
            return Err(interp.fail_host_instruction(
                r.a as usize,
                format_args!(
                    "left {new_len} stack items, but the superinstruction backend expects \
                     {expected} from the opcode's fixed inputs and outputs"
                ),
            ));
        }
        if r.c != NO_SECTION {
            self.charge_section(r.c, top)?;
        }
        Ok(())
    }

    /// Runs the rest of the frame on the interpreter from `pc`, with `len` words on the stack.
    #[cold]
    #[inline(never)]
    fn bail(&mut self, pc: u32, len: usize) {
        let host = self.host;
        let interp = self.interp();
        // SAFETY: `len` is within the stack and its words are initialized (see the invariant).
        unsafe { *interp.stack_mut().into_raw_parts().1 = len };
        interp.set_pc(pc as usize);
        // SAFETY: [`run`] lends the context the host, and nothing else uses it while the
        // interpreter runs.
        let stop = match interp.run(unsafe { &mut *host }) {
            Ok(stop) => stop,
            Err(error) => interp.fail(error),
        };
        self.stop = stop;
        self.bailed = true;
    }
}

macro_rules! handler {
    ($(#[$attr:meta])* fn $name:ident[$($g:tt)*]($rec:ident, $top:ident, $cx:ident) $body:block) => {
        #[cfg(tco)]
        $(#[$attr])*
        #[allow(clippy::missing_const_for_fn)]
        extern "rust-preserve-none" fn $name<$($g)*>(
            $rec: RecPtr<T>,
            $top: Top,
            $cx: &mut Cx<'_, '_, '_, T>,
        ) $body

        #[cfg(not(tco))]
        $(#[$attr])*
        #[allow(clippy::missing_const_for_fn)]
        fn $name<$($g)*>($rec: RecPtr<T>, $top: Top, $cx: &mut Cx<'_, '_, '_, T>) -> Next<T>
            $body
    };
}

#[cfg(tco)]
macro_rules! next {
    ($rec:expr, $top:expr, $cx:expr) => {{
        let rec = $rec;
        let f = $cx.rec(rec).f;
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
        match $cx.enter($block, top) {
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
    // SAFETY: The view is dropped at once. Handlers keep the length within the stack and read
    // only words that the caller or they wrote.
    let (base, len) = unsafe { interp.stack_mut().into_raw_parts() };
    let len = *len;
    let prog = &program.program;
    let mut cx = Cx {
        interp,
        host,
        base,
        recs: &program.recs,
        consts: &prog.consts,
        blocks: &prog.blocks,
        sections: &prog.sections,
        moves: &prog.moves,
        jump_map: &prog.jump_map,
        host_instrs: &program.host_instrs,
        gas_params,
        stop: InstrStop::Stop,
        bailed: false,
    };
    let top = Top(base.wrapping_add(len));
    // Block 0 starts at pc 0, where every frame starts.
    if let Some(rec) = cx.enter(0, top) {
        dispatch(rec, top, &mut cx);
    }
    (cx.stop, cx.bailed)
}

#[cfg(tco)]
#[inline(always)]
fn dispatch<T: EvmTypesHost>(rec: RecPtr<T>, top: Top, cx: &mut Cx<'_, '_, '_, T>) {
    (cx.rec(rec).f)(rec, top, cx)
}

#[cfg(not(tco))]
#[inline(always)]
fn dispatch<T: EvmTypesHost>(mut rec: RecPtr<T>, mut top: Top, cx: &mut Cx<'_, '_, '_, T>) {
    while let Some((next, next_top)) = (cx.rec(rec).f)(rec, top, cx) {
        rec = next;
        top = next_top;
    }
}

#[inline(always)]
fn address_word(address: &alloy_primitives::Address) -> Word {
    Word::from_be_bytes(address.into_word().0)
}

handler! {
    fn alu2[T: EvmTypesHost, const OP: u8, const NEG: u8, const AI: bool, const BI: bool](rec, top, cx) {
        let r = cx.rec(rec);
        let value = eval2(OP, NEG, cx.operand::<AI>(top, r.a), cx.operand::<BI>(top, r.b));
        cx.write(top, r.c, value);
        next!(rec.next(), top, cx)
    }
}

handler! {
    fn alu1[T: EvmTypesHost, const OP: u8, const NEG: u8](rec, top, cx) {
        let r = cx.rec(rec);
        let value = eval1(OP, NEG, cx.operand::<false>(top, r.a));
        cx.write(top, r.c, value);
        next!(rec.next(), top, cx)
    }
}

handler! {
    fn mload[T: EvmTypesHost, const AI: bool](rec, top, cx) {
        let r = cx.rec(rec);
        let offset = match cx.resize(*cx.operand::<AI>(top, r.a), 32) {
            Ok(offset) => offset,
            Err(stop) => stop!(cx, stop),
        };
        let value = cx.interp().memory().get_word(offset);
        cx.store(top, r.c, value);
        next!(rec.next(), top, cx)
    }
}

handler! {
    fn mstore[T: EvmTypesHost, const OP: u8, const AI: bool, const BI: bool](rec, top, cx) {
        let r = cx.rec(rec);
        let value = *cx.operand::<BI>(top, r.b);
        let len = if OP == op::MSTORE8 { 1 } else { 32 };
        let offset = match cx.resize(*cx.operand::<AI>(top, r.a), len) {
            Ok(offset) => offset,
            Err(stop) => stop!(cx, stop),
        };
        let memory = cx.interp().memory_mut();
        if OP == op::MSTORE8 {
            memory.set(offset, &[value.byte(0)]);
        } else {
            memory.set(offset, &value.to_be_bytes::<32>());
        }
        next!(rec.next(), top, cx)
    }
}

handler! {
    fn env[T: EvmTypesHost, const OP: u8](rec, top, cx) {
        let r = cx.rec(rec);
        let interp = cx.interp();
        let message = interp.message();
        let value = match OP {
            op::ADDRESS => address_word(&message.destination),
            op::CALLER => address_word(&message.caller),
            op::CALLVALUE => message.value,
            op::CALLDATASIZE => Word::from(message.input.len()),
            op::RETURNDATASIZE => Word::from(interp.return_data().len()),
            _ => unreachable!(),
        };
        cx.write(top, r.c, value);
        next!(rec.next(), top, cx)
    }
}

handler! {
    fn calldataload[T: EvmTypesHost, const AI: bool](rec, top, cx) {
        let r = cx.rec(rec);
        let offset = *cx.operand::<AI>(top, r.a);
        let input = &cx.interp().message().input;
        let mut word = [0u8; 32];
        if let Ok(offset) = usize::try_from(offset)
            && offset < input.len()
        {
            let len = (input.len() - offset).min(32);
            word[..len].copy_from_slice(&input[offset..offset + len]);
        }
        cx.store(top, r.c, Word::from_be_bytes(word));
        next!(rec.next(), top, cx)
    }
}

handler! {
    fn jump[T: EvmTypesHost, const N: u8](rec, top, cx) {
        let r = cx.rec(rec);
        cx.moves_n::<N>(top, r.d);
        let top = top.exit(r.e);
        match cx.enter_static(r, top) {
            Some(rec) => next!(rec, top, cx),
            None => exit!(),
        }
    }
}

handler! {
    fn jump_dyn[T: EvmTypesHost](rec, top, cx) {
        let r = cx.rec(rec);
        let dest = *cx.operand_dyn(top, r.a);
        cx.moves(top, r.d);
        let top = top.exit(r.e);
        match cx.jump_block(&dest) {
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
        let r = cx.rec(rec);
        if cond(CMP, NEG, cx.operand::<AI>(top, r.a), cx.operand::<BI>(top, r.b)) {
            cx.moves(top, r.d);
            if r.c == INVALID_BLOCK {
                stop!(cx, InstrStop::InvalidJump);
            }
            goto!(cx, r.c, top.exit(r.e))
        }
        if CONT {
            if r.g != NO_SECTION
                && let Err(stop) = cx.charge_section(r.g, top)
            {
                stop!(cx, stop);
            }
            next!(rec.next(), top, cx)
        }
        cx.moves(top, r.d);
        goto!(cx, r.g, top.exit(r.e))
    }
}

handler! {
    fn br_dyn[T: EvmTypesHost](rec, top, cx) {
        let r = cx.rec(rec);
        let taken = !cx.operand_dyn(top, r.a).is_zero();
        let dest = *cx.operand_dyn(top, r.b);
        cx.moves(top, r.d);
        let top = top.exit(r.e);
        if taken {
            match cx.jump_block(&dest) {
                Some(block) => goto!(cx, block, top),
                None => stop!(cx, InstrStop::InvalidJump),
            }
        }
        goto!(cx, r.g, top)
    }
}

handler! {
    fn generic[T: EvmTypesHost](rec, top, cx) {
        let r = cx.rec(rec);
        if let Err(stop) = cx.host_call(r, top) {
            stop!(cx, stop);
        }
        next!(rec.next(), top, cx)
    }
}

handler! {
    fn halt[T: EvmTypesHost](rec, top, cx) {
        let _ = top;
        let stop = match cx.rec(rec).a {
            h if h == Halt::InvalidJump as u32 => InstrStop::InvalidJump,
            h if h == Halt::InvalidFE as u32 => InstrStop::InvalidFEOpcode,
            _ => InstrStop::Stop,
        };
        stop!(cx, stop)
    }
}

handler! {
    fn bail[T: EvmTypesHost](rec, top, cx) {
        let r = cx.rec(rec);
        cx.moves(top, r.d);
        let len = cx.len(top.exit(r.e));
        cx.bail(r.a, len);
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
