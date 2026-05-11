use alloc::{vec, vec::Vec};
use alloy_primitives::{Address, U256, map::DefaultHashBuilder};
use core::{
    fmt,
    hash::{BuildHasher, Hash, Hasher},
};
use evm2::{
    EvmTypes, Inspector,
    bytecode::opcode::op,
    interpreter::{Interpreter, Word},
};

const MAX_EDGE_COUNT: usize = 65536;

/// An `Inspector` that tracks [edge coverage](https://clang.llvm.org/docs/SanitizerCoverage.html#edge-coverage).
#[derive(Clone)]
pub struct EdgeCovInspector {
    /// Map of hitcounts that can be diffed against to determine if new coverage was reached.
    hitcount: Vec<u8>,
    hash_builder: DefaultHashBuilder,
}

impl fmt::Debug for EdgeCovInspector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EdgeCovInspector").finish_non_exhaustive()
    }
}

impl EdgeCovInspector {
    /// Create a new `EdgeCovInspector` with `MAX_EDGE_COUNT` size.
    pub fn new() -> Self {
        Self { hitcount: vec![0; MAX_EDGE_COUNT], hash_builder: DefaultHashBuilder::default() }
    }

    /// Reset the hitcount to zero.
    pub fn reset(&mut self) {
        self.hitcount.fill(0);
    }

    /// Get an immutable reference to the hitcount.
    pub const fn get_hitcount(&self) -> &[u8] {
        self.hitcount.as_slice()
    }

    /// Consume the inspector and take ownership of the hitcount.
    pub fn into_hitcount(self) -> Vec<u8> {
        self.hitcount
    }

    fn store_hit(&mut self, address: Address, pc: usize, jump_dest: U256) {
        let mut hasher = self.hash_builder.build_hasher();
        address.hash(&mut hasher);
        pc.hash(&mut hasher);
        jump_dest.hash(&mut hasher);
        let edge_id = (hasher.finish() % MAX_EDGE_COUNT as u64) as usize;
        self.hitcount[edge_id] = self.hitcount[edge_id].checked_add(1).unwrap_or(1);
    }

    #[cold]
    fn do_step<T: EvmTypes>(&mut self, interp: &mut Interpreter<'_, T>) {
        let address = interp.message().destination;
        let current_pc = interp.pc();

        match interp.opcode() {
            op::JUMP => {
                if let Some(jump_dest) = stack_peek(interp, 0) {
                    self.store_hit(address, current_pc, jump_dest);
                }
            }
            op::JUMPI => {
                if let Some(stack_value) = stack_peek(interp, 1) {
                    let jump_dest = if !stack_value.is_zero() {
                        stack_peek(interp, 0)
                    } else {
                        Some(U256::from(current_pc + 1))
                    };
                    if let Some(jump_dest) = jump_dest {
                        self.store_hit(address, current_pc, jump_dest);
                    }
                }
            }
            _ => {}
        }
    }
}

impl Default for EdgeCovInspector {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: EvmTypes> Inspector<T> for EdgeCovInspector {
    #[inline]
    fn step(&mut self, interp: &mut Interpreter<'_, T>) {
        if matches!(interp.opcode(), op::JUMP | op::JUMPI) {
            self.do_step(interp);
        }
    }
}

#[inline]
fn stack_peek<T: EvmTypes>(interp: &Interpreter<'_, T>, index_from_top: usize) -> Option<Word> {
    let stack = interp.stack();
    stack.get(stack.len().checked_sub(index_from_top + 1)?).copied()
}
