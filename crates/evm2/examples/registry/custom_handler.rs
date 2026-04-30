//! Custom transaction handler object with a custom envelope.

use evm2::registry::{HandlerResult, TxHandler, TxRegistry, TxRequest};

const CUSTOM_TX_TYPE: u8 = 0x42;

/// A simple custom EIP-2718-style transaction.
#[derive(Debug)]
struct CustomTx {
    gas_limit: u64,
    nonce: u64,
}

impl CustomTx {
    const fn ty(&self) -> u8 {
        CUSTOM_TX_TYPE
    }
}

/// A minimal custom envelope that can grow more variants later.
#[derive(Debug)]
enum CustomEnvelope {
    Custom(CustomTx),
}

impl CustomEnvelope {
    const fn as_custom_tx(&self) -> Option<&CustomTx> {
        match self {
            Self::Custom(tx) => Some(tx),
        }
    }

    const fn ty(&self) -> u8 {
        match self {
            Self::Custom(tx) => tx.ty(),
        }
    }
}

#[derive(Debug)]
struct Receipt {
    success: bool,
    cumulative_gas_used: u64,
    logs: Vec<String>,
}

const fn dummy_receipt(cumulative_gas_used: u64) -> Receipt {
    Receipt { success: true, cumulative_gas_used, logs: Vec::new() }
}

/// A concrete handler object instead of a bare function.
///
/// This is useful when a handler needs configuration, dependencies, caches, or
/// policy objects. The registry accepts it because it implements `TxHandler<Tx, Output>`.
struct CustomTxHandler {
    intrinsic_gas: u64,
}

impl TxHandler<CustomTx, Receipt> for CustomTxHandler {
    fn call(&self, req: TxRequest<'_, CustomTx>) -> HandlerResult<Receipt> {
        let gas_used = self.intrinsic_gas + req.tx.gas_limit / 10 + req.tx.nonce;
        Ok(dummy_receipt(gas_used))
    }
}

fn build_registry() -> TxRegistry<CustomEnvelope, Receipt> {
    TxRegistry::<CustomEnvelope, Receipt>::new().with_handler(
        CUSTOM_TX_TYPE,
        CustomEnvelope::as_custom_tx,
        CustomTxHandler { intrinsic_gas: 21_000 },
    )
}

fn main() -> HandlerResult<()> {
    let registry = build_registry();

    let tx = CustomEnvelope::Custom(CustomTx { gas_limit: 100_000, nonce: 7 });

    let receipt = registry.try_get_by_type(tx.ty())?.call(&tx)?;

    println!(
        "custom tx type=0x{:02x}: success={}, cumulative_gas_used={}, logs={}",
        tx.ty(),
        receipt.success,
        receipt.cumulative_gas_used,
        receipt.logs.len()
    );

    Ok(())
}
