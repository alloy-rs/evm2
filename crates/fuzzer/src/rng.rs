use alloy_primitives::U256;
use std::fmt;

pub(crate) struct Gen {
    state: u64,
}

impl fmt::Debug for Gen {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Gen").field("state", &self.state).finish()
    }
}

impl Gen {
    pub(crate) fn new(seed: u64) -> Self {
        Self { state: seed.max(1) }
    }

    const fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 7;
        x ^= x >> 9;
        x ^= x << 8;
        self.state = x;
        x
    }

    pub(crate) const fn range(&mut self, end: usize) -> usize {
        (self.next_u64() % end as u64) as usize
    }

    pub(crate) const fn range_inclusive(&mut self, start: usize, end: usize) -> usize {
        start + self.range(end - start + 1)
    }

    pub(crate) const fn one_in(&mut self, n: usize) -> bool {
        self.range(n) == 0
    }

    pub(crate) fn pick<T: Copy>(&mut self, values: &[T]) -> T {
        values[self.range(values.len())]
    }

    pub(crate) fn bytes(&mut self, len: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(len);
        for _ in 0..len {
            out.push(self.next_u64() as u8);
        }
        out
    }

    pub(crate) fn small_word(&mut self, max: u64) -> U256 {
        U256::from(self.next_u64() % (max + 1))
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
