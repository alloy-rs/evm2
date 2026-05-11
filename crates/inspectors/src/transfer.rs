use alloc::{vec, vec::Vec};
use alloy_primitives::{Address, B256, Log, LogData, U256, address, b256};
use alloy_sol_types::SolValue;
use evm2::{
    EvmTypes, Inspector,
    interpreter::{Message, MessageKind, MessageResult},
};

/// Sender of ETH transfer log per `eth_simulateV1` spec.
///
/// <https://github.com/ethereum/execution-apis/pull/484>
pub const TRANSFER_LOG_EMITTER: Address = address!("eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee");

/// Topic of `Transfer(address,address,uint256)` event.
pub const TRANSFER_EVENT_TOPIC: B256 =
    b256!("ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef");

/// An [Inspector] that collects internal ETH transfers.
#[derive(Debug, Default, Clone)]
pub struct TransferInspector {
    internal_only: bool,
    transfers: Vec<TransferOperation>,
    logs: Vec<Log>,
    /// If enabled, will collect ERC20-style transfer logs for each ETH transfer.
    insert_logs: bool,
}

impl TransferInspector {
    /// Creates a new transfer inspector.
    pub const fn new(internal_only: bool) -> Self {
        Self { internal_only, transfers: Vec::new(), logs: Vec::new(), insert_logs: false }
    }

    /// Creates a new transfer inspector that only collects internal transfers.
    pub const fn internal_only() -> Self {
        Self::new(true)
    }

    /// Consumes the inspector and returns the collected transfers.
    pub fn into_transfers(self) -> Vec<TransferOperation> {
        self.transfers
    }

    /// Sets whether to collect ERC20-style transfer logs.
    pub const fn with_logs(mut self, insert_logs: bool) -> Self {
        self.insert_logs = insert_logs;
        self
    }

    /// Returns a reference to the collected transfers.
    pub fn transfers(&self) -> &[TransferOperation] {
        &self.transfers
    }

    /// Returns collected ERC20-style transfer logs.
    pub fn logs(&self) -> &[Log] {
        &self.logs
    }

    /// Returns an iterator over the collected transfers.
    pub fn iter(&self) -> impl Iterator<Item = &TransferOperation> {
        self.transfers.iter()
    }

    fn on_transfer(
        &mut self,
        from: Address,
        to: Address,
        value: U256,
        kind: TransferKind,
        depth: u16,
    ) {
        if self.internal_only && depth <= 1 {
            return;
        }
        if value.is_zero() {
            return;
        }
        self.transfers.push(TransferOperation { kind, from, to, value });

        if self.insert_logs {
            let from = B256::from_slice(&from.abi_encode());
            let to = B256::from_slice(&to.abi_encode());
            let data = value.abi_encode();

            self.logs.push(Log {
                address: TRANSFER_LOG_EMITTER,
                data: LogData::new_unchecked(vec![TRANSFER_EVENT_TOPIC, from, to], data.into()),
            });
        }
    }
}

impl<T: EvmTypes> Inspector<T> for TransferInspector {
    fn call(&mut self, message: &mut Message) -> Option<MessageResult> {
        if matches!(message.kind, MessageKind::Call | MessageKind::CallCode) {
            self.on_transfer(
                message.caller,
                message.destination,
                message.value,
                TransferKind::Call,
                message.depth,
            );
        }
        None
    }

    fn create_end(&mut self, message: &Message, result: &mut MessageResult) {
        let Some(address) = result.created_address else {
            return;
        };
        let kind = match message.kind {
            MessageKind::Create => TransferKind::Create,
            MessageKind::Create2 => TransferKind::Create2,
            _ => return,
        };
        self.on_transfer(message.caller, address, message.value, kind, message.depth);
    }

    fn selfdestruct(&mut self, contract: Address, target: Address, value: U256) {
        self.transfers.push(TransferOperation {
            kind: TransferKind::SelfDestruct,
            from: contract,
            to: target,
            value,
        });
    }
}

/// A transfer operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferOperation {
    /// Source of the transfer call.
    pub kind: TransferKind,
    /// Sender of the transfer.
    pub from: Address,
    /// Receiver of the transfer.
    pub to: Address,
    /// Value of the transfer.
    pub value: U256,
}

/// The kind of transfer operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferKind {
    /// A non-zero value transfer CALL.
    Call,
    /// A CREATE operation.
    Create,
    /// A CREATE2 operation.
    Create2,
    /// A SELFDESTRUCT operation.
    SelfDestruct,
}
