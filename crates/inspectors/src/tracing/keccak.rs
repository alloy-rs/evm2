//! Keccak256 preimage tracing inspector
//!
//! The keccak256PreimageTracer collects the preimage of every `KECCAK256` operation executed in
//! the lifetime of a transaction. The result is a map of hash to preimage.
//!
//! Like geth, it records an operation that goes on to fail. [`MEMORY_PAD_LIMIT`] bounds the work
//! per operation, and the [`TraceLimits`] of the inspector bound the bytes it retains, which geth
//! does not.
//!
//! See also <https://geth.ethereum.org/docs/developers/evm-tracing/built-in-tracers>

use crate::tracing::{TraceLimits, limits::TraceBudget};
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

/// The default limit on the bytes of all recorded preimages, 64 MiB. geth has no limit.
pub const DEFAULT_MAX_RECORDED_BYTES: usize = 64 * 1024 * 1024;

/// Inspector that records the preimage of every `KECCAK256` operation.
#[derive(Clone, Debug)]
pub struct Keccak256PreimageInspector {
    /// The map of hash to preimage.
    inner: HashMap<B256, Bytes>,
    /// The bytes of the recorded preimages.
    budget: TraceBudget,
}

impl Default for Keccak256PreimageInspector {
    fn default() -> Self {
        Self::new(TraceLimits::default().set_max_recorded_bytes(Some(DEFAULT_MAX_RECORDED_BYTES)))
    }
}

impl Keccak256PreimageInspector {
    /// Returns an inspector that records preimages up to the given limits, which default to
    /// [`DEFAULT_MAX_RECORDED_BYTES`].
    pub fn new(limits: TraceLimits) -> Self {
        Self { inner: HashMap::default(), budget: TraceBudget { limits, ..Default::default() } }
    }

    /// Returns whether a preimage was omitted after reaching the byte limit.
    pub const fn limit_exceeded(&self) -> bool {
        self.budget.exceeded()
    }

    /// Clears the recorded preimages.
    pub fn clear(&mut self) {
        self.inner.clear();
        self.budget.recorded = 0;
        self.budget.exceeded = false;
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
        if interp.opcode() != op::KECCAK256 || self.budget.exceeded() {
            return;
        }
        // The stack can be too small if the operation is about to fail.
        let &[.., length, offset] = interp.stack().as_slice() else { return };
        // The operands are truncated to 64 bits.
        let (offset, length) = (offset.as_limbs()[0], length.as_limbs()[0]);
        let memory = interp.memory().as_slice();
        if let Some(preimage) = memory_copy_padded(memory, offset, length) {
            let hash = keccak256(&preimage);
            if !self.inner.contains_key(&hash) && self.budget.reserve(preimage.len()) {
                self.inner.insert(hash, preimage.into_owned().into());
            }
        }
        self.budget.halt(interp, true);
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
