//! Otterscan internal operations inspector.

use alloc::vec::Vec;
use alloy_primitives::{Address, U256};
use alloy_rpc_types_trace::otterscan::{InternalOperation, OperationType};
use evm2::{
    EvmTypesHost, Inspector,
    interpreter::{Interpreter, Message, MessageKind, MessageResult},
};

/// Records attempted internal operations for Otterscan's `ots_getInternalOperations`.
///
/// Collects non-zero-value `CALL` transfers, `CREATE`, `CREATE2`, and `SELFDESTRUCT`
/// operations in execution order without retaining call inputs or outputs. Top-level calls and
/// creations are excluded, while zero-value creations and self-destructs are included.
///
/// Operations are retained even if their frame or an ancestor reverts. This is an execution
/// history, not a list of committed balance changes.
#[derive(Debug, Default, Clone)]
pub struct InternalOperationsInspector {
    operations: Vec<InternalOperation>,
}

impl InternalOperationsInspector {
    /// Returns the collected operations in execution order.
    pub fn operations(&self) -> &[InternalOperation] {
        &self.operations
    }

    /// Consumes the inspector and returns the collected operations in execution order.
    pub fn into_operations(self) -> Vec<InternalOperation> {
        self.operations
    }

    fn on_operation<T: EvmTypesHost>(&mut self, message: &Message<T>) {
        if message.depth == 0 {
            return;
        }
        let r#type = match message.kind {
            MessageKind::Call if !message.value.is_zero() => OperationType::OpTransfer,
            MessageKind::Create => OperationType::OpCreate,
            MessageKind::Create2 => OperationType::OpCreate2,
            _ => return,
        };
        self.operations.push(InternalOperation {
            from: message.caller,
            to: message.destination,
            value: message.value,
            r#type,
        });
    }
}

impl<T: EvmTypesHost> Inspector<T> for InternalOperationsInspector {
    fn call(
        &mut self,
        _interp: &mut Interpreter<'_, '_, T>,
        message: &mut Message<T>,
    ) -> Option<MessageResult<T>> {
        self.on_operation::<T>(message);
        None
    }

    fn create(
        &mut self,
        _interp: &mut Interpreter<'_, '_, T>,
        message: &mut Message<T>,
    ) -> Option<MessageResult<T>> {
        self.on_operation::<T>(message);
        None
    }

    fn selfdestruct(
        &mut self,
        contract: &Address,
        target: &Address,
        value: &U256,
        _host: &mut T::Host<'_>,
    ) {
        self.operations.push(InternalOperation {
            from: *contract,
            to: *target,
            value: *value,
            r#type: OperationType::OpSelfDestruct,
        });
    }
}
