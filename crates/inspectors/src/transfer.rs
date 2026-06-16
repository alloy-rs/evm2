//! Internal transfer inspector.

use alloc::{vec, vec::Vec};
use alloy_primitives::{Address, B256, Log, LogData, U256, address, b256};
use alloy_sol_types::SolValue;
use evm2::{
    EvmTypes, Inspector,
    interpreter::{Host, Interpreter, Message, MessageKind, MessageResult},
};

/// Sender of ETH transfer log per `eth_simulateV1` spec.
///
/// <https://github.com/ethereum/execution-apis/pull/484>
pub const TRANSFER_LOG_EMITTER: Address = address!("eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee");

/// Topic of `Transfer(address,address,uint256)` event.
pub const TRANSFER_EVENT_TOPIC: B256 =
    b256!("ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef");

/// An [Inspector] that collects internal ETH transfers.
///
/// This can be used to construct `ots_getInternalOperations` or `eth_simulateV1` response.
#[derive(Debug, Default, Clone)]
pub struct TransferInspector {
    internal_only: bool,
    transfers: Vec<TransferOperation>,
    /// If enabled, will insert ERC20-style transfer logs emitted by [TRANSFER_LOG_EMITTER] for
    /// each ETH transfer.
    ///
    /// Can be used for [eth_simulateV1](https://github.com/ethereum/execution-apis/pull/484) execution.
    insert_logs: bool,
}

impl TransferInspector {
    /// Creates a new transfer inspector.
    ///
    /// If `internal_only` is set to `true`, only internal transfers are collected, in other words,
    /// the top level call is ignored.
    pub const fn new(internal_only: bool) -> Self {
        Self { internal_only, transfers: Vec::new(), insert_logs: false }
    }

    /// Creates a new transfer inspector that only collects internal transfers.
    pub const fn internal_only() -> Self {
        Self::new(true)
    }

    /// Consumes the inspector and returns the collected transfers.
    pub fn into_transfers(self) -> Vec<TransferOperation> {
        self.transfers
    }

    /// Sets whether to insert ERC20-style transfer logs.
    pub const fn with_logs(mut self, insert_logs: bool) -> Self {
        self.insert_logs = insert_logs;
        self
    }

    /// Returns a reference to the collected transfers.
    pub fn transfers(&self) -> &[TransferOperation] {
        &self.transfers
    }

    /// Returns an iterator over the collected transfers.
    pub fn iter(&self) -> impl Iterator<Item = &TransferOperation> {
        self.transfers.iter()
    }

    fn on_transfer<T: EvmTypes>(&mut self, message: &Message<T>, host: &mut T::Host) {
        let kind = match message.kind {
            MessageKind::Call | MessageKind::CallCode => TransferKind::Call,
            MessageKind::Create => TransferKind::Create,
            MessageKind::Create2 => TransferKind::Create2,
            _ => return,
        };

        // skip top level transfers
        if self.internal_only && message.depth == 0 {
            return;
        }
        // skip zero transfers
        let value = message.value;
        if value.is_zero() {
            return;
        }

        let from = message.caller;
        let to = message.destination;
        self.transfers.push(TransferOperation { kind, from, to, value });

        if self.insert_logs {
            let from = B256::from_slice(&from.abi_encode());
            let to = B256::from_slice(&to.abi_encode());
            let data = value.abi_encode();

            let log = Log {
                address: TRANSFER_LOG_EMITTER,
                data: LogData::new_unchecked(vec![TRANSFER_EVENT_TOPIC, from, to], data.into()),
            };
            host.log(log);
        }
    }
}

impl<T: EvmTypes> Inspector<T> for TransferInspector
where
    T::Host: Host<T>,
{
    fn call(
        &mut self,
        interp: &mut Interpreter<'_, T>,
        message: &mut Message<T>,
    ) -> Option<MessageResult<T>> {
        self.on_transfer(message, interp.host());
        None
    }

    fn create(
        &mut self,
        interp: &mut Interpreter<'_, T>,
        message: &mut Message<T>,
    ) -> Option<MessageResult<T>> {
        self.on_transfer(message, interp.host());
        None
    }

    fn selfdestruct(
        &mut self,
        contract: &Address,
        target: &Address,
        value: &U256,
        _host: &mut T::Host,
    ) {
        self.transfers.push(TransferOperation {
            kind: TransferKind::SelfDestruct,
            from: *contract,
            to: *target,
            value: *value,
        });
    }
}

/// A transfer operation.
#[derive(Debug, Clone, PartialEq, Eq)]
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
#[allow(missing_copy_implementations)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferKind {
    /// A non-zero value transfer CALL
    Call,
    /// A CREATE operation
    Create,
    /// A CREATE2 operation
    Create2,
    /// A SELFDESTRUCT operation
    SelfDestruct,
}
