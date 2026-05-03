//! Transaction handler registry with Alloy transaction envelopes.

use alloy_consensus::{Receipt, Signed, TxEip1559, TxEip7702, TxEnvelope, TxLegacy, TxType};
use alloy_eips::eip2718::Typed2718;
use alloy_primitives::Signature;
use evm2::registry::{HandlerResult, TxRegistry, TxRequest};

const fn dummy_receipt(cumulative_gas_used: u64) -> Receipt {
    Receipt {
        status: alloy_consensus::Eip658Value::success(),
        cumulative_gas_used,
        logs: Vec::new(),
    }
}

const fn handle_legacy(req: TxRequest<'_, Signed<TxLegacy>>) -> HandlerResult<Receipt> {
    // A dummy receipt. A real handler would validate, run the interpreter, settle gas,
    // collect logs, and then build the receipt from the execution result.
    Ok(dummy_receipt(req.tx.tx().gas_limit))
}

const fn handle_eip1559(req: TxRequest<'_, Signed<TxEip1559>>) -> HandlerResult<Receipt> {
    let tx = req.tx.tx();

    Ok(dummy_receipt(tx.gas_limit))
}

const fn handle_eip7702(req: TxRequest<'_, Signed<TxEip7702>>) -> HandlerResult<Receipt> {
    let tx = req.tx.tx();

    Ok(dummy_receipt(tx.gas_limit))
}

fn build_registry() -> TxRegistry<TxEnvelope, Receipt> {
    // Each registration has three parts:
    //
    // 1. The runtime dispatch key. Here we use the EIP-2718 transaction type byte.
    // 2. A typed extractor from the erased envelope (`TxEnvelope`) to the concrete tx.
    // 3. A typed handler that receives that concrete tx type.
    //
    // After registration the registry is type-erased, but `handle_legacy` still sees
    // `Signed<TxLegacy>`, not `dyn Any` or a generic transaction view.
    //
    // Functions automatically implement `TxHandler<Tx, Output>`, so no explicit adapter is needed.
    TxRegistry::<TxEnvelope, Receipt>::new()
        .with_handler(TxType::Legacy.ty(), TxEnvelope::as_legacy, handle_legacy)
        // The EIP-1559 handler is a different concrete function type. It can access
        // EIP-1559 fields directly after the extractor succeeds.
        .with_handler(TxType::Eip1559.ty(), TxEnvelope::as_eip1559, handle_eip1559)
        // The same function-handler registration works for EIP-7702. A real EVM
        // handler would validate, apply authorizations, enter the interpreter, and
        // settle gas before returning a receipt.
        .with_handler(TxType::Eip7702.ty(), TxEnvelope::as_eip7702, handle_eip7702)
}

fn sample_transactions() -> [TxEnvelope; 3] {
    let legacy = TxEnvelope::Legacy(Signed::new_unhashed(
        TxLegacy { gas_limit: 21_000, ..Default::default() },
        Signature::test_signature(),
    ));

    let eip1559 = TxEnvelope::Eip1559(Signed::new_unhashed(
        TxEip1559 {
            gas_limit: 21_000,
            max_fee_per_gas: 10,
            max_priority_fee_per_gas: 2,
            ..Default::default()
        },
        Signature::test_signature(),
    ));

    let eip7702 = TxEnvelope::Eip7702(Signed::new_unhashed(
        TxEip7702 { gas_limit: 21_000, authorization_list: Vec::new(), ..Default::default() },
        Signature::test_signature(),
    ));

    [legacy, eip1559, eip7702]
}

fn main() -> HandlerResult<()> {
    let registry = build_registry();
    let transactions = sample_transactions();

    for tx in &transactions {
        let receipt = registry.try_get_by_type(tx.ty())?.call(tx, &mut ())?;
        println!(
            "{:?}: success={}, cumulative_gas_used={}, logs={}",
            tx.tx_type(),
            receipt.status.coerce_status(),
            receipt.cumulative_gas_used,
            receipt.logs.len()
        );
    }

    Ok(())
}
