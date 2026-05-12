//! Custom transaction envelope and registry handlers.

use crate::config::CustomTypes;
use alloy_eips::eip2718::Typed2718;
use alloy_primitives::{Address, Bytes};
use evm2::{
    Evm,
    bytecode::Bytecode,
    interpreter::{Host, Message},
    registry::{HandlerResult, TxRegistry, TxRequest},
};

pub const EXECUTE_CODE_TX_TYPE: u8 = 0x7f;

#[derive(Debug)]
pub enum CustomEnvelope {
    ExecuteCode(ExecuteCodeTx),
}

impl Typed2718 for CustomEnvelope {
    fn ty(&self) -> u8 {
        match self {
            Self::ExecuteCode(tx) => tx.ty(),
        }
    }
}

impl CustomEnvelope {
    pub const fn as_execute_code(&self) -> Option<&ExecuteCodeTx> {
        match self {
            Self::ExecuteCode(tx) => Some(tx),
        }
    }
}

#[derive(Debug)]
pub struct ExecuteCodeTx {
    pub target: Address,
    pub code: Bytes,
    pub gas_limit: u64,
}

impl ExecuteCodeTx {
    pub const fn ty(&self) -> u8 {
        EXECUTE_CODE_TX_TYPE
    }
}

pub fn execute_code(
    req: TxRequest<'_, ExecuteCodeTx, Evm<CustomTypes>>,
) -> HandlerResult<evm2::TxResult> {
    // The transaction handler owns policy; the interpreter still executes a normal message.
    let message = Message {
        gas_limit: req.tx.gas_limit,
        destination: req.tx.target,
        code_address: req.tx.target,
        ..Message::default()
    };
    let result = req.host.execute_message(
        &Default::default(),
        Bytecode::new_legacy(req.tx.code.clone()),
        &message,
        false,
    );
    Ok(evm2::TxResult {
        status: result.stop.is_success(),
        gas_used: req.tx.gas_limit - result.gas.remaining(),
        stop: result.stop,
        output: result.output,
        ..Default::default()
    })
}

pub fn custom_registry() -> TxRegistry<CustomEnvelope, evm2::TxResult, Evm<CustomTypes>> {
    // The EIP-2718 type byte selects the typed extractor and handler.
    TxRegistry::new().with_handler(
        EXECUTE_CODE_TX_TYPE,
        CustomEnvelope::as_execute_code,
        execute_code,
    )
}
