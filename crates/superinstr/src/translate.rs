//! Linear-time translation of EVM bytecode into records.
//!
//! One forward pass per basic block keeps a virtual stack: `PUSH` becomes an immediate, `DUP`,
//! `SWAP` and `POP` rename entries, and each computing instruction becomes one record that reads
//! its operands from slots or immediates and writes a slot. Pure operations on immediates fold,
//! and a compare (with any `ISZERO`s after it) fuses into the `JUMPI` that consumes it. Before an
//! exit or a host instruction the virtual stack is written back, so every block starts and ends
//! with the canonical EVM stack.

use crate::program::{
    Block, CMP_NZ, Halt, INVALID_BLOCK, Inst, Kind, MAX_UNROLLED_MOVES, Move, NO_MOVES, NO_SECTION,
    Operand, Program, Section, eval1, eval2, flip, is_alu2, is_cond, is_env,
};
use alloy_primitives::{U256, map::HashMap};
use evm2::{
    EvmTypesHost, OpcodeConfig,
    constants::STACK_LIMIT,
    interpreter::{op, opcode::OpCode},
};
use std::collections::VecDeque;

/// Where a virtual stack entry lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Loc {
    /// A stack slot, relative to the block's entry top.
    Slot(i32),
    /// A temporary, placed above every slot the block uses once the block is translated.
    Temp(u32),
    /// A constant.
    Imm(u32),
}

/// How the translator handles an opcode.
#[derive(Clone, Copy, Debug)]
enum Class {
    Push(usize),
    Dup(usize),
    Swap(usize),
    Pop,
    JumpDest,
    Pc,
    Jump,
    JumpI,
    Alu2,
    Alu1,
    Mload,
    Mstore,
    Env,
    CallDataLoad,
    Generic { inputs: u8, outputs: u8, ends: bool, terminates: bool },
    Bail,
}

/// A record before its slots are final.
#[derive(Clone, Copy, Debug)]
enum Ir {
    Alu2 { op: u8, neg: u8, a: Loc, b: Loc, d: Loc },
    Alu1 { op: u8, neg: u8, a: Loc, d: Loc },
    Mload { a: Loc, d: Loc },
    Mstore { op: u8, a: Loc, b: Loc },
    Env { op: u8, d: Loc },
    CallDataLoad { a: Loc, d: Loc },
    Jump { target: u32, exit: Exit },
    JumpDyn { dest: Loc, exit: Exit },
    Br { cmp: u8, neg: u8, cont: bool, a: Loc, b: Loc, target: u32, fall: u32, exit: Exit },
    BrDyn { cond: Loc, dest: Loc, fall: u32, exit: Exit },
    Fall { target: u32, exit: Exit },
    Generic { pc: u32, io: u32, next_section: u32, exit: Exit },
    Halt(Halt),
    Bail { pc: u32, exit: Exit },
}

/// Moves to run before an exit or host instruction, and the stack height they leave.
#[derive(Clone, Copy, Debug)]
struct Exit {
    start: u32,
    len: u32,
    height: i32,
}

/// Accumulates one gas and stack section.
#[derive(Clone, Copy, Debug)]
struct SectionAcc {
    index: u32,
    gas: u64,
    inputs: i32,
    diff: i32,
    max_growth: i32,
    base: i32,
}

impl SectionAcc {
    const fn new(index: u32, base: i32) -> Self {
        Self { index, gas: 0, inputs: 0, diff: 0, max_growth: 0, base }
    }

    #[inline]
    fn process(&mut self, inputs: u8, outputs: u8, gas: u16) {
        self.inputs = self.inputs.max(inputs as i32 - self.diff);
        self.diff += outputs as i32 - inputs as i32;
        self.max_growth = self.max_growth.max(self.diff);
        self.gas += gas as u64;
    }

    const fn section(&self) -> Section {
        Section {
            gas: self.gas,
            inputs: self.inputs,
            max_growth: self.max_growth,
            base: self.base,
            need: 0,
        }
    }
}

/// Bias that maps a slot position to an index of the per-slot tables. Translation stops well
/// before positions leave `-SLOT_BIAS..SLOTS - SLOT_BIAS`.
const SLOT_BIAS: i32 = 1100;

/// Size of the per-slot tables.
const SLOTS: usize = 2304;

/// No result index.
const NO_DEF: u32 = u32::MAX;

/// No slot hint.
const NO_HINT: i32 = i32::MIN;

/// Translation state of one block, reused across blocks to keep its allocations.
#[derive(Debug, Default)]
struct BlockState {
    /// Stack height relative to the entry top.
    height: i32,
    /// Lowest position held in `stack`; positions below it are their own entry slots.
    low: i32,
    /// Entries for positions `low..height`.
    stack: VecDeque<Loc>,
    /// Explicit references to each slot, by slot index.
    slot_refs: Vec<u32>,
    /// The result index that last wrote each slot, by slot index.
    slot_defs: Vec<u32>,
    /// Slot indexes whose entry in `slot_refs` or `slot_defs` may be set.
    touched: Vec<usize>,
    /// References to each temporary.
    temp_refs: Vec<u32>,
    /// The result index that last wrote each temporary.
    temp_defs: Vec<u32>,
    free_temps: Vec<u32>,
    max_height: i32,
    ir: Vec<Ir>,
    moves: Vec<(Loc, Loc)>,
    /// The last record and its result, while nothing else has been emitted since.
    last_def: Option<(usize, Loc)>,
    /// Slot each result should be written to, by result index, from a first pass; `NO_HINT` for
    /// none.
    hints: Vec<i32>,
    /// Canonical position each result reaches at the first exit after it, by result index.
    exit_positions: Vec<i32>,
    /// Scratch space of [`Self::exit_moves`].
    pending: Vec<(Loc, Loc)>,
}

const fn slot_index(p: i32) -> usize {
    (p + SLOT_BIAS) as usize
}

impl BlockState {
    /// Prepares the state for a new block, translated with `hints`.
    fn start(&mut self, hints: Vec<i32>) {
        self.height = 0;
        self.low = 0;
        self.stack.clear();
        if self.slot_refs.len() != SLOTS {
            self.slot_refs = vec![0; SLOTS];
            self.slot_defs = vec![NO_DEF; SLOTS];
        }
        self.clear_slots();
        self.temp_refs.clear();
        self.temp_defs.clear();
        self.free_temps.clear();
        self.max_height = 0;
        self.ir.clear();
        self.moves.clear();
        self.last_def = None;
        self.hints = hints;
        self.exit_positions.clear();
    }

    fn clear_slots(&mut self) {
        for &i in &self.touched {
            self.slot_refs[i] = 0;
            self.slot_defs[i] = NO_DEF;
        }
        self.touched.clear();
    }

    #[inline]
    fn refs(&self, loc: Loc) -> u32 {
        match loc {
            Loc::Slot(p) => self.slot_refs[slot_index(p)] + (p < self.low) as u32,
            Loc::Temp(t) => self.temp_refs[t as usize],
            Loc::Imm(_) => 0,
        }
    }

    #[inline]
    fn inc(&mut self, loc: Loc) {
        match loc {
            Loc::Slot(p) => {
                let i = slot_index(p);
                self.slot_refs[i] += 1;
                self.touched.push(i);
            }
            Loc::Temp(t) => self.temp_refs[t as usize] += 1,
            Loc::Imm(_) => {}
        }
    }

    #[inline]
    fn dec(&mut self, loc: Loc) {
        match loc {
            Loc::Slot(p) => self.slot_refs[slot_index(p)] -= 1,
            Loc::Temp(t) => {
                let refs = &mut self.temp_refs[t as usize];
                *refs -= 1;
                if *refs == 0 {
                    self.free_temps.push(t);
                }
            }
            Loc::Imm(_) => {}
        }
    }

    #[inline]
    fn def(&self, loc: Loc) -> u32 {
        match loc {
            Loc::Slot(p) => self.slot_defs[slot_index(p)],
            Loc::Temp(t) => self.temp_defs[t as usize],
            Loc::Imm(_) => NO_DEF,
        }
    }

    #[inline]
    fn set_def(&mut self, loc: Loc, index: u32) {
        match loc {
            Loc::Slot(p) => {
                let i = slot_index(p);
                self.slot_defs[i] = index;
                self.touched.push(i);
            }
            Loc::Temp(t) => self.temp_defs[t as usize] = index,
            Loc::Imm(_) => {}
        }
    }

    #[inline]
    fn alloc_temp(&mut self) -> u32 {
        if let Some(t) = self.free_temps.pop() {
            return t;
        }
        self.temp_refs.push(0);
        self.temp_defs.push(NO_DEF);
        self.temp_refs.len() as u32 - 1
    }

    /// Makes position `p` explicit.
    #[inline]
    fn materialize(&mut self, p: i32) {
        while self.low > p {
            self.low -= 1;
            self.stack.push_front(Loc::Slot(self.low));
            self.inc(Loc::Slot(self.low));
        }
    }

    #[inline]
    fn get(&self, p: i32) -> Loc {
        if p < self.low { Loc::Slot(p) } else { self.stack[(p - self.low) as usize] }
    }

    #[inline]
    fn push(&mut self, loc: Loc) {
        // A temporary popped and pushed back must leave the free list.
        if let Loc::Temp(t) = loc
            && self.temp_refs[t as usize] == 0
            && let Some(i) = self.free_temps.iter().rposition(|&free| free == t)
        {
            self.free_temps.swap_remove(i);
        }
        self.inc(loc);
        self.stack.push_back(loc);
        self.height += 1;
        self.max_height = self.max_height.max(self.height);
    }

    #[inline]
    fn pop(&mut self) -> Loc {
        self.materialize(self.height - 1);
        let loc = self.stack.pop_back().expect("materialized");
        self.height -= 1;
        self.dec(loc);
        loc
    }

    /// Returns where a result pushed now lives.
    ///
    /// That is the slot it reaches at the next exit when the first pass found one, else its own
    /// position's slot, unless a live entry still reads the slot; else a temporary. Writing a slot
    /// no live entry reads is always safe: nothing can read its old value again.
    #[inline]
    fn dest(&mut self) -> Loc {
        let index = self.exit_positions.len() as u32;
        self.exit_positions.push(NO_HINT);
        let hint = self.hints.get(index as usize).copied().unwrap_or(NO_HINT);
        let own = Loc::Slot(self.height);
        let loc = if hint != NO_HINT && self.refs(Loc::Slot(hint)) == 0 {
            Loc::Slot(hint)
        } else if self.refs(own) == 0 {
            own
        } else {
            Loc::Temp(self.alloc_temp())
        };
        self.set_def(loc, index);
        loc
    }

    #[inline]
    fn emit(&mut self, ir: Ir) -> usize {
        self.last_def = None;
        self.ir.push(ir);
        self.ir.len() - 1
    }

    /// Writes the virtual stack back to its canonical slots and returns the moves.
    fn flush(&mut self) -> Exit {
        let exit = self.exit_moves();
        self.reset();
        exit
    }

    /// Returns the moves that write the virtual stack back to its canonical slots, keeping the
    /// virtual stack for code that continues without them.
    ///
    /// Moves form a parallel copy: a destination that is still the source of another move waits
    /// until that move ran, and a cycle saves one value in a temporary first. Constant writes
    /// read no slot, so they go last.
    fn exit_moves(&mut self) -> Exit {
        let mut pending = core::mem::take(&mut self.pending);
        pending.clear();
        for i in 0..self.stack.len() {
            let loc = self.stack[i];
            let p = self.low + i as i32;
            if loc != Loc::Slot(p) {
                pending.push((Loc::Slot(p), loc));
                let index = self.def(loc);
                if index != NO_DEF && self.exit_positions[index as usize] == NO_HINT {
                    self.exit_positions[index as usize] = p;
                }
            }
        }
        let start = self.moves.len();
        let free = self.free_temps.len();
        while !pending.is_empty() {
            let ready =
                (0..pending.len()).find(|&i| pending.iter().all(|&(_, src)| src != pending[i].0));
            if let Some(i) = ready {
                let m = pending.swap_remove(i);
                self.moves.push(m);
            } else {
                let blocked = pending[0].0;
                let t = Loc::Temp(self.alloc_temp());
                self.moves.push((t, blocked));
                for m in &mut pending {
                    if m.1 == blocked {
                        m.1 = t;
                    }
                }
            }
        }
        // The cycle temporaries only live during the moves.
        let used = self.moves[start..]
            .iter()
            .filter_map(|&(dst, _)| match dst {
                Loc::Temp(t) => Some(t),
                _ => None,
            })
            .collect::<Vec<_>>();
        if !used.is_empty() {
            self.free_temps.truncate(free.min(self.free_temps.len()));
            self.free_temps.extend(used);
        }
        self.moves[start..].sort_by_key(|&(_, src)| matches!(src, Loc::Imm(_)));
        self.pending = pending;
        Exit { start: start as u32, len: (self.moves.len() - start) as u32, height: self.height }
    }

    /// Forgets every virtual entry: the canonical stack holds the current height.
    fn reset(&mut self) {
        self.stack.clear();
        self.low = self.height;
        self.clear_slots();
        self.free_temps.clear();
        for t in (0..self.temp_refs.len()).rev() {
            self.temp_refs[t] = 0;
            self.temp_defs[t] = NO_DEF;
            self.free_temps.push(t as u32);
        }
        self.last_def = None;
    }
}

struct Translator<'a, T: EvmTypesHost, F> {
    code: &'a [u8],
    is_jumpdest: F,
    opcodes: &'a OpcodeConfig<T>,
    base: &'a OpcodeConfig<T>,
    prog: Program,
    consts: HashMap<U256, u32>,
    block_ids: HashMap<u32, u32>,
    worklist: VecDeque<(u32, u32)>,
    /// Block state, reused across blocks.
    state: BlockState,
    /// How to translate each opcode, with its stack inputs, outputs and static gas.
    table: [(Class, u8, u8, u16); 256],
}

/// Translates `code` for `opcodes`.
///
/// `is_jumpdest` must answer as the interpreter's jump table does.
pub(crate) fn translate<T: EvmTypesHost>(
    code: &[u8],
    is_jumpdest: impl Fn(usize) -> bool,
    opcodes: &OpcodeConfig<T>,
) -> Program {
    let mut t = Translator {
        code,
        is_jumpdest,
        opcodes,
        base: OpcodeConfig::<T>::base_for_spec(opcodes.origin_spec()),
        prog: Program { jump_map: vec![INVALID_BLOCK; code.len()], ..Program::default() },
        consts: HashMap::default(),
        block_ids: HashMap::default(),
        worklist: VecDeque::new(),
        state: BlockState::default(),
        table: [(Class::Bail, 0, 0, 0); 256],
    };
    for opcode in 0..=255u8 {
        let class = t.classify(opcode);
        let (inputs, outputs) = OpCode::new(opcode).map_or((0, 0), |info| info.input_output());
        t.table[opcode as usize] = (class, inputs, outputs, opcodes.static_gas(opcode));
    }
    t.block_id(0);
    for pc in 0..code.len() {
        if (t.is_jumpdest)(pc) {
            let id = t.block_id(pc as u32);
            t.prog.jump_map[pc] = id;
        }
    }
    while let Some((pc, id)) = t.worklist.pop_front() {
        t.translate_block(pc, id);
    }
    t.prog
}

impl<T: EvmTypesHost, F: Fn(usize) -> bool> Translator<'_, T, F> {
    fn block_id(&mut self, pc: u32) -> u32 {
        if let Some(&id) = self.block_ids.get(&pc) {
            return id;
        }
        let id = self.prog.blocks.len() as u32;
        self.prog.blocks.push(Block { pc, ..Block::default() });
        self.block_ids.insert(pc, id);
        self.worklist.push_back((pc, id));
        id
    }

    fn intern(&mut self, value: U256) -> Loc {
        let next = self.prog.consts.len() as u32;
        let index = *self.consts.entry(value).or_insert(next);
        if index == next {
            self.prog.consts.push(value);
        }
        Loc::Imm(index)
    }

    fn imm(&self, loc: Loc) -> Option<U256> {
        match loc {
            Loc::Imm(i) => Some(self.prog.consts[i as usize]),
            _ => None,
        }
    }

    const fn classify(&self, opcode: u8) -> Class {
        let opcodes = self.opcodes;
        if !opcodes.is_defined(opcode) {
            return Class::Bail;
        }
        let Some(info) = OpCode::new(opcode) else { return Class::Bail };
        let overridden = opcodes.is_overridden(self.base, opcode);
        let class = match opcode {
            op::PUSH0..=op::PUSH32 => Class::Push((opcode - op::PUSH0) as usize),
            op::DUP1..=op::DUP16 => Class::Dup((opcode - op::DUP1 + 1) as usize),
            op::SWAP1..=op::SWAP16 => Class::Swap((opcode - op::SWAP1 + 1) as usize),
            op::POP => Class::Pop,
            op::JUMPDEST => Class::JumpDest,
            op::PC => Class::Pc,
            op::JUMP => Class::Jump,
            op::JUMPI => Class::JumpI,
            _ if overridden => {
                // The host's own instruction may read the remaining gas.
                return generic(info, true);
            }
            _ if is_alu2(opcode) => Class::Alu2,
            op::ISZERO | op::NOT => Class::Alu1,
            op::MLOAD => Class::Mload,
            op::MSTORE | op::MSTORE8 => Class::Mstore,
            op::CALLDATALOAD => Class::CallDataLoad,
            _ if is_env(opcode) => Class::Env,
            _ => return generic(info, ends_section(opcode)),
        };
        // Stack and control instructions run virtually, so a host may not replace them.
        if overridden { Class::Bail } else { class }
    }

    fn translate_block(&mut self, start: u32, id: u32) {
        // The first pass finds where each result is needed at the next exit; the second writes
        // results there directly, which saves moves. Both make the same decisions otherwise.
        let sections = self.prog.sections.len();
        let mut b = core::mem::take(&mut self.state);
        b.start(Vec::new());
        self.translate_pass(&mut b, start);
        self.prog.sections.truncate(sections);
        let hints = core::mem::take(&mut b.exit_positions);
        b.start(hints);
        let entry_section = self.translate_pass(&mut b, start);
        self.finish_block(&b, id, start, entry_section);
        self.state = b;
    }

    fn translate_pass(&mut self, b: &mut BlockState, start: u32) -> u32 {
        let entry_section = self.prog.sections.len() as u32;
        self.prog.sections.push(Section::default());
        let mut section = SectionAcc::new(entry_section, 0);
        let mut pc = start as usize;
        loop {
            if pc >= self.code.len() {
                b.emit(Ir::Halt(Halt::Stop));
                break;
            }
            if pc != start as usize && (self.is_jumpdest)(pc) {
                let target = self.block_id(pc as u32);
                let exit = b.flush();
                b.emit(Ir::Fall { target, exit });
                break;
            }
            let opcode = self.code[pc];
            let (mut class, inputs, outputs, gas) = self.table[opcode as usize];
            // Far past the stack's limits the block fails anyway; the interpreter reports how.
            if b.low < -(STACK_LIMIT as i32) || b.height > STACK_LIMIT as i32 {
                class = Class::Bail;
            }
            // The designated invalid opcode halts; its static gas is zero.
            if opcode == op::INVALID {
                b.emit(Ir::Halt(Halt::InvalidFE));
                break;
            }
            if let Class::Bail = class {
                let exit = b.flush();
                b.emit(Ir::Bail { pc: pc as u32, exit });
                break;
            }
            section.process(inputs, outputs, gas);
            let next_pc = pc + 1;
            match class {
                Class::Push(n) => {
                    let mut bytes = [0u8; 32];
                    let end = (next_pc + n).min(self.code.len());
                    let avail = end.saturating_sub(next_pc);
                    bytes[32 - n..32 - n + avail].copy_from_slice(&self.code[next_pc..end]);
                    let loc = self.intern(U256::from_be_bytes(bytes));
                    b.push(loc);
                    pc = next_pc + n;
                    continue;
                }
                Class::Dup(n) => {
                    let loc = b.get(b.height - n as i32);
                    b.push(loc);
                }
                Class::Swap(n) => {
                    let top = b.height - 1;
                    let other = top - n as i32;
                    b.materialize(other);
                    let (i, j) = ((top - b.low) as usize, (other - b.low) as usize);
                    b.stack.swap(i, j);
                }
                Class::Pop => {
                    b.pop();
                }
                Class::JumpDest => {}
                Class::Pc => {
                    let loc = self.intern(U256::from(pc));
                    b.push(loc);
                }
                Class::Alu2 => self.alu2(b, opcode),
                Class::Alu1 => self.alu1(b, opcode),
                Class::Mload => {
                    let a = b.pop();
                    let d = b.dest();
                    b.emit(Ir::Mload { a, d });
                    b.push(d);
                }
                Class::Mstore => {
                    let a = b.pop();
                    let v = b.pop();
                    b.emit(Ir::Mstore { op: opcode, a, b: v });
                }
                Class::Env => {
                    let d = b.dest();
                    b.emit(Ir::Env { op: opcode, d });
                    b.push(d);
                }
                Class::CallDataLoad => {
                    let a = b.pop();
                    let d = b.dest();
                    b.emit(Ir::CallDataLoad { a, d });
                    b.push(d);
                }
                Class::Jump => {
                    let dest = b.pop();
                    match self.imm(dest) {
                        Some(dest) => match self.static_target(dest) {
                            INVALID_BLOCK => {
                                b.emit(Ir::Halt(Halt::InvalidJump));
                            }
                            target => {
                                let exit = b.flush();
                                b.emit(Ir::Jump { target, exit });
                            }
                        },
                        None => {
                            let exit = b.flush();
                            b.emit(Ir::JumpDyn { dest, exit });
                        }
                    }
                    break;
                }
                Class::JumpI => {
                    let Some(index) = self.jumpi(b, next_pc as u32) else { break };
                    // Only this `JUMPI` enters the code after it, which keeps the virtual stack
                    // and starts a section.
                    self.prog.sections[section.index as usize] = section.section();
                    let next = self.prog.sections.len() as u32;
                    self.prog.sections.push(Section::default());
                    section = SectionAcc::new(next, b.height);
                    if let Ir::Br { fall, .. } = &mut b.ir[index] {
                        *fall = next;
                    }
                }
                Class::Generic { inputs, outputs, ends, terminates } => {
                    let exit = b.flush();
                    let index = b.emit(Ir::Generic {
                        pc: pc as u32,
                        io: ((opcode as u32) << 16) | ((inputs as u32) << 8) | outputs as u32,
                        next_section: NO_SECTION,
                        exit,
                    });
                    b.height += outputs as i32 - inputs as i32;
                    b.max_height = b.max_height.max(b.height);
                    b.reset();
                    if terminates {
                        break;
                    }
                    if ends {
                        self.prog.sections[section.index as usize] = section.section();
                        let next = self.prog.sections.len() as u32;
                        self.prog.sections.push(Section::default());
                        section = SectionAcc::new(next, b.height);
                        if let Ir::Generic { next_section, .. } = &mut b.ir[index] {
                            *next_section = next;
                        }
                    }
                }
                Class::Bail => unreachable!(),
            }
            pc = next_pc;
        }
        self.prog.sections[section.index as usize] = section.section();
        entry_section
    }

    fn static_target(&self, dest: U256) -> u32 {
        match usize::try_from(dest) {
            Ok(pc) if pc < self.code.len() => self.prog.jump_map[pc],
            _ => INVALID_BLOCK,
        }
    }

    fn alu2(&mut self, b: &mut BlockState, opcode: u8) {
        let x = b.pop();
        let y = b.pop();
        if let (Some(x), Some(y)) = (self.imm(x), self.imm(y)) {
            let loc = self.intern(eval2(opcode, 0, &x, &y));
            b.push(loc);
            return;
        }
        let d = b.dest();
        let index = b.emit(Ir::Alu2 { op: opcode, neg: 0, a: x, b: y, d });
        b.push(d);
        b.last_def = Some((index, d));
    }

    fn alu1(&mut self, b: &mut BlockState, opcode: u8) {
        let x = b.pop();
        if let Some(value) = self.imm(x) {
            let loc = self.intern(eval1(opcode, 0, &value));
            b.push(loc);
            return;
        }
        // `ISZERO` of the condition just computed flips it in place.
        if opcode == op::ISZERO
            && let Some((index, def)) = b.last_def
            && def == x
            && b.refs(x) == 0
        {
            match &mut b.ir[index] {
                Ir::Alu2 { op, neg, .. } if is_cond(*op) => {
                    *neg = flip(*op, *neg);
                    b.push(x);
                    b.last_def = Some((index, x));
                    return;
                }
                Ir::Alu1 { op: op::ISZERO, neg, .. } => {
                    *neg ^= 1;
                    b.push(x);
                    b.last_def = Some((index, x));
                    return;
                }
                _ => {}
            }
        }
        let d = b.dest();
        let index = b.emit(Ir::Alu1 { op: opcode, neg: 0, a: x, d });
        b.push(d);
        b.last_def = Some((index, d));
    }

    /// Translates a `JUMPI`. Returns the index of its record if translation continues after it
    /// with the same virtual stack, which it does unless a block starts at `fall_pc`.
    fn jumpi(&mut self, b: &mut BlockState, fall_pc: u32) -> Option<usize> {
        let dest = b.pop();
        let cond = b.pop();
        let Some(dest) = self.imm(dest) else {
            let fall = self.block_id(fall_pc);
            let exit = b.flush();
            b.emit(Ir::BrDyn { cond, dest, fall, exit });
            return None;
        };
        let target = self.static_target(dest);
        let zero = self.intern(U256::ZERO);
        // Fuse the compare that computed the condition, if nothing else reads it.
        let (cmp, neg, x, y) = match b.last_def {
            Some((index, def)) if def == cond && b.refs(cond) == 0 && index + 1 == b.ir.len() => {
                match b.ir[index] {
                    Ir::Alu2 { op, neg, a, b: y, .. } if is_cond(op) => {
                        b.ir.pop();
                        (op, neg, a, y)
                    }
                    Ir::Alu1 { op: op::ISZERO, neg, a, .. } => {
                        b.ir.pop();
                        (CMP_NZ, neg ^ 1, a, zero)
                    }
                    _ => (CMP_NZ, 0, cond, zero),
                }
            }
            _ => (CMP_NZ, 0, cond, zero),
        };
        let cont = (fall_pc as usize) >= self.code.len() || !(self.is_jumpdest)(fall_pc as usize);
        if cont {
            let exit = b.exit_moves();
            let ir = Ir::Br { cmp, neg, cont, a: x, b: y, target, fall: NO_SECTION, exit };
            return Some(b.emit(ir));
        }
        let fall = self.block_id(fall_pc);
        let exit = b.flush();
        b.emit(Ir::Br { cmp, neg, cont, a: x, b: y, target, fall, exit });
        None
    }

    fn finish_block(&mut self, b: &BlockState, id: u32, pc: u32, entry_section: u32) {
        let temps_base = b.max_height.max(0);
        let need = temps_base + b.temp_refs.len() as i32;
        let section = &mut self.prog.sections[entry_section as usize];
        section.need = need;
        let limit = STACK_LIMIT as i64;
        let min_len = section.inputs as i64;
        let max_len = (limit - section.max_growth as i64).min(limit - need as i64);
        let (min_len, span) = if max_len < min_len {
            (u32::MAX, 0)
        } else {
            (min_len as u32, (max_len - min_len) as u32)
        };
        let first = self.prog.insts.len() as u32;
        self.prog.blocks[id as usize] =
            Block { first, pc, gas: section.gas, min_len, span, section: entry_section };

        let enc = |loc: Loc| match loc {
            Loc::Slot(p) => Operand::slot(p).0,
            Loc::Temp(t) => Operand::slot(temps_base + t as i32).0,
            Loc::Imm(i) => Operand::imm(i).0,
        };
        let is_imm = |loc: Loc| matches!(loc, Loc::Imm(_));
        let mut exits = Vec::new();
        for ir in &b.ir {
            let exit = match *ir {
                Ir::Jump { exit, .. }
                | Ir::JumpDyn { exit, .. }
                | Ir::Br { exit, .. }
                | Ir::BrDyn { exit, .. }
                | Ir::Fall { exit, .. }
                | Ir::Generic { exit, .. }
                | Ir::Bail { exit, .. } => exit,
                _ => continue,
            };
            let moves = &b.moves[exit.start as usize..(exit.start + exit.len) as usize];
            if moves.is_empty() {
                exits.push(NO_MOVES);
                continue;
            }
            exits.push(self.prog.moves.len() as u32);
            self.prog.moves.push(Move { dst: moves.len() as i32, src: 0 });
            for &(dst, src) in moves {
                self.prog.moves.push(Move { dst: enc(dst) as i32, src: enc(src) });
            }
        }
        let move_kind = |exit: Exit| exit.len.min(MAX_UNROLLED_MOVES as u32 + 1) as u8;
        let mut exits = exits.into_iter();
        let mut exit = |inst: &mut Inst, exit: Exit| {
            inst.d = exits.next().expect("one moves start per exit");
            inst.e = exit.height as u32;
        };
        for &ir in &b.ir {
            let inst = match ir {
                Ir::Alu2 { op, neg, a, b: y, d } => Inst {
                    a: enc(a),
                    b: enc(y),
                    c: enc(d),
                    ..Inst::new(Kind::Alu2 { op, neg, a_imm: is_imm(a), b_imm: is_imm(y) })
                },
                Ir::Alu1 { op, neg, a, d } => {
                    Inst { a: enc(a), c: enc(d), ..Inst::new(Kind::Alu1 { op, neg }) }
                }
                Ir::Mload { a, d } => {
                    Inst { a: enc(a), c: enc(d), ..Inst::new(Kind::Mload { a_imm: is_imm(a) }) }
                }
                Ir::Mstore { op, a, b: v } => Inst {
                    a: enc(a),
                    b: enc(v),
                    ..Inst::new(Kind::Mstore { op, a_imm: is_imm(a), b_imm: is_imm(v) })
                },
                Ir::Env { op, d } => Inst { c: enc(d), ..Inst::new(Kind::Env { op }) },
                Ir::CallDataLoad { a, d } => Inst {
                    a: enc(a),
                    c: enc(d),
                    ..Inst::new(Kind::CallDataLoad { a_imm: is_imm(a) })
                },
                Ir::Jump { target, exit: e } => {
                    let mut inst =
                        Inst { c: target, ..Inst::new(Kind::Jump { moves: move_kind(e) }) };
                    exit(&mut inst, e);
                    inst
                }
                Ir::JumpDyn { dest, exit: e } => {
                    let mut inst = Inst { a: enc(dest), ..Inst::new(Kind::JumpDyn) };
                    exit(&mut inst, e);
                    inst
                }
                Ir::Br { cmp, neg, cont, a, b: y, target, fall, exit: e } => {
                    let kind = Kind::Br { cmp, neg, cont, a_imm: is_imm(a), b_imm: is_imm(y) };
                    let mut inst =
                        Inst { a: enc(a), b: enc(y), c: target, g: fall, ..Inst::new(kind) };
                    exit(&mut inst, e);
                    inst
                }
                Ir::BrDyn { cond, dest, fall, exit: e } => {
                    let mut inst =
                        Inst { a: enc(cond), b: enc(dest), g: fall, ..Inst::new(Kind::BrDyn) };
                    exit(&mut inst, e);
                    inst
                }
                Ir::Fall { target, exit: e } => {
                    let mut inst =
                        Inst { c: target, ..Inst::new(Kind::Fall { moves: move_kind(e) }) };
                    exit(&mut inst, e);
                    inst
                }
                Ir::Generic { pc, io, next_section, exit: e } => {
                    let mut inst =
                        Inst { a: pc, b: io, c: next_section, ..Inst::new(Kind::Generic) };
                    exit(&mut inst, e);
                    inst
                }
                Ir::Halt(halt) => Inst { a: halt as u32, ..Inst::new(Kind::Halt) },
                Ir::Bail { pc, exit: e } => {
                    let mut inst = Inst { a: pc, ..Inst::new(Kind::Bail) };
                    exit(&mut inst, e);
                    inst
                }
            };
            self.prog.insts.push(inst);
        }
    }
}

const fn generic(info: OpCode, ends: bool) -> Class {
    let (inputs, outputs) = info.input_output();
    if info.immediate_size() != 0 {
        return Class::Bail;
    }
    Class::Generic { inputs, outputs, ends, terminates: info.info().is_terminating() }
}

/// Returns whether the host instruction of `opcode` may read the remaining gas, so the section
/// must end after it. `GAS` reads it directly.
const fn ends_section(opcode: u8) -> bool {
    matches!(
        opcode,
        op::GAS
            | op::CREATE
            | op::CALL
            | op::CALLCODE
            | op::DELEGATECALL
            | op::CREATE2
            | op::STATICCALL
            | op::SSTORE
            | op::BALANCE
            | op::EXTCODESIZE
            | op::EXTCODECOPY
            | op::EXTCODEHASH
            | op::SLOAD
            | op::SELFDESTRUCT
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal deterministic generator for randomized tests.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self, bound: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 % bound as u64) as usize
        }
    }

    /// Runs `moves` on a model of the slots and temporaries, where each slot initially holds its
    /// own position and a constant holds `1000 + index`.
    fn run_moves(b: &BlockState, exit: Exit, slots: &mut HashMap<Loc, i64>) {
        for &(dst, src) in &b.moves[exit.start as usize..(exit.start + exit.len) as usize] {
            let value = match src {
                Loc::Imm(i) => 1000 + i as i64,
                Loc::Slot(p) => *slots.get(&src).unwrap_or(&(p as i64)),
                Loc::Temp(_) => slots[&src],
            };
            slots.insert(dst, value);
        }
    }

    #[test]
    fn temporary_pushed_back_leaves_free_list() {
        let mut b = BlockState::default();
        b.start(Vec::new());
        let t = Loc::Temp(b.alloc_temp());
        b.push(t);
        let popped = b.pop();
        assert_eq!(popped, t);
        b.push(popped);
        assert!(b.free_temps.is_empty());
        assert_ne!(Loc::Temp(b.alloc_temp()), t);
    }

    /// Shuffles a block's stack with `DUP`, `SWAP`, `POP` and constant pushes, then checks that
    /// its exit moves leave every position holding the value the virtual stack says.
    #[test]
    fn exit_moves_write_back_the_virtual_stack() {
        let mut rng = Rng(0x9e3779b97f4a7c15);
        let mut b = BlockState::default();
        for _ in 0..2000 {
            b.start(Vec::new());
            // The block starts with 8 entry items.
            for _ in 0..rng.next(24) {
                match rng.next(4) {
                    0 if b.height > -8 => {
                        let n = 1 + rng.next(((b.height + 8) as usize).min(16));
                        let loc = b.get(b.height - n as i32);
                        b.push(loc);
                    }
                    1 if b.height > -7 => {
                        let n = 1 + rng.next(((b.height + 7) as usize).min(16));
                        let top = b.height - 1;
                        let other = top - n as i32;
                        b.materialize(other);
                        b.stack.swap((top - b.low) as usize, (other - b.low) as usize);
                    }
                    2 if b.height > -8 => {
                        b.pop();
                    }
                    _ => b.push(Loc::Imm(rng.next(4) as u32)),
                }
            }
            let expected = (b.low..b.height)
                .map(|p| match b.get(p) {
                    Loc::Imm(i) => 1000 + i as i64,
                    Loc::Slot(q) => q as i64,
                    Loc::Temp(_) => unreachable!("no results in this test"),
                })
                .collect::<Vec<_>>();
            let low = b.low;
            let exit = b.exit_moves();
            let mut slots = HashMap::default();
            run_moves(&b, exit, &mut slots);
            for (p, want) in (low..b.height).zip(expected) {
                let got = *slots.get(&Loc::Slot(p)).unwrap_or(&(p as i64));
                assert_eq!(got, want, "position {p}");
            }
        }
    }
}
