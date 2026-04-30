use super::{InstrErr, Result};
use core::hint::cold_path;

#[derive(Clone, Copy)]
pub struct Gas {
    pub(crate) remaining: u64,
    memory: MemoryGas,
}

pub type GasRef<'a> = &'a mut Gas;

impl Gas {
    pub(crate) fn new(remaining: u64) -> Self {
        Self { remaining, memory: MemoryGas::new() }
    }

    #[inline(always)]
    pub fn spend(&mut self, amount: u64) -> Result {
        let overflow;
        (self.remaining, overflow) = self.remaining.overflowing_sub(amount);
        if overflow {
            cold_path();
            Err(InstrErr::OutOfGas)
        } else {
            Ok(())
        }
    }

    #[inline]
    pub const fn memory(&self) -> &MemoryGas {
        &self.memory
    }

    #[inline]
    pub const fn memory_mut(&mut self) -> &mut MemoryGas {
        &mut self.memory
    }

    #[inline]
    pub const fn record_regular_cost(&mut self, cost: u64) -> bool {
        let remaining = self.remaining;
        let enough = remaining >= cost;
        self.remaining = remaining.wrapping_sub(cost);
        enough
    }
}

#[derive(Clone, Copy, Default)]
pub struct MemoryGas {
    pub words_num: usize,
    pub expansion_cost: u64,
}

impl MemoryGas {
    #[inline]
    pub const fn new() -> Self {
        Self { words_num: 0, expansion_cost: 0 }
    }

    #[inline]
    pub const fn set_words_num(
        &mut self,
        words_num: usize,
        mut expansion_cost: u64,
    ) -> Option<u64> {
        self.words_num = words_num;
        core::mem::swap(&mut self.expansion_cost, &mut expansion_cost);
        self.expansion_cost.checked_sub(expansion_cost)
    }
}
