use alloy_primitives::U256;
use rand::{RngExt, SeedableRng, rngs::StdRng};
use std::fmt;

pub(crate) struct Gen {
    rng: StdRng,
}

impl fmt::Debug for Gen {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Gen").finish_non_exhaustive()
    }
}

impl Gen {
    pub(crate) fn new(seed: u64) -> Self {
        Self { rng: StdRng::seed_from_u64(seed) }
    }

    fn next_u64(&mut self) -> u64 {
        self.rng.random()
    }

    pub(crate) fn range(&mut self, end: usize) -> usize {
        self.rng.random_range(..end)
    }

    pub(crate) fn range_inclusive(&mut self, start: usize, end: usize) -> usize {
        self.rng.random_range(start..=end)
    }

    pub(crate) fn one_in(&mut self, n: usize) -> bool {
        self.rng.random_range(..n) == 0
    }

    pub(crate) fn pick<T: Copy>(&mut self, values: &[T]) -> T {
        values[self.range(values.len())]
    }

    pub(crate) fn bytes(&mut self, len: usize) -> Vec<u8> {
        let mut out = vec![0; len];
        self.rng.fill(&mut out[..]);
        out
    }

    pub(crate) fn small_word(&mut self, max: u64) -> U256 {
        U256::from(self.rng.random_range(..=max))
    }

    pub(crate) fn biased_invalid_jumpdest(&mut self) -> U256 {
        match self.range(4) {
            0 => U256::ZERO,
            1 => U256::ONE,
            2 => U256::from(self.next_u64() & 0xffff),
            _ => U256::MAX,
        }
    }

    pub(crate) fn biased_word(&mut self) -> U256 {
        match self.range(16) {
            0 => U256::ZERO,
            1 => U256::ONE,
            2 => U256::from(2),
            3 => U256::from(31),
            4 => U256::from(32),
            5 => U256::from(33),
            6 => U256::from(255),
            7 => U256::from(256),
            8 => U256::from(u64::MAX),
            9 => U256::MAX,
            10..=13 => U256::from(self.next_u64() & 0xffff),
            _ => U256::from_limbs([
                self.next_u64(),
                self.next_u64(),
                self.next_u64(),
                self.next_u64(),
            ]),
        }
    }
}
