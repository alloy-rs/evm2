use alloc::{vec, vec::Vec};
use alloy_primitives::{Address, U256, map::DefaultHashBuilder};
use core::{
    fmt,
    hash::{BuildHasher, Hash, Hasher},
};
use evm2::{Evm, EvmTypes, Inspector, bytecode::opcode, interpreter::Interpreter};

const MAX_EDGE_COUNT: usize = 65536;

/// An `Inspector` that tracks edge coverage.
#[derive(Clone)]
pub struct EdgeCovInspector {
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
    pub fn get_hitcount(&self) -> &[u8] {
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
        let address = interp.message().code_address;
        let current_pc = interp.pc();
        let stack = interp.stack();

        match interp.opcode() {
            opcode::op::JUMP => {
                if let Some(jump_dest) = stack.peek(0) {
                    self.store_hit(address, current_pc, jump_dest);
                }
            }
            opcode::op::JUMPI => {
                if let Some(stack_value) = stack.peek(1) {
                    let jump_dest = if stack_value.is_zero() {
                        Some(U256::from(current_pc + 1))
                    } else {
                        stack.peek(0)
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

impl<T: EvmTypes<Host = Evm<T>>> Inspector<T> for EdgeCovInspector {
    #[inline]
    fn step(&mut self, interp: &mut Interpreter<'_, T>, _host: &mut T::Host) {
        if matches!(interp.opcode(), opcode::op::JUMP | opcode::op::JUMPI) {
            self.do_step(interp);
        }
    }
}
