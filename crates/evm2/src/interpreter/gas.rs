use core::hint::cold_path;

use super::{InstrErr, Result};

#[derive(Clone, Copy)]
pub struct Gas {
    pub(crate) remaining: u64,
}

pub type GasRef<'a> = &'a mut Gas;

impl Gas {
    pub(crate) fn new(remaining: u64) -> Self {
        Self { remaining }
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
}
