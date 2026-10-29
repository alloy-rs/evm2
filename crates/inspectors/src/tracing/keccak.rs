//! Keccak256 preimage tracing inspector
//!
//! The keccak256PreimageTracer collects the preimage of every `KECCAK256` operation executed in
//! the lifetime of a transaction. The result is a map of hash to preimage.
//!
//! Unlike geth, it does not record an operation that runs out of gas for lack of gas to hash the
//! preimage or expand memory for it, which bounds the work the tracer does by the gas of the
//! transaction.
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

/// The most zero padding a preimage that reaches past the end of memory may need.
const MEMORY_PAD_LIMIT: u64 = 1024 * 1024;

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

        // Skip an operation that cannot pay for hashing and expanding memory: it fails with out of
        // gas, and a transaction could otherwise make every such operation copy up to
        // `MEMORY_PAD_LIMIT` bytes for next to no gas.
        let gas_params = &interp.version().gas_params;
        let usize_of = |value: u64| usize::try_from(value).unwrap_or(usize::MAX);
        let words = memory.len().div_ceil(32);
        let new_words = if length == 0 {
            words
        } else {
            words.max(usize_of(offset.saturating_add(length)).div_ceil(32))
        };
        let gas = gas_params
            .keccak256_word_cost(usize_of(length))
            .saturating_add(gas_params.memory_cost(new_words) - gas_params.memory_cost(words));
        if gas > interp.gas().remaining() {
            return;
        }

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
