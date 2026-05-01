use alloy_primitives::{Address, Bytes, U256};

/// EVM message kind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum MessageKind {
    /// Regular `CALL` message.
    #[default]
    Call,
    /// `DELEGATECALL` message.
    DelegateCall,
    /// `CALLCODE` message.
    CallCode,
    /// `CREATE` message.
    Create,
    /// `CREATE2` message.
    Create2,
    /// `STATICCALL` message.
    StaticCall,
}

/// Frame-local EVM call/create message executed by the interpreter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    /// Message kind.
    pub kind: MessageKind,
    /// Current call depth.
    pub depth: u16,
    /// Gas available to this message.
    pub gas_limit: u64,
    /// Account whose context is being executed.
    pub destination: Address,
    /// Immediate caller.
    pub caller: Address,
    /// Call input data, or initcode for create messages.
    pub input: Bytes,
    /// Value transferred with the message.
    pub value: U256,
    /// Address whose code is being executed. This can differ from `destination` for `CALLCODE`
    /// and `DELEGATECALL`.
    pub code_address: Address,
}

impl Message {
    /// EVM call depth limit.
    pub const CALL_DEPTH_LIMIT: u16 = 1024;
}

impl Default for Message {
    #[inline]
    fn default() -> Self {
        Self {
            kind: MessageKind::Call,
            depth: 0,
            gas_limit: 0,
            destination: Address::ZERO,
            caller: Address::ZERO,
            input: Bytes::new(),
            value: U256::ZERO,
            code_address: Address::ZERO,
        }
    }
}
