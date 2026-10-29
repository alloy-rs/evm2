//! Keccak256 preimage tracing inspector
//!
//! The keccak256PreimageTracer collects the preimage of every `KECCAK256` operation executed in
//! the lifetime of a transaction. The result is a map of hash to preimage.
//!
//! Like geth, it records an operation that goes on to fail. [`MEMORY_PAD_LIMIT`] bounds the work
//! per operation, so a transaction cannot make the tracer copy more than that much padding.
//!
//! See also <https://geth.ethereum.org/docs/developers/evm-tracing/built-in-tracers>

use alloc::{borrow::Cow, vec};
use alloy_primitives::{B256, Bytes, hex, keccak256, map::HashMap};
use evm2::{
    EvmTypes, Inspector,
    interpreter::{Interpreter, opcode::op},
};
use serde_json::Value;

/// The name of the tracer in the `tracer` option of `debug_trace*` and in a mux tracer config.
pub const KECCAK256_PREIMAGE_TRACER: &str = "keccak256PreimageTracer";

/// The most zero padding, in bytes, a preimage that reaches past the end of memory may need. geth
/// records nothing for a preimage that needs more.
pub const MEMORY_PAD_LIMIT: u64 = 1024 * 1024;

/// Inspector that records the preimage of every `KECCAK256` operation.
#[derive(Clone, Debug, Default)]
pub struct Keccak256PreimageInspector {
    /// The map of hash to preimage.
    inner: HashMap<B256, Bytes>,
}

impl Keccak256PreimageInspector {
    /// Clears the recorded preimages.
    pub fn clear(&mut self) {
        self.inner.clear();
    }

    /// Returns the result of the tracer: a JSON object mapping each hash to its preimage.
    pub fn json_result(&self) -> Value {
        Value::Object(
            self.inner
                .iter()
                .map(|(hash, preimage)| {
                    (hex::encode_prefixed(hash), hex::encode_prefixed(preimage).into())
                })
                .collect(),
        )
    }
}

impl<T: EvmTypes> Inspector<T> for Keccak256PreimageInspector {
    fn step(&mut self, interp: &mut Interpreter<'_, '_, T>) {
        if interp.opcode() != op::KECCAK256 {
            return;
        }
        // The stack can be too small if the operation is about to fail.
        let &[.., length, offset] = interp.stack().as_slice() else { return };
        // The operands are truncated to 64 bits.
        let (offset, length) = (offset.as_limbs()[0], length.as_limbs()[0]);
        let memory = interp.memory().as_slice();
        if let Some(preimage) = memory_copy_padded(memory, offset, length) {
            self.inner.entry(keccak256(&preimage)).or_insert_with(|| preimage.into_owned().into());
        }
    }
}

/// Returns the `size` bytes of `memory` at `offset`, zero padded past its end.
///
/// Returns `None` if the range reaches more than [`MEMORY_PAD_LIMIT`] past the end of memory.
fn memory_copy_padded(memory: &[u8], offset: u64, size: u64) -> Option<Cow<'_, [u8]>> {
    let end = offset.saturating_add(size);
    if end.saturating_sub(memory.len() as u64) > MEMORY_PAD_LIMIT {
        return None;
    }
    if let Some(preimage) = memory.get(offset as usize..end as usize) {
        return Some(Cow::Borrowed(preimage));
    }
    let mut preimage = vec![0; size as usize];
    if let Some(overlap) = memory.get(offset as usize..) {
        preimage[..overlap.len()].copy_from_slice(overlap);
    }
    Some(Cow::Owned(preimage))
}
