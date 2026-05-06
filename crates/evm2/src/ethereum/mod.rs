//! Ethereum transaction envelope and handlers.

mod eip1559;
mod eip2930;
mod eip4844;
mod eip7702;
mod legacy;

use crate::{
    Evm, EvmTypes, SpecId, TxResult, Version,
    bytecode::Bytecode,
    constants::MAX_INITCODE_SIZE,
    evm::{AccountInfo, StateCheckpoint, precompile::PrecompileProvider},
    interpreter::{Message, MessageKind, MessageResult, Word},
    registry::{HandlerError, HandlerResult, TxRegistry},
    utils::num_words,
    version::GasId,
};
use alloy_consensus::{
    TxEip1559, TxEip2930, TxEip7702, TxLegacy,
    transaction::{Recovered, TxEip4844Variant},
};
use alloy_eips::{eip2718::Typed2718, eip2930::AccessList};
use alloy_primitives::{Address, B256, Bytes, KECCAK256_EMPTY, TxKind, U256};

const EIP7825_TX_GAS_LIMIT_CAP: u64 = 16_777_216;

/// Ethereum transaction envelope containing recovered transactions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecoveredTxEnvelope {
    /// Legacy transaction.
    Legacy(Recovered<TxLegacy>),
    /// EIP-2930 access-list transaction.
    Eip2930(Recovered<TxEip2930>),
    /// EIP-1559 dynamic-fee transaction.
    Eip1559(Recovered<TxEip1559>),
    /// EIP-4844 blob transaction.
    Eip4844(Recovered<TxEip4844Variant>),
    /// EIP-7702 set-code transaction.
    Eip7702(Recovered<TxEip7702>),
}

impl RecoveredTxEnvelope {
    /// Returns the contained legacy transaction, if this is legacy.
    pub const fn as_legacy(&self) -> Option<&Recovered<TxLegacy>> {
        match self {
            Self::Legacy(tx) => Some(tx),
            Self::Eip2930(_) | Self::Eip1559(_) | Self::Eip4844(_) | Self::Eip7702(_) => None,
        }
    }

    /// Returns the contained EIP-2930 transaction, if this is EIP-2930.
    pub const fn as_eip2930(&self) -> Option<&Recovered<TxEip2930>> {
        match self {
            Self::Eip2930(tx) => Some(tx),
            Self::Legacy(_) | Self::Eip1559(_) | Self::Eip4844(_) | Self::Eip7702(_) => None,
        }
    }

    /// Returns the contained EIP-1559 transaction, if this is EIP-1559.
    pub const fn as_eip1559(&self) -> Option<&Recovered<TxEip1559>> {
        match self {
            Self::Eip1559(tx) => Some(tx),
            Self::Legacy(_) | Self::Eip2930(_) | Self::Eip4844(_) | Self::Eip7702(_) => None,
        }
    }

    /// Returns the contained EIP-4844 transaction, if this is EIP-4844.
    pub const fn as_eip4844(&self) -> Option<&Recovered<TxEip4844Variant>> {
        match self {
            Self::Eip4844(tx) => Some(tx),
            Self::Legacy(_) | Self::Eip2930(_) | Self::Eip1559(_) | Self::Eip7702(_) => None,
        }
    }

    /// Returns the contained EIP-7702 transaction, if this is EIP-7702.
    pub const fn as_eip7702(&self) -> Option<&Recovered<TxEip7702>> {
        match self {
            Self::Eip7702(tx) => Some(tx),
            Self::Legacy(_) | Self::Eip2930(_) | Self::Eip1559(_) | Self::Eip4844(_) => None,
        }
    }
}

impl Typed2718 for RecoveredTxEnvelope {
    fn ty(&self) -> u8 {
        match self {
            Self::Legacy(tx) => tx.ty(),
            Self::Eip2930(tx) => tx.ty(),
            Self::Eip1559(tx) => tx.ty(),
            Self::Eip4844(tx) => tx.ty(),
            Self::Eip7702(tx) => tx.ty(),
        }
    }
}

/// Returns the Ethereum transaction registry.
pub fn ethereum_tx_registry<T: EvmTypes<Host = Evm<T>>>()
-> TxRegistry<RecoveredTxEnvelope, TxResult, Evm<T>> {
    TxRegistry::new()
        .with_handler(0, RecoveredTxEnvelope::as_legacy, legacy::handle::<T>)
        .with_handler(1, RecoveredTxEnvelope::as_eip2930, eip2930::handle::<T>)
        .with_handler(2, RecoveredTxEnvelope::as_eip1559, eip1559::handle::<T>)
        .with_handler(3, RecoveredTxEnvelope::as_eip4844, eip4844::handle::<T>)
        .with_handler(4, RecoveredTxEnvelope::as_eip7702, eip7702::handle::<T>)
}

pub(super) fn validate_gas_price(
    spec_id: SpecId,
    gas_price: U256,
    basefee: U256,
) -> HandlerResult<()> {
    if spec_id.enables(SpecId::LONDON) && gas_price < basefee {
        return Err(HandlerError::FeeCapLessThanBaseFee {
            max_fee_per_gas: gas_price,
            base_fee: basefee,
        });
    }
    Ok(())
}

pub(super) fn validate_priority_fee(
    max_fee_per_gas: U256,
    max_priority_fee_per_gas: U256,
) -> HandlerResult<()> {
    if max_priority_fee_per_gas > max_fee_per_gas {
        return Err(HandlerError::PriorityFeeGreaterThanMaxFee);
    }
    Ok(())
}

pub(super) fn effective_gas_price(
    max_fee_per_gas: U256,
    max_priority_fee_per_gas: U256,
    basefee: U256,
) -> U256 {
    max_fee_per_gas.min(basefee.saturating_add(max_priority_fee_per_gas))
}

pub(super) fn validate_block_gas_limit(
    tx_gas_limit: u64,
    block_gas_limit: U256,
) -> HandlerResult<()> {
    if U256::from(tx_gas_limit) > block_gas_limit {
        return Err(HandlerError::GasLimitMoreThanBlock {
            gas_limit: tx_gas_limit,
            block_gas_limit,
        });
    }
    Ok(())
}

pub(super) const fn validate_tx_gas_limit_cap(
    spec_id: SpecId,
    tx_gas_limit: u64,
) -> HandlerResult<()> {
    // EIP-7825 caps each transaction gas limit to 2^24 in Osaka. Amsterdam/EIP-8037
    // replaces this with a regular-gas cap while allowing extra transaction gas to serve as
    // the state-gas reservoir.
    if matches!(spec_id, SpecId::OSAKA) && tx_gas_limit > EIP7825_TX_GAS_LIMIT_CAP {
        return Err(HandlerError::TxGasLimitGreaterThanCap {
            gas_limit: tx_gas_limit,
            cap: EIP7825_TX_GAS_LIMIT_CAP,
        });
    }
    Ok(())
}

pub(super) const fn validate_regular_gas_limit_cap(
    spec_id: SpecId,
    tx_gas_limit: u64,
    intrinsic: u64,
    floor_gas: u64,
) -> HandlerResult<()> {
    if spec_id.enables(SpecId::AMSTERDAM) && tx_gas_limit > EIP7825_TX_GAS_LIMIT_CAP {
        let required_regular_gas = if intrinsic > floor_gas { intrinsic } else { floor_gas };
        if required_regular_gas > EIP7825_TX_GAS_LIMIT_CAP {
            return Err(HandlerError::TxGasLimitGreaterThanCap {
                gas_limit: required_regular_gas,
                cap: EIP7825_TX_GAS_LIMIT_CAP,
            });
        }
    }
    Ok(())
}

pub(super) fn validate_create_initcode(
    spec_id: SpecId,
    to: TxKind,
    input: &Bytes,
) -> HandlerResult<()> {
    if spec_id.enables(SpecId::SHANGHAI) && to.is_create() && input.len() > MAX_INITCODE_SIZE {
        return Err(HandlerError::CreateInitCodeSizeLimit {
            limit: MAX_INITCODE_SIZE,
            got: input.len(),
        });
    }
    Ok(())
}

pub(super) const fn validate_nonce_not_overflow(nonce: u64) -> HandlerResult<()> {
    if nonce == u64::MAX {
        return Err(HandlerError::NonceOverflow);
    }
    Ok(())
}

pub(super) const fn validate_intrinsic_gas(gas_limit: u64, intrinsic: u64) -> HandlerResult<()> {
    if gas_limit < intrinsic {
        return Err(HandlerError::IntrinsicGasTooLow { required: intrinsic, got: gas_limit });
    }
    Ok(())
}

pub(super) const fn validate_floor_gas(gas_limit: u64, floor_gas: u64) -> HandlerResult<()> {
    if gas_limit < floor_gas {
        return Err(HandlerError::IntrinsicGasTooLow { required: floor_gas, got: gas_limit });
    }
    Ok(())
}

pub(super) fn validate_sender<T: EvmTypes<Host = Evm<T>>>(
    host: &mut Evm<T>,
    caller: Address,
    nonce: u64,
    max_upfront: U256,
) -> HandlerResult<AccountInfo> {
    let sender_info = host.state.account_info(caller).unwrap_or_default();
    if sender_info.code_hash != KECCAK256_EMPTY {
        let code = host.state.get_code(caller);
        if !code.is_empty() && !code.is_eip7702() {
            return Err(HandlerError::RejectCallerWithCode);
        }
    }
    if sender_info.nonce != nonce {
        return Err(HandlerError::InvalidNonce { expected: sender_info.nonce, got: nonce });
    }
    if sender_info.balance < max_upfront {
        return Err(HandlerError::InsufficientFunds);
    }
    Ok(sender_info)
}

pub(super) fn warm_base_accounts<T: EvmTypes<Host = Evm<T>>>(
    host: &mut Evm<T>,
    spec_id: SpecId,
    caller: Address,
    to: TxKind,
) {
    host.state.warm_account(caller);
    if spec_id.enables(SpecId::SHANGHAI) {
        host.state.warm_account(host.block.beneficiary);
    }
    if let TxKind::Call(to) = to {
        host.state.warm_account(to);
    }
    host.state.warm_accounts(host.precompiles().warm_addresses());
}

pub(super) fn warm_access_list<T: EvmTypes<Host = Evm<T>>>(
    host: &mut Evm<T>,
    access_list: &AccessList,
) {
    for item in access_list.iter() {
        host.state.warm_account(item.address);
        for key in &item.storage_keys {
            let _ = host.state.warm_storage(item.address, U256::from_be_bytes(key.0));
        }
    }
}

pub(super) fn charge_upfront<T: EvmTypes<Host = Evm<T>>>(
    host: &mut Evm<T>,
    caller: Address,
    max_gas_cost: U256,
) {
    host.state.add_balance(caller, Word::ZERO.wrapping_sub(max_gas_cost));
}

pub(super) fn initial_message<T: EvmTypes<Host = Evm<T>>>(
    host: &mut Evm<T>,
    caller: Address,
    nonce: u64,
    to: TxKind,
    input: &Bytes,
    value: U256,
    gas_limit: u64,
) -> (Bytecode, Message) {
    match to {
        TxKind::Call(to) => {
            let initial_code = initial_call_code(host, to);
            let message = Message {
                kind: MessageKind::Call,
                depth: 0,
                gas_limit,
                destination: to,
                caller,
                input: input.clone(),
                value,
                code_address: initial_code.code_address,
                disable_precompiles: initial_code.disable_precompiles,
                salt: B256::ZERO,
            };
            (initial_code.code, message)
        }
        TxKind::Create => {
            let address = caller.create(nonce);
            let message = Message {
                kind: MessageKind::Create,
                depth: 0,
                gas_limit,
                destination: address,
                caller,
                input: Bytes::new(),
                value,
                code_address: address,
                disable_precompiles: false,
                salt: B256::ZERO,
            };
            (Bytecode::new_legacy(input.clone()), message)
        }
    }
}

struct InitialCallCode {
    code: Bytecode,
    code_address: Address,
    disable_precompiles: bool,
}

fn initial_call_code<T: EvmTypes<Host = Evm<T>>>(
    host: &mut Evm<T>,
    to: Address,
) -> InitialCallCode {
    let code = host.state.get_code(to);
    if host.spec_id().enables(SpecId::PRAGUE)
        && let Some(delegated_address) = code.eip7702_address()
    {
        host.state.warm_account(delegated_address);
        return InitialCallCode {
            code: host.state.get_code(delegated_address),
            code_address: delegated_address,
            disable_precompiles: true,
        };
    }
    InitialCallCode { code, code_address: to, disable_precompiles: false }
}

pub(super) fn rollback_failed_execution<T: EvmTypes<Host = Evm<T>>>(
    host: &mut Evm<T>,
    checkpoint: StateCheckpoint,
    result: &mut MessageResult,
) {
    if !result.stop.is_success() {
        host.state.rollback(checkpoint, host.spec_id());
        if result.stop.is_halt() {
            result.gas_remaining = 0;
        }
    }
}

pub(super) fn settle_gas<T: EvmTypes<Host = Evm<T>>>(
    host: &mut Evm<T>,
    spec_id: SpecId,
    caller: Address,
    gas_price: U256,
    tx_gas_limit: u64,
    floor_gas: u64,
    result: MessageResult,
) -> TxResult {
    let (gas_remaining, gas_used) =
        final_tx_gas(&result, tx_gas_limit, spec_id.enables(SpecId::LONDON), floor_gas);
    host.state.add_balance(caller, U256::from(gas_remaining) * gas_price);
    let beneficiary_gas_price = if spec_id.enables(SpecId::LONDON) {
        gas_price.saturating_sub(host.block.basefee)
    } else {
        gas_price
    };
    host.state.add_balance(host.block.beneficiary, U256::from(gas_used) * beneficiary_gas_price);
    TxResult {
        status: result.stop.is_success(),
        gas_used,
        stop: result.stop,
        output: result.output,
        ..TxResult::default()
    }
}

const fn final_tx_gas(
    result: &MessageResult,
    tx_gas_limit: u64,
    is_london: bool,
    floor_gas: u64,
) -> (u64, u64) {
    let gas_remaining = result.gas_remaining_after_final_refund(tx_gas_limit, is_london);
    let gas_used = result.gas_used_after_final_refund(tx_gas_limit, is_london);
    // EIP-7623 charges at least the calldata floor after applying refunds.
    if gas_used < floor_gas {
        return (tx_gas_limit.saturating_sub(floor_gas), floor_gas);
    }
    (gas_remaining, gas_used)
}

pub(super) fn access_list_counts(access_list: &AccessList) -> (u64, u64) {
    (access_list.len() as u64, access_list.storage_keys_count() as u64)
}

/// Calculates transaction calldata floor gas.
pub(super) fn floor_gas(version: &Version, input: &Bytes) -> u64 {
    let params = version.gas_params();
    let floor_cost_per_token = u64::from(params.get(GasId::TxFloorCostPerToken));
    if floor_cost_per_token == 0 {
        return 0;
    }

    let non_zero_multiplier = u64::from(params.get(GasId::TxTokenNonZeroByteMultiplier));
    let mut tokens = 0;
    for byte in input {
        tokens += if *byte == 0 { 1 } else { non_zero_multiplier };
    }

    u64::from(params.get(GasId::TxFloorCostBase)) + tokens * floor_cost_per_token
}

/// Calculates intrinsic transaction gas.
pub(super) fn intrinsic_gas(
    version: &Version,
    to: TxKind,
    input: &Bytes,
    access_list_accounts: u64,
    access_list_storage_keys: u64,
) -> u64 {
    let spec = version.spec_id();
    let params = version.gas_params();
    let non_zero_multiplier = if spec.enables(SpecId::ISTANBUL) { 16 } else { 68 };
    let mut gas = 21_000;
    for byte in input {
        gas += if *byte == 0 { 4 } else { non_zero_multiplier };
    }
    gas += access_list_accounts * u64::from(params.get(GasId::TxAccessListAddressCost));
    gas += access_list_storage_keys * u64::from(params.get(GasId::TxAccessListStorageKeyCost));
    if to.is_create() && spec.enables(SpecId::HOMESTEAD) {
        gas += 32_000;
    }
    if to.is_create() && spec.enables(SpecId::SHANGHAI) {
        gas += u64::from(params.get(GasId::TxInitcodeCost)) * num_words(input.len()) as u64;
    }
    gas
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BaseEvmTypes, Precompiles,
        env::{BlockEnv, TxEnv},
        evm::InMemoryDB,
        interpreter::{Host, InstrStop, op},
        registry::TxRegistry,
    };
    use alloc::vec;

    #[test]
    fn intrinsic_gas_charges_shanghai_create_initcode_words() {
        let input = Bytes::from(vec![1; 74]);

        assert_eq!(
            intrinsic_gas(&Version::base(SpecId::LONDON), TxKind::Create, &input, 0, 0),
            21_000 + 32_000 + 74 * 16
        );
        assert_eq!(
            intrinsic_gas(&Version::base(SpecId::SHANGHAI), TxKind::Create, &input, 0, 0),
            21_000 + 32_000 + 74 * 16 + 3 * 2
        );
    }

    #[test]
    fn intrinsic_gas_charges_access_list_items() {
        let input = Bytes::new();

        assert_eq!(
            intrinsic_gas(
                &Version::base(SpecId::BERLIN),
                TxKind::Call(Address::ZERO),
                &input,
                2,
                3
            ),
            21_000 + 2 * 2400 + 3 * 1900
        );
    }

    #[test]
    fn floor_gas_charges_prague_calldata_tokens() {
        let input = Bytes::from_static(&[0, 1, 2]);

        assert_eq!(floor_gas(&Version::base(SpecId::SHANGHAI), &input), 0);
        assert_eq!(floor_gas(&Version::base(SpecId::PRAGUE), &input), 21_000 + 9 * 10);
    }

    #[test]
    fn final_tx_gas_charges_calldata_floor_after_refund() {
        let result = MessageResult {
            stop: crate::interpreter::InstrStop::Return,
            gas_remaining: 50_000,
            gas_refunded: 10_000,
            ..MessageResult::default()
        };

        assert_eq!(final_tx_gas(&result, 100_000, true, 60_000), (40_000, 60_000));
    }

    #[test]
    fn final_tx_gas_preserves_higher_actual_usage() {
        let result = MessageResult {
            stop: crate::interpreter::InstrStop::Return,
            gas_remaining: 30_000,
            gas_refunded: 0,
            ..MessageResult::default()
        };

        assert_eq!(final_tx_gas(&result, 100_000, true, 60_000), (30_000, 70_000));
    }

    #[test]
    fn initial_delegated_call_uses_delegated_code_address() {
        let caller = Address::with_last_byte(0xaa);
        let target = Address::with_last_byte(0x02);
        let delegated = Address::with_last_byte(0x33);
        let delegated_code = Bytecode::new_legacy(Bytes::from_static(&[
            op::PUSH1,
            0x2a,
            op::PUSH0,
            op::MSTORE,
            op::PUSH1,
            0x20,
            op::PUSH0,
            op::RETURN,
        ]));
        let mut database = InMemoryDB::default();
        database.insert_account_info(
            target,
            AccountInfo::default().with_code(Bytecode::new_eip7702(delegated)),
        );
        database.insert_account_info(delegated, AccountInfo::default().with_code(delegated_code));
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::PRAGUE,
            BlockEnv::default(),
            TxRegistry::new(),
            database,
            Precompiles::base(SpecId::PRAGUE),
        );

        let (bytecode, message) = initial_message(
            &mut evm,
            caller,
            0,
            TxKind::Call(target),
            &Bytes::new(),
            U256::ZERO,
            100_000,
        );
        assert_eq!(message.destination, target);
        assert_eq!(message.code_address, delegated);
        assert!(message.disable_precompiles);

        let result = Host::execute_message(&mut evm, &TxEnv::default(), bytecode, &message, false);

        assert_eq!(result.stop, InstrStop::Return);
        assert_eq!(result.output.len(), 32);
        assert_eq!(result.output[31], 0x2a);
    }

    #[test]
    fn amsterdam_allows_total_gas_above_osaka_cap_when_regular_gas_fits() {
        let tx_gas_limit = EIP7825_TX_GAS_LIMIT_CAP + 1;
        let intrinsic = 21_000;
        let floor_gas = 21_000;

        assert_eq!(
            validate_tx_gas_limit_cap(SpecId::OSAKA, tx_gas_limit),
            Err(HandlerError::TxGasLimitGreaterThanCap {
                gas_limit: tx_gas_limit,
                cap: EIP7825_TX_GAS_LIMIT_CAP
            })
        );
        assert_eq!(validate_tx_gas_limit_cap(SpecId::AMSTERDAM, tx_gas_limit), Ok(()));
        assert_eq!(
            validate_regular_gas_limit_cap(SpecId::AMSTERDAM, tx_gas_limit, intrinsic, floor_gas),
            Ok(())
        );
        assert_eq!(
            validate_regular_gas_limit_cap(
                SpecId::AMSTERDAM,
                tx_gas_limit,
                EIP7825_TX_GAS_LIMIT_CAP + 1,
                floor_gas,
            ),
            Err(HandlerError::TxGasLimitGreaterThanCap {
                gas_limit: EIP7825_TX_GAS_LIMIT_CAP + 1,
                cap: EIP7825_TX_GAS_LIMIT_CAP
            })
        );
    }
}
