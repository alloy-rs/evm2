//! Ethereum transaction envelope and handlers.

use crate::{
    Evm, EvmTypes, SpecId, TxResult, Version,
    bytecode::Bytecode,
    env::TxEnv,
    evm::precompile::PrecompileProvider,
    interpreter::{Host, Message, MessageKind, Word},
    registry::{HandlerError, HandlerResult, TxRegistry, TxRequest},
    utils::num_words,
    version::GasId,
};
use alloy_consensus::{TxEip1559, TxEip2930, TxEip7702, TxLegacy, transaction::Recovered};
use alloy_eips::eip2718::Typed2718;
use alloy_primitives::{B256, Bytes, TxKind, U256};

/// Ethereum transaction envelope containing recovered transactions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecoveredTxEnvelope {
    /// Legacy transaction.
    Legacy(Recovered<TxLegacy>),
    /// EIP-2930 access-list transaction.
    Eip2930(Recovered<TxEip2930>),
    /// EIP-1559 dynamic-fee transaction.
    Eip1559(Recovered<TxEip1559>),
    /// EIP-7702 set-code transaction.
    Eip7702(Recovered<TxEip7702>),
}

impl RecoveredTxEnvelope {
    /// Returns the contained legacy transaction, if this is legacy.
    pub const fn as_legacy(&self) -> Option<&Recovered<TxLegacy>> {
        match self {
            Self::Legacy(tx) => Some(tx),
            Self::Eip2930(_) | Self::Eip1559(_) | Self::Eip7702(_) => None,
        }
    }

    /// Returns the contained EIP-2930 transaction, if this is EIP-2930.
    pub const fn as_eip2930(&self) -> Option<&Recovered<TxEip2930>> {
        match self {
            Self::Eip2930(tx) => Some(tx),
            Self::Legacy(_) | Self::Eip1559(_) | Self::Eip7702(_) => None,
        }
    }

    /// Returns the contained EIP-1559 transaction, if this is EIP-1559.
    pub const fn as_eip1559(&self) -> Option<&Recovered<TxEip1559>> {
        match self {
            Self::Eip1559(tx) => Some(tx),
            Self::Legacy(_) | Self::Eip2930(_) | Self::Eip7702(_) => None,
        }
    }

    /// Returns the contained EIP-7702 transaction, if this is EIP-7702.
    pub const fn as_eip7702(&self) -> Option<&Recovered<TxEip7702>> {
        match self {
            Self::Eip7702(tx) => Some(tx),
            Self::Legacy(_) | Self::Eip2930(_) | Self::Eip1559(_) => None,
        }
    }
}

impl Typed2718 for RecoveredTxEnvelope {
    fn ty(&self) -> u8 {
        match self {
            Self::Legacy(tx) => tx.ty(),
            Self::Eip2930(tx) => tx.ty(),
            Self::Eip1559(tx) => tx.ty(),
            Self::Eip7702(tx) => tx.ty(),
        }
    }
}

/// Returns the Ethereum transaction registry.
///
/// Currently only legacy transactions are registered. Future Ethereum typed
/// transaction handlers should be added here.
pub fn ethereum_tx_registry<T: EvmTypes<Host = Evm<T>>>()
-> TxRegistry<RecoveredTxEnvelope, TxResult, Evm<T>> {
    TxRegistry::new().with_handler(0, RecoveredTxEnvelope::as_legacy, handle_legacy::<T>)
}

fn handle_legacy<T: EvmTypes<Host = Evm<T>>>(
    req: TxRequest<'_, Recovered<TxLegacy>, Evm<T>>,
) -> HandlerResult<TxResult> {
    let caller = req.tx.signer();
    let tx = req.tx.inner();
    let spec_id = req.host.spec_id();
    let gas_price = U256::from(tx.gas_price);
    if spec_id.enables(SpecId::LONDON) && gas_price < req.host.block.basefee {
        return Err(HandlerError::FeeCapLessThanBaseFee {
            max_fee_per_gas: gas_price,
            base_fee: req.host.block.basefee,
        });
    }
    let intrinsic = legacy_intrinsic_gas(req.host.version(), tx);
    if tx.gas_limit < intrinsic {
        return Err(HandlerError::IntrinsicGasTooLow { required: intrinsic, got: tx.gas_limit });
    }

    let sender_info = req.host.state.account_info(caller).unwrap_or_default();
    if sender_info.nonce != tx.nonce {
        return Err(HandlerError::InvalidNonce { expected: sender_info.nonce, got: tx.nonce });
    }

    let max_gas_cost = U256::from(tx.gas_limit) * gas_price;
    let max_upfront = max_gas_cost.saturating_add(tx.value);
    if sender_info.balance < max_upfront {
        return Err(HandlerError::InsufficientFunds);
    }

    req.host.state.warm_account(caller);
    if spec_id.enables(SpecId::SHANGHAI) {
        req.host.state.warm_account(req.host.block.beneficiary);
    }
    if let TxKind::Call(to) = tx.to {
        req.host.state.warm_account(to);
    }
    req.host.state.warm_accounts(req.host.precompiles().warm_addresses());

    req.host.state.add_balance(caller, Word::ZERO.wrapping_sub(max_gas_cost));
    req.host.state.increment_nonce(caller);
    let execution_checkpoint = req.host.state.checkpoint();

    let gas_limit = tx.gas_limit - intrinsic;
    let tx_env = TxEnv {
        origin: caller,
        gas_price,
        chain_id: tx.chain_id.map(U256::from).unwrap_or(U256::ONE),
        ..TxEnv::default()
    };

    let (bytecode, message) = match tx.to {
        TxKind::Call(to) => {
            let code = req.host.state.get_code(to);
            let message = Message {
                kind: MessageKind::Call,
                depth: 0,
                gas_limit,
                destination: to,
                caller,
                input: tx.input.clone(),
                value: tx.value,
                code_address: to,
                salt: B256::ZERO,
            };
            (code, message)
        }
        TxKind::Create => {
            let address = caller.create(tx.nonce);
            let message = Message {
                kind: MessageKind::Create,
                depth: 0,
                gas_limit,
                destination: address,
                caller,
                input: Bytes::new(),
                value: tx.value,
                code_address: address,
                salt: B256::ZERO,
            };
            (Bytecode::new_legacy(tx.input.clone()), message)
        }
    };

    let mut result = req.host.execute_message(&tx_env, bytecode, &message, false);
    if !result.stop.is_success() {
        req.host.state.rollback(execution_checkpoint);
        if result.stop.is_halt() {
            result.gas_remaining = 0;
        }
    }

    let gas_remaining =
        result.gas_remaining_after_final_refund(tx.gas_limit, spec_id.enables(SpecId::LONDON));
    let gas_used =
        result.gas_used_after_final_refund(tx.gas_limit, spec_id.enables(SpecId::LONDON));
    req.host.state.add_balance(caller, U256::from(gas_remaining) * gas_price);
    let beneficiary_gas_price = if spec_id.enables(SpecId::LONDON) {
        gas_price.saturating_sub(req.host.block.basefee)
    } else {
        gas_price
    };
    req.host
        .state
        .add_balance(req.host.block.beneficiary, U256::from(gas_used) * beneficiary_gas_price);
    Ok(TxResult {
        status: result.stop.is_success(),
        gas_used,
        stop: result.stop,
        output: result.output,
        ..TxResult::default()
    })
}

/// Calculates intrinsic legacy transaction gas.
pub fn legacy_intrinsic_gas(version: &Version, tx: &TxLegacy) -> u64 {
    let spec = version.spec_id();
    let non_zero_multiplier = if spec.enables(SpecId::ISTANBUL) { 16 } else { 68 };
    let mut gas = 21_000;
    for byte in &tx.input {
        gas += if *byte == 0 { 4 } else { non_zero_multiplier };
    }
    if tx.to.is_create() && spec.enables(SpecId::HOMESTEAD) {
        gas += 32_000;
    }
    if tx.to.is_create() && spec.enables(SpecId::SHANGHAI) {
        gas += u64::from(version.gas_params().get(GasId::TxInitcodeCost))
            * num_words(tx.input.len()) as u64;
    }
    gas
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn legacy_tx(to: TxKind, input: Bytes) -> TxLegacy {
        TxLegacy {
            chain_id: None,
            nonce: 0,
            gas_price: 0,
            gas_limit: 0,
            to,
            value: U256::ZERO,
            input,
        }
    }

    #[test]
    fn legacy_intrinsic_gas_charges_shanghai_create_initcode_words() {
        let tx = legacy_tx(TxKind::Create, Bytes::from(vec![1; 74]));

        assert_eq!(
            legacy_intrinsic_gas(&Version::base(SpecId::LONDON), &tx),
            21_000 + 32_000 + 74 * 16
        );
        assert_eq!(
            legacy_intrinsic_gas(&Version::base(SpecId::SHANGHAI), &tx),
            21_000 + 32_000 + 74 * 16 + 3 * 2
        );
    }
}
