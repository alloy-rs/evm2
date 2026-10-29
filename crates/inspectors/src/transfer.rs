//! Internal transfer inspector.

use alloc::{vec, vec::Vec};
use alloy_primitives::{Address, B256, Log, U256, address, b256};
use alloy_sol_types::SolValue;
use evm2::{
    Evm, EvmTypes, Inspector,
    interpreter::{Interpreter, Message, MessageKind, MessageResult},
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
/// Transfers made by a call or create that fails, including transfers in its subcalls, are
/// discarded together with their logs.
///
/// Like `go-ethereum`'s `eth_simulateV1` tracer, the logs are kept by the inspector and are never
/// added to the EVM's own logs, so receipts and log blooms do not include them.
///
/// This can be used to construct an `eth_simulateV1` response. For `ots_getInternalOperations`,
/// use [InternalOperationsInspector](crate::otterscan::InternalOperationsInspector).
#[derive(Debug, Default, Clone)]
pub struct TransferInspector {
    internal_only: bool,
    transfers: Vec<TransferOperation>,
    /// Number of transfers recorded before each call and create frame currently executing.
    checkpoints: Vec<usize>,
    /// If enabled, collects the emitted logs together with ERC20-style transfer logs emitted by
    /// [TRANSFER_LOG_EMITTER] for each ETH transfer.
    ///
    /// Can be used for [eth_simulateV1](https://github.com/ethereum/execution-apis/pull/484) execution.
    collect_logs: bool,
    /// Logs of each call and create frame currently executing.
    frames: Vec<Vec<Log>>,
    /// Logs of the top-level frames that succeeded.
    logs: Vec<Log>,
}

impl TransferInspector {
    /// Creates a new transfer inspector.
    ///
    /// If `internal_only` is set to `true`, only internal transfers are collected, in other words,
    /// the top level call is ignored.
    pub const fn new(internal_only: bool) -> Self {
        Self {
            internal_only,
            transfers: Vec::new(),
            checkpoints: Vec::new(),
            collect_logs: false,
            frames: Vec::new(),
            logs: Vec::new(),
        }
    }

    /// Creates a new transfer inspector that only collects internal transfers.
    pub const fn internal_only() -> Self {
        Self::new(true)
    }

    /// Consumes the inspector and returns the collected transfers.
    pub fn into_transfers(self) -> Vec<TransferOperation> {
        self.transfers
    }

    /// Sets whether to collect the emitted logs and the ERC20-style transfer logs.
    ///
    /// Once EIP-7708 is active the EVM emits the transfer logs itself, so this must be disabled
    /// to report each transfer once, as `eth_simulateV1` does.
    pub const fn with_logs(mut self, collect_logs: bool) -> Self {
        self.collect_logs = collect_logs;
        self
    }

    /// Returns a reference to the logs of the succeeded top-level frames, in emission order.
    pub fn logs(&self) -> &[Log] {
        &self.logs
    }

    /// Takes the logs of the succeeded top-level frames, in emission order.
    pub fn take_logs(&mut self) -> Vec<Log> {
        core::mem::take(&mut self.logs)
    }

    /// Returns a reference to the collected transfers.
    pub fn transfers(&self) -> &[TransferOperation] {
        &self.transfers
    }

    /// Returns an iterator over the collected transfers.
    pub fn iter(&self) -> impl Iterator<Item = &TransferOperation> {
        self.transfers.iter()
    }

    fn on_transfer<T: EvmTypes>(&mut self, message: &Message<T>, host: &mut Evm<'_, T>) {
        self.checkpoints.push(self.transfers.len());
        if self.collect_logs {
            self.frames.push(Vec::new());
        }

        let kind = match message.kind {
            MessageKind::Call => TransferKind::Call,
            MessageKind::Create => TransferKind::Create,
            MessageKind::Create2 => TransferKind::Create2,
            _ => return,
        };

        // skip top level transfers
        if self.internal_only && message.depth == host.version().initial_call_depth {
            return;
        }

        self.record_transfer(kind, message.caller, message.destination, message.value);
    }

    fn on_transfer_end(&mut self, failed: bool) {
        let Some(transfers) = self.checkpoints.pop() else { return };
        if failed {
            self.transfers.truncate(transfers);
        }
        if self.collect_logs
            && let Some(frame) = self.frames.pop()
            && !failed
        {
            match self.frames.last_mut() {
                Some(parent) => parent.extend(frame),
                None => self.logs.extend(frame),
            }
        }
    }

    fn record_transfer(&mut self, kind: TransferKind, from: Address, to: Address, value: U256) {
        // skip zero transfers
        if value.is_zero() {
            return;
        }

        self.transfers.push(TransferOperation { kind, from, to, value });

        if self.collect_logs
            && let Some(frame) = self.frames.last_mut()
        {
            let from = B256::from_slice(&from.abi_encode());
            let to = B256::from_slice(&to.abi_encode());
            let data = value.abi_encode();

            frame.push(Log::new_unchecked(
                TRANSFER_LOG_EMITTER,
                vec![TRANSFER_EVENT_TOPIC, from, to],
                data.into(),
            ));
        }
    }
}

impl<T: EvmTypes> Inspector<T> for TransferInspector {
    fn log(&mut self, log: &Log, _host: &mut T::Host<'_>) {
        if self.collect_logs
            && let Some(frame) = self.frames.last_mut()
        {
            frame.push(log.clone());
        }
    }

    fn call(
        &mut self,
        interp: &mut Interpreter<'_, '_, T>,
        message: &mut Message<T>,
    ) -> Option<MessageResult<T>> {
        self.on_transfer::<T>(message, interp.host());
        None
    }

    fn call_end(
        &mut self,
        _interp: &mut Interpreter<'_, '_, T>,
        _message: &Message<T>,
        result: &mut MessageResult<T>,
    ) {
        self.on_transfer_end(!result.is_success());
    }

    fn create(
        &mut self,
        interp: &mut Interpreter<'_, '_, T>,
        message: &mut Message<T>,
    ) -> Option<MessageResult<T>> {
        self.on_transfer::<T>(message, interp.host());
        None
    }

    fn create_end(
        &mut self,
        _interp: &mut Interpreter<'_, '_, T>,
        _message: &Message<T>,
        result: &mut MessageResult<T>,
    ) {
        // A create that stops early without a created address, such as on nonce overflow, also
        // counts as failed even though its stop is `Return`.
        let failed = !result.is_success() || result.created_address.is_none();
        self.on_transfer_end(failed);
    }

    fn selfdestruct(
        &mut self,
        contract: &Address,
        target: &Address,
        value: &U256,
        _host: &mut T::Host<'_>,
    ) {
        self.record_transfer(TransferKind::SelfDestruct, *contract, *target, *value);
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
