//! Fourbyte tracing inspector.

use alloc::format;
use alloy_primitives::{Selector, hex, map::HashMap};
use alloy_rpc_types_trace::geth::FourByteFrame;
use evm2::{
    EvmTypes, Inspector,
    interpreter::{Message, MessageResult},
};

/// Fourbyte tracing inspector that records all function selectors and their calldata sizes.
#[derive(Clone, Debug, Default)]
pub struct FourByteInspector {
    /// The map of SELECTOR to number of occurrences entries.
    inner: HashMap<(Selector, usize), u64>,
}

impl FourByteInspector {
    /// Returns the map of SELECTOR to number of occurrences entries.
    pub const fn inner(&self) -> &HashMap<(Selector, usize), u64> {
        &self.inner
    }
}

impl<T: EvmTypes> Inspector<T> for FourByteInspector {
    fn call(&mut self, message: &mut Message) -> Option<MessageResult> {
        if message.input.len() >= 4 {
            let selector =
                Selector::try_from(&message.input[..4]).expect("input is at least 4 bytes");
            let calldata_size = message.input[4..].len();
            *self.inner.entry((selector, calldata_size)).or_default() += 1;
        }
        None
    }
}

impl From<FourByteInspector> for FourByteFrame {
    fn from(value: FourByteInspector) -> Self {
        Self::from(&value)
    }
}

impl From<&FourByteInspector> for FourByteFrame {
    fn from(value: &FourByteInspector) -> Self {
        Self(
            value
                .inner
                .iter()
                .map(|((selector, calldata_size), count)| {
                    let key = format!("0x{}-{}", hex::encode(selector), *calldata_size);
                    (key, *count)
                })
                .collect(),
        )
    }
}
