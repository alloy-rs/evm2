//! Translation of legacy bytecode to superinstructions.

use super::patterns::{PATTERNS, Sx, instruction_len};
use crate::{OpcodeSet, interpreter::op};
use alloc::{boxed::Box, vec::Vec};

/// The opcodes of a configuration, as far as superinstructions are concerned.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Ops {
    /// Opcodes that are enabled and run without hooks, so they may be fused.
    pub(crate) fusable: OpcodeSet,
    /// Opcodes the configuration leaves undefined, so they may be slots.
    pub(crate) free: OpcodeSet,
}

impl Ops {
    /// Returns whether `sx` applies: its slot is undefined, every opcode it runs is fusable, and
    /// PUSH is the only defined instruction with an immediate.
    #[inline]
    pub(crate) const fn is_active(&self, sx: Sx) -> bool {
        if !self.free.contains(sx.slot)
            || !self.free.contains(op::DUPN)
            || !self.free.contains(op::SWAPN)
            || !self.free.contains(op::EXCHANGE)
        {
            return false;
        }
        let mut i = 0;
        while i < sx.len() {
            if !self.fusable.contains(sx.op(i)) {
                return false;
            }
            i += 1;
        }
        true
    }
}

/// Bytes a pattern's opcodes and immediates span at most.
const WINDOW: usize = 24;

/// Bytes past an instruction translation reads: the window, or the opcode after a PUSH32.
pub(super) const LOOKAHEAD: usize = 33;

/// The longest code [`translate`] translates.
pub(crate) const MAX_LEN: usize = 1 << 21;

/// Patterns compared at once: at least the most that share their first two opcodes.
pub(super) const LANES: usize = 8;

/// [`Matcher::hash_b`] flags: the first opcode of an active pattern, and a slot in use.
pub(super) const IS_FIRST: u8 = 1 << 3;
pub(super) const HASH_SLOT: u8 = 1 << 4;

/// Returns the hashes of the first opcode of a pattern.
const fn hashes(op: u8) -> (u8, u8) {
    // Picked so the opcode pairs of [`PATTERNS`] get distinct keys (checked below), with few other
    // pairs of mainnet code sharing them.
    ((op as u32 * 10) as u8 & 0x7F, ((op as u32 * 167) >> 5) as u8 & 7 | IS_FIRST)
}

/// Returns the 11-bit key of an instruction whose opcode has hashes `hash_a` and `hash_b` and whose
/// next opcode is `second`: a 10-bit pair hash, and bit 10 for a slot in use. Bits 0 to 6 pick a
/// byte of [`Matcher::filter`] and bits 7 to 9 a bit in it.
const fn key(hash_a: u8, hash_b: u8, second: u8) -> usize {
    ((second ^ hash_a) & 0x7F) as usize
        | (((second >> 5 ^ hash_b) & 7) as usize) << 7
        | ((hash_b & HASH_SLOT != 0) as usize) << 10
}

/// Info bit of a match that jumps to a constant.
const HAS_TARGET: u32 = 1 << 16;

/// For each slot whose pattern jumps to a constant: the offset of the target's PUSH immediate, and
/// its size << 8.
const TARGETS: [u16; 256] = {
    let mut targets = [0; 256];
    let mut i = 0;
    while i < PATTERNS.len() {
        let sx = PATTERNS[i];
        if let Some(n) = sx.static_jump() {
            targets[sx.slot as usize] = (sx.offsets()[sx.len() - 2] + 1) as u16 | (n as u16) << 8;
        }
        i += 1;
    }
    targets
};

/// Blocks of a matcher: none (0), one per opcode pair of [`PATTERNS`], and one for slots in use.
const BLOCKS: usize = {
    let mut n = 2;
    let mut i = 0;
    while i < PATTERNS.len() {
        let mut j = 0;
        while j < i
            && !(PATTERNS[j].op(0) == PATTERNS[i].op(0) && PATTERNS[j].op(1) == PATTERNS[i].op(1))
        {
            j += 1;
        }
        if j == i {
            n += 1;
        }
        i += 1;
    }
    assert!(n <= 256);
    n
};

/// The block of slots in use: its first lane always matches one byte and writes the filler.
const FILL_BLOCK: usize = BLOCKS - 1;

/// The patterns of one opcode pair, longest first, laid out to be compared with the code at an
/// instruction all at once. Unused lanes never match.
#[derive(Clone, Copy, Debug)]
#[repr(C, align(64))]
pub(super) struct Block {
    pub(super) mask: [[u64; LANES]; 3],
    pub(super) value: [[u64; LANES]; 3],
    /// Per lane, then for no match: the span, slot << 8 and [`HAS_TARGET`].
    info: [u32; LANES + 1],
}

impl Block {
    const NONE: Self = Self { mask: [[0; LANES]; 3], value: [[1; LANES]; 3], info: [0; LANES + 1] };

    /// Returns the info of the first lane that matches `w`, the code at an instruction, or 0.
    ///
    /// # Safety
    ///
    /// The CPU must support `K`'s target features.
    #[inline(always)]
    unsafe fn find<K: Kernel>(&self, w: &[u64; 3]) -> u32 {
        // SAFETY: As the caller promises.
        self.info[(unsafe { K::hits(self, w) } | 1 << LANES).trailing_zeros() as usize]
    }
}

/// Translation steps a target can speed up.
pub(super) trait Kernel {
    /// Lists the candidates of whole blocks below `fast`, as [`candidates`] does, and returns how
    /// many, with the first instruction after the last block.
    ///
    /// # Safety
    ///
    /// As for [`candidates`], and the CPU supports the kernel's target features.
    unsafe fn candidates(
        src: *const u8,
        code_len: usize,
        fast: usize,
        m: &Matcher,
        buf: *mut u32,
    ) -> (usize, usize);

    /// Returns the lanes of `block` that match `w`, one bit each.
    ///
    /// # Safety
    ///
    /// The CPU supports the kernel's target features.
    unsafe fn hits(block: &Block, w: &[u64; 3]) -> u32;
}

/// Translation without target features.
struct Portable;

impl Kernel for Portable {
    #[inline(always)]
    unsafe fn candidates(
        _: *const u8,
        _: usize,
        _: usize,
        _: &Matcher,
        _: *mut u32,
    ) -> (usize, usize) {
        (0, 0)
    }

    #[inline(always)]
    unsafe fn hits(block: &Block, w: &[u64; 3]) -> u32 {
        let mut hits = 0;
        for i in 0..LANES {
            let d = ((w[0] & block.mask[0][i]) ^ block.value[0][i])
                | ((w[1] & block.mask[1][i]) ^ block.value[1][i])
                | ((w[2] & block.mask[2][i]) ^ block.value[2][i]);
            hits |= ((d == 0) as u32) << i;
        }
        hits
    }
}

/// The superinstructions of a configuration, prepared for [`translate`].
#[derive(Clone, Debug)]
pub(crate) struct Matcher {
    ops: Ops,
    /// By opcode: the hashes of the first opcode of an active pattern, or [`HASH_SLOT`] for a slot
    /// in use; zero otherwise.
    pub(super) hash_a: [u8; 256],
    pub(super) hash_b: [u8; 256],
    /// A bit per [`key`] of an active pattern's opcode pair, for the AVX-512 candidate pass.
    pub(super) filter: [u8; 128],
    /// Block by [`key`], 0 for none.
    keys: [u8; 2048],
    blocks: [Block; BLOCKS],
    filler: u8,
    active: bool,
}

impl Matcher {
    /// Prepares the superinstructions that apply with `ops`; see [`Ops::is_active`].
    pub(crate) const fn new(ops: Ops) -> Self {
        let mut m = Self {
            ops,
            hash_a: [0; 256],
            hash_b: [0; 256],
            filter: [0; 128],
            keys: [0; 2048],
            blocks: [Block::NONE; BLOCKS],
            filler: op::INVALID,
            active: false,
        };
        let mut slots = OpcodeSet::EMPTY;
        let mut i = 0;
        while i < PATTERNS.len() {
            if ops.is_active(PATTERNS[i]) {
                slots.insert(PATTERNS[i].slot);
                m.hash_b[PATTERNS[i].slot as usize] = HASH_SLOT;
                m.active = true;
            }
            i += 1;
        }
        // An undefined opcode that is no slot in use halts like any undefined opcode.
        let mut op = 0;
        while op < 256 {
            if ops.free.contains(op as u8) && !slots.contains(op as u8) {
                m.filler = op as u8;
                break;
            }
            op += 1;
        }
        // Slots in use have keys from 1024 on, which name the fill block.
        m.blocks[FILL_BLOCK].value[0][0] = 0;
        m.blocks[FILL_BLOCK].value[1][0] = 0;
        m.blocks[FILL_BLOCK].value[2][0] = 0;
        m.blocks[FILL_BLOCK].info[0] = 1 | (m.filler as u32) << 8;
        let mut k = 1024;
        while k < 2048 {
            m.keys[k] = FILL_BLOCK as u8;
            k += 1;
        }
        // Patterns longest first, so each block's lanes are longest first.
        let mut lanes = [0; BLOCKS];
        let (mut blocks, mut len) = (1, 8);
        while len >= 2 {
            let mut i = 0;
            while i < PATTERNS.len() {
                let sx = PATTERNS[i];
                i += 1;
                if sx.len() != len || !ops.is_active(sx) {
                    continue;
                }
                let first = sx.op(0) as usize;
                let (a, b) = hashes(sx.op(0));
                (m.hash_a[first], m.hash_b[first]) = (a, b);
                let k = key(a, b, sx.op(1));
                if m.keys[k] == 0 {
                    m.keys[k] = blocks as u8;
                    m.filter[k & 0x7F] |= 1 << (k >> 7);
                    blocks += 1;
                }
                let block = &mut m.blocks[m.keys[k] as usize];
                let lane = lanes[m.keys[k] as usize];
                assert!(lane < LANES, "more patterns share two opcodes than lanes");
                lanes[m.keys[k] as usize] += 1;
                block.value[0][lane] = 0;
                block.value[1][lane] = 0;
                block.value[2][lane] = 0;
                let offsets = sx.offsets();
                let mut j = 0;
                while j < sx.len() {
                    let at = offsets[j];
                    block.mask[at / 8][lane] |= 0xFF << (8 * (at % 8));
                    block.value[at / 8][lane] |= (sx.op(j) as u64) << (8 * (at % 8));
                    j += 1;
                }
                let span = offsets[sx.len()];
                assert!(span <= WINDOW, "pattern longer than the window");
                let jump = if sx.static_jump().is_some() { HAS_TARGET } else { 0 };
                block.info[lane] = span as u32 | (sx.slot as u32) << 8 | jump;
            }
            len -= 1;
        }
        m
    }

    /// Returns the opcodes it was prepared for.
    #[inline]
    pub(crate) const fn ops(&self) -> &Ops {
        &self.ops
    }

    /// Returns whether any superinstruction applies.
    #[inline]
    pub(crate) const fn is_active(&self) -> bool {
        self.active
    }
}

/// Translates legacy bytecode to the superinstructions of `matcher`.
///
/// `code` is padded bytecode, as evm2 pads legacy code: `len` is its length before padding, at
/// most [`MAX_LEN`], and it ends with zero bytes, at least 33 or all past `len`. `jumpdests` marks
/// its jump destinations, one bit per byte of `code[..len]`, least significant bit first.
///
/// Returns a copy of `code` in which the first byte of each sequence that matches an active pattern
/// holds the pattern's slot, and every other instruction whose opcode is a slot in use holds an
/// undefined opcode that is no slot in use. Matching is greedy and longest first; a match that
/// jumps to a constant that is no jump destination keeps its original bytes.
///
/// It runs in two passes without data-dependent branches. The first lists the instructions whose
/// first two opcodes may start a pattern, 64 bytes at a time with AVX-512 where the CPU has it; the
/// second compares each
/// with its patterns at once and takes a match that starts at or after the end of the last one.
pub(crate) fn translate(code: &[u8], len: usize, matcher: &Matcher, jumpdests: &[u8]) -> Box<[u8]> {
    #[cfg(target_arch = "x86_64")]
    if super::arch::x86::available() {
        // SAFETY: The CPU has the kernel's target features.
        return translate_using(code, len, matcher, jumpdests, |code, fast, matcher, out| unsafe {
            super::arch::x86::translate(code, fast, matcher, out)
        });
    }
    // SAFETY: The portable kernel needs no target features.
    translate_using(code, len, matcher, jumpdests, |code, fast, matcher, out| unsafe {
        translate_with::<Portable>(code, fast, matcher, out)
    })
}

/// [`translate`] with `pass`, which [`translate_with`] a kernel, for the instructions below `fast`.
fn translate_using(
    code: &[u8],
    len: usize,
    matcher: &Matcher,
    jumpdests: &[u8],
    pass: impl FnOnce(&[u8], usize, &Matcher, &mut [u8]) -> Vec<u32>,
) -> Box<[u8]> {
    // Instructions from `fast` on are zero bytes, which start and match no pattern.
    let fast = len.min(code.len().saturating_sub(LOOKAHEAD));
    assert!(len <= code.len() && len <= MAX_LEN && jumpdests.len() >= len.div_ceil(8));
    assert!(code[fast..].iter().all(|&b| b == 0), "code is not padded");
    let mut out: Box<[u8]> = code.into();
    for at in pass(code, fast, matcher, &mut out) {
        let at = at as usize;
        let target = TARGETS[out[at] as usize] as usize;
        let imm = at + (target & 0xFF);
        if !valid_target(&code[imm..imm + (target >> 8)], len, jumpdests) {
            out[at] = code[at];
        }
    }
    out
}

/// Writes the matches below `fast` into `out` and returns the positions of those that jump to a
/// constant.
///
/// # Safety
///
/// As [`translate_using`] checks, and the CPU supports `K`'s target features.
#[inline(always)]
pub(super) unsafe fn translate_with<K: Kernel>(
    code: &[u8],
    fast: usize,
    matcher: &Matcher,
    out: &mut [u8],
) -> Vec<u32> {
    use core::hint::select_unpredictable as select;
    // Candidates as pc << 11 | key, then the positions of matches that jump to a constant.
    let mut list = Vec::<u32>::with_capacity(fast + 64);
    let (src, dst, buf) = (code.as_ptr(), out.as_mut_ptr(), list.as_mut_ptr());
    // SAFETY: Reads up to [`LOOKAHEAD`] bytes past an instruction below `fast` stay in `code`,
    // writes at an instruction below `fast` stay in `out`, and each pass writes at most one entry
    // of `list` per instruction, plus 64 for the last block.
    unsafe {
        let n = candidates::<K>(src, code.len(), fast, matcher, buf);
        // The end of the last match taken.
        let (mut free, mut jumps) = (0, 0);
        for i in 0..n {
            let c = *buf.add(i) as usize;
            let (at, block) = (c >> 11, *matcher.keys.get_unchecked(c & 0x7FF) as usize);
            let p = src.add(at).cast::<u64>();
            let w = [
                u64::from_le(p.read_unaligned()),
                u64::from_le(p.add(1).read_unaligned()),
                u64::from_le(p.add(2).read_unaligned()),
            ];
            let info = matcher.blocks.get_unchecked(block).find::<K>(&w);
            let taken = (at >= free) & (info != 0);
            free = select(taken, at + (info & 0xFF) as usize, free);
            *dst.add(at) = select(taken, (info >> 8) as u8, *src.add(at));
            // `jumps <= i`, so this overwrites only candidates already read.
            *buf.add(jumps) = at as u32;
            jumps += (taken & (info & HAS_TARGET != 0)) as usize;
        }
        list.set_len(jumps);
    }
    list
}

/// Lists the instructions below `fast` whose first two opcodes may start a pattern, or that are a
/// slot in use, in `buf` as `pc << 11 | key`, and returns how many.
///
/// # Safety
///
/// `src` holds `code_len` bytes, at least [`LOOKAHEAD`] past `fast`, `fast <= MAX_LEN`, and `buf`
/// has room for an entry per byte below `fast`, plus 64.
#[inline(always)]
unsafe fn candidates<K: Kernel>(
    src: *const u8,
    code_len: usize,
    fast: usize,
    m: &Matcher,
    buf: *mut u32,
) -> usize {
    // SAFETY: As the caller promises.
    let (mut n, mut pc) = unsafe { K::candidates(src, code_len, fast, m, buf) };
    while pc < fast {
        // SAFETY: As the caller promises.
        unsafe {
            let op = *src.add(pc);
            let next = pc + instruction_len(op);
            let (a, b) =
                (*m.hash_a.get_unchecked(op as usize), *m.hash_b.get_unchecked(op as usize));
            let k = key(a, b, *src.add(next));
            *buf.add(n) = (pc << 11 | k) as u32;
            n += ((*m.keys.get_unchecked(k) != 0) & (b & (IS_FIRST | HASH_SLOT) != 0)) as usize;
            pc = next;
        }
    }
    n
}

/// Returns whether the big-endian immediate `bytes` is a jump destination in `marks`.
fn valid_target(bytes: &[u8], len: usize, marks: &[u8]) -> bool {
    let n = bytes.len();
    if n > 8 && bytes[..n - 8].iter().any(|&b| b != 0) {
        return false;
    }
    let target = bytes[n.saturating_sub(8)..].iter().fold(0u64, |acc, &b| acc << 8 | b as u64);
    target < len as u64 && marks[target as usize / 8] >> (target % 8) & 1 != 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interpreter::sx::patterns::tests::sx;

    /// Prague's opcodes, none hooked: every pattern applies.
    fn prague() -> Ops {
        let defined = |op: u8| matches!(op, 0x00..=0x0B | 0x10..=0x1D | 0x20 | 0x30..=0x4A | 0x50..=0x9F | 0xA0..=0xA4 | 0xF0..=0xF5 | 0xFA | 0xFD..=0xFF);
        Ops {
            fusable: (0..=255).filter(|&op| defined(op)).collect(),
            free: (0..=255).filter(|&op| !defined(op)).collect(),
        }
    }

    fn without(set: OpcodeSet, ops: &[u8]) -> OpcodeSet {
        (0..=255).filter(|op| set.contains(*op) && !ops.contains(op)).collect()
    }

    fn jumpdests(code: &[u8]) -> Vec<u8> {
        let mut table = alloc::vec![0; code.len().div_ceil(8)];
        let mut pc = 0;
        while pc < code.len() {
            if code[pc] == op::JUMPDEST {
                table[pc / 8] |= 1 << (pc % 8);
            }
            pc += instruction_len(code[pc]);
        }
        table
    }

    /// Translates `code[..len]` with the kernel the CPU has and the portable one, and checks that
    /// they agree.
    fn check(code: &[u8], len: usize, ops: Ops) -> Box<[u8]> {
        let (matcher, marks) = (Matcher::new(ops), jumpdests(&code[..len]));
        let out = translate(code, len, &matcher, &marks);
        let portable = translate_using(code, len, &matcher, &marks, |code, fast, matcher, out| {
            // SAFETY: The portable kernel needs no target features.
            unsafe { translate_with::<Portable>(code, fast, matcher, out) }
        });
        assert_eq!(out, portable, "kernels disagree");
        out
    }

    /// Pads `code` as evm2 pads legacy code, and translates it.
    fn translated(code: &[u8], ops: Ops) -> (Vec<u8>, Box<[u8]>) {
        let mut padded = code.to_vec();
        padded.extend_from_slice(&[0; LOOKAHEAD]);
        let out = check(&padded, code.len(), ops);
        (padded, out)
    }

    /// Returns the end of `sx`'s match at instruction `pc`, if it matches there.
    fn match_at(code: &[u8], len: usize, mut pc: usize, sx: Sx) -> Option<usize> {
        for i in 0..sx.len() {
            if pc >= len || code[pc] != sx.op(i) {
                return None;
            }
            pc += instruction_len(code[pc]);
        }
        Some(pc)
    }

    /// [`translate`] one instruction and pattern at a time.
    fn reference(code: &[u8], len: usize, ops: Ops) -> Vec<u8> {
        let filler = Matcher::new(ops).filler;
        let marks = jumpdests(&code[..len]);
        let mut out = code.to_vec();
        let mut pc = 0;
        while pc < len {
            let active = PATTERNS.iter().copied().filter(|sx| ops.is_active(*sx));
            if active.clone().any(|sx| sx.slot == code[pc]) {
                out[pc] = filler;
                pc += 1;
            } else if let Some((sx, end)) = active
                .filter_map(|sx| Some((sx, match_at(code, len, pc, sx)?)))
                .max_by_key(|(sx, _)| sx.len())
            {
                let valid = sx.static_jump().is_none_or(|n| {
                    let at = pc + sx.offsets()[sx.len() - 2] + 1;
                    valid_target(&code[at..at + n], len, &marks)
                });
                if valid {
                    out[pc] = sx.slot;
                }
                pc = end;
            } else {
                pc += instruction_len(code[pc]);
            }
        }
        out
    }

    #[test]
    fn pair_keys_are_distinct() {
        for (i, p) in PATTERNS.iter().enumerate() {
            for q in &PATTERNS[..i] {
                if (p.op(0), p.op(1)) != (q.op(0), q.op(1)) {
                    let (pa, pb) = hashes(p.op(0));
                    let (qa, qb) = hashes(q.op(0));
                    assert_ne!(key(pa, pb, p.op(1)), key(qa, qb, q.op(1)), "pattern {i}");
                }
            }
        }
    }

    #[test]
    fn fuses_longest_and_keeps_layout() {
        use op::*;
        // DUP1 PUSH4 sel EQ PUSH2 dst JUMPI, then PUSH2 dst JUMPI, to the JUMPDEST at 15.
        let mut code =
            [DUP1, PUSH4, 1, 2, 3, 4, EQ, PUSH2, 0, 15, JUMPI, PUSH2, 0, 15, JUMPI, JUMPDEST];
        let (padded, out) = translated(&code, prague());
        assert_eq!(out[0], sx(&[DUP1, PUSH4, EQ, PUSH2, JUMPI]).slot);
        assert_eq!(out[11], sx(&[PUSH2, JUMPI]).slot);
        assert_eq!(&out[1..11], &padded[1..11]);
        assert_eq!(&out[12..], &padded[12..]);
        // A constant target that is no JUMPDEST leaves its sequence unfused.
        code[13] = 14;
        let (padded, out) = translated(&code, prague());
        assert_eq!(out[0], sx(&[DUP1, PUSH4, EQ, PUSH2, JUMPI]).slot);
        assert_eq!(&out[1..], &padded[1..]);
    }

    #[test]
    fn slot_bytes_in_code_become_undefined() {
        use op::*;
        let slot = sx(&[PUSH2, JUMPI]).slot;
        // A slot as an instruction, and as PUSH data, which is no instruction.
        let code = [slot, PUSH1, slot, PUSH2, 0, 7, JUMPI, JUMPDEST];
        let (_, out) = translated(&code, prague());
        // 0x0C, undefined like the slot without superinstructions, halts the same way.
        assert_eq!(&out[..4], &[0x0C, PUSH1, slot, slot]);
        // A slot of a pattern that does not apply is left alone.
        let ops = Ops { fusable: without(prague().fusable, &[JUMPI]), ..prague() };
        assert_eq!(translated(&code, ops).1[0], slot);
    }

    #[test]
    fn nothing_fuses_when_other_instructions_take_immediates() {
        use op::*;
        // EIP-8024's DUPN, SWAPN and EXCHANGE, defined even if hooked, so not fusable.
        let ops = Ops { free: without(prague().free, &[DUPN, SWAPN, EXCHANGE]), ..prague() };
        assert!(PATTERNS.iter().all(|sx| !ops.is_active(*sx)));
        let (padded, out) = translated(&[PUSH2, 0, 4, JUMPI, JUMPDEST], ops);
        assert_eq!(&out[..], &padded[..]);
    }

    #[test]
    fn matches_reference() {
        let mut s = 0x2545F4914F6CDD1Du64;
        let mut next = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        let alphabet: Vec<u8> =
            PATTERNS.iter().flat_map(|p| (0..p.len()).map(move |i| p.op(i))).collect();
        let mut random = |n: usize| -> Vec<u8> {
            (0..n)
                .map(|_| {
                    let r = next();
                    if r % 5 == 0 {
                        (r >> 24) as u8
                    } else {
                        alphabet[(r >> 8) as usize % alphabet.len()]
                    }
                })
                .collect()
        };
        // Short and long code, and immediates that cross every 64-byte block.
        let mut codes: Vec<Vec<u8>> = (0..4000).map(|i| random(i % 400)).collect();
        codes.extend((0..100).map(|i| random(1000 + 97 * i)));
        codes.push(alloc::vec![op::PUSH32; 5000]);
        codes.push([op::PUSH2, op::JUMPDEST, op::PUSH2].repeat(2000));
        let partial = {
            // Before Constantinople and Shanghai: no SHL, SHR or PUSH0.
            let removed = [op::SHL, op::SHR, op::PUSH0];
            let mut free = prague().free;
            for op in removed {
                free.insert(op);
            }
            Ops { fusable: without(prague().fusable, &removed), free }
        };
        for (i, code) in codes.iter().enumerate() {
            let ops = if i % 2 == 0 { prague() } else { partial };
            let mut padded = code.clone();
            padded.extend_from_slice(&[0; LOOKAHEAD]);
            // Padded, and code that ends with 33 zeros, which padding leaves as it is.
            for len in [code.len(), padded.len()] {
                let got = check(&padded, len, ops);
                assert_eq!(&got[..], &reference(&padded, len, ops)[..], "code {i}, length {len}");
            }
        }
    }
}
