//! Translation with AVX-512 VBMI2, detected at run time.

use super::super::translate::{Block, HASH_SLOT, IS_FIRST, Kernel, Matcher, translate_with};
use crate::utils::detected;
use alloc::vec::Vec;
use core::arch::x86_64::*;

/// Returns whether the CPU has the target features of [`translate`].
#[inline]
pub(in crate::interpreter::sx) fn available() -> bool {
    detected!("avx512f", "avx512bw", "avx512vbmi", "avx512vbmi2")
}

/// [`translate_with`] with AVX-512.
///
/// # Safety
///
/// As for [`translate_with`], and [`available`] returns true.
#[target_feature(enable = "avx512f,avx512bw,avx512vbmi,avx512vbmi2")]
pub(in crate::interpreter::sx) unsafe fn translate(
    code: &[u8],
    fast: usize,
    matcher: &Matcher,
    out: &mut [u8],
) -> Vec<u32> {
    // SAFETY: As the caller promises.
    unsafe { translate_with::<Avx512>(code, fast, matcher, out) }
}

/// Candidates of whole 64-byte blocks: instruction starts from successor maps, as in jump
/// destination analysis, and a vector filter on each start's first two opcodes; and all of a
/// block's patterns compared at once.
struct Avx512;

impl Kernel for Avx512 {
    #[inline]
    #[target_feature(enable = "avx512f,avx512bw,avx512vbmi,avx512vbmi2")]
    unsafe fn candidates(
        src: *const u8,
        code_len: usize,
        fast: usize,
        m: &Matcher,
        buf: *mut u32,
    ) -> (usize, usize) {
        // SAFETY: As the caller promises.
        unsafe { candidates_avx512(src, code_len, fast, m, buf) }
    }

    #[inline]
    #[target_feature(enable = "avx512f,avx512bw,avx512vbmi,avx512vbmi2")]
    unsafe fn hits(block: &Block, w: &[u64; 3]) -> u32 {
        // SAFETY: The target has AVX-512F, and `Block` is 64-byte aligned with each row 64 bytes.
        unsafe {
            let (m, v) =
                (block.mask.as_ptr().cast::<__m512i>(), block.value.as_ptr().cast::<__m512i>());
            // (w & mask) ^ value per qword, then the OR of the three.
            let d = |i: usize| {
                _mm512_ternarylogic_epi64::<0x6A>(
                    _mm512_set1_epi64(w[i] as i64),
                    _mm512_load_si512(m.add(i)),
                    _mm512_load_si512(v.add(i)),
                )
            };
            let d = _mm512_ternarylogic_epi64::<0xFE>(d(0), d(1), d(2));
            _mm512_testn_epi64_mask(d, d) as u32
        }
    }
}

/// Every opcode up to this, and `0x80..` as signed bytes, is one byte long.
const PUSHX: i8 = 0x5F;

const fn lanes(start: u8) -> [u8; 64] {
    let mut lanes = [0; 64];
    let mut i = 0;
    while i < 64 {
        lanes[i] = start.wrapping_add(i as u8);
        i += 1;
    }
    lanes
}

// SAFETY: Both are 64 bytes.
const LANES: __m512i = unsafe { core::mem::transmute(lanes(0)) };
/// Added to `max(opcode, PUSHX)` to get the lane of the next instruction.
const END_OFFSETS: __m512i = unsafe { core::mem::transmute(lanes(0u8.wrapping_sub(0x5E))) };
/// Second table of `vpermt2b`, which keeps offsets past the block.
const EXITS: __m512i = unsafe { core::mem::transmute(lanes(64)) };

/// Returns the successor maps of `ops` for 1, 2, 4, ..., 64 instructions.
#[inline]
#[target_feature(enable = "avx512f,avx512bw,avx512vbmi,avx512vbmi2")]
fn successors(ops: __m512i) -> [__m512i; 7] {
    let mut n = [_mm512_add_epi8(_mm512_max_epi8(ops, _mm512_set1_epi8(PUSHX)), END_OFFSETS); 7];
    for i in 1..7 {
        n[i] = _mm512_permutex2var_epi8(n[i - 1], n[i - 1], EXITS);
    }
    n
}

/// Moves each lane's position along the successor map if the result does not pass the lane.
#[inline]
#[target_feature(enable = "avx512f,avx512bw,avx512vbmi,avx512vbmi2")]
fn descend(successors: __m512i, positions: __m512i) -> __m512i {
    let next = _mm512_permutexvar_epi8(positions, successors);
    _mm512_mask_mov_epi8(positions, _mm512_cmple_epu8_mask(next, LANES), next)
}

/// Returns the lanes of the block with successor maps `n` that start an instruction.
#[inline]
#[target_feature(enable = "avx512f,avx512bw,avx512vbmi,avx512vbmi2")]
fn starts(n: &[__m512i; 7], entry: __m512i) -> u64 {
    let mut position = entry;
    for i in (1..6).rev() {
        position = descend(n[i], position);
    }
    _mm512_cmpeq_epi8_mask(position, LANES)
        | _mm512_cmpeq_epi8_mask(_mm512_permutexvar_epi8(position, n[0]), LANES)
}

/// Looks up each byte of `ops` in a 256-byte table.
#[inline]
#[target_feature(enable = "avx512f,avx512bw,avx512vbmi,avx512vbmi2")]
fn lookup(table: &[__m512i; 4], ops: __m512i, high: __mmask64) -> __m512i {
    _mm512_mask_blend_epi8(
        high,
        _mm512_permutex2var_epi8(table[0], ops, table[1]),
        _mm512_permutex2var_epi8(table[2], ops, table[3]),
    )
}

/// Lists the candidates of whole 64-byte blocks below `fast` and returns how many, with the
/// first instruction after the last block.
///
/// # Safety
///
/// As for [`Kernel::candidates`].
#[inline]
#[target_feature(enable = "avx512f,avx512bw,avx512vbmi,avx512vbmi2")]
unsafe fn candidates_avx512(
    src: *const u8,
    code_len: usize,
    fast: usize,
    m: &Matcher,
    buf: *mut u32,
) -> (usize, usize) {
    // SAFETY: Tables are 64-byte multiples; loads stay below `code_len` (the next block's
    // load is masked to it); a block writes 64 entries at `buf + n`, with `n` at most the
    // instructions before it.
    unsafe {
        let load4 = |t: &[u8; 256]| -> [__m512i; 4] {
            core::array::from_fn(|i| _mm512_loadu_si512(t.as_ptr().add(64 * i).cast()))
        };
        let (hash_a, hash_b) = (load4(&m.hash_a), load4(&m.hash_b));
        let filter = [
            _mm512_loadu_si512(m.filter.as_ptr().cast()),
            _mm512_loadu_si512(m.filter.as_ptr().add(64).cast()),
        ];
        // 1 << k at byte k of each 8.
        let pow2 = _mm512_set1_epi64(0x8040_2010_0804_0201_u64 as i64);
        let mut entry = _mm512_setzero_si512();
        let (mut n, mut base) = (0, 0);
        while base + 64 <= fast {
            let ops = _mm512_loadu_si512(src.add(base).cast());
            let rest = (code_len - base - 64).min(64);
            let next = _mm512_maskz_loadu_epi8(u64::MAX >> (64 - rest), src.add(base + 64).cast());
            let succ = successors(ops);
            let starts = starts(&succ, entry);
            entry = _mm512_sub_epi8(_mm512_permutexvar_epi8(entry, succ[6]), _mm512_set1_epi8(64));
            // The opcode of each lane's next instruction.
            let second = _mm512_permutex2var_epi8(ops, succ[0], next);
            let high = _mm512_movepi8_mask(ops);
            let a = lookup(&hash_a, ops, high);
            let b = lookup(&hash_b, ops, high);
            let byte = _mm512_permutex2var_epi8(filter[0], _mm512_xor_si512(second, a), filter[1]);
            let bit = _mm512_and_si512(
                _mm512_xor_si512(_mm512_srli_epi16(second, 5), b),
                _mm512_set1_epi8(7),
            );
            let hit = _mm512_test_epi8_mask(byte, _mm512_shuffle_epi8(pow2, bit));
            let first = _mm512_test_epi8_mask(b, _mm512_set1_epi8(IS_FIRST as i8));
            let slot = _mm512_test_epi8_mask(b, _mm512_set1_epi8(HASH_SLOT as i8));
            let cand = starts & (first & hit | slot);
            // Each candidate as pc << 11 | key, from its lane, the key's bits 7 to 10 and its
            // bits 0 to 6.
            let lanes = _mm512_maskz_compress_epi8(cand, LANES);
            let high = _mm512_maskz_compress_epi8(
                cand,
                _mm512_or_si512(
                    bit,
                    _mm512_and_si512(_mm512_srli_epi16(b, 1), _mm512_set1_epi8(8)),
                ),
            );
            let low = _mm512_maskz_compress_epi8(
                cand,
                _mm512_and_si512(_mm512_xor_si512(second, a), _mm512_set1_epi8(0x7F)),
            );
            let at = _mm512_set1_epi32((base << 11) as i32);
            macro_rules! store {
                ($($q:literal)*) => {$(
                    let p = _mm512_cvtepu8_epi32(_mm512_extracti32x4_epi32::<$q>(lanes));
                    let h = _mm512_cvtepu8_epi32(_mm512_extracti32x4_epi32::<$q>(high));
                    let l = _mm512_cvtepu8_epi32(_mm512_extracti32x4_epi32::<$q>(low));
                    let c = _mm512_ternarylogic_epi32::<0xFE>(_mm512_slli_epi32(p, 11), _mm512_slli_epi32(h, 7), l);
                    _mm512_storeu_si512(buf.add(n + 16 * $q).cast(), _mm512_add_epi32(c, at));
                )*};
            }
            store!(0 1 2 3);
            n += cand.count_ones() as usize;
            base += 64;
        }
        (n, base + _mm512_cvtsi512_si32(entry) as u8 as usize)
    }
}
