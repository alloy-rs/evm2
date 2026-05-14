//! Ethereum transaction envelope and handlers.

mod eip1559;
mod eip2930;
mod eip4844;
mod eip7702;
mod legacy;

use crate::{
    Evm, EvmFeatures, EvmTypes, SpecId, TxResult, Version,
    bytecode::Bytecode,
    evm::{AccountInfo, StateCheckpoint},
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

    /// Returns the transaction gas limit.
    pub const fn gas_limit(&self) -> u64 {
        match self {
            Self::Legacy(tx) => tx.inner().gas_limit,
            Self::Eip2930(tx) => tx.inner().gas_limit,
            Self::Eip1559(tx) => tx.inner().gas_limit,
            Self::Eip4844(tx) => tx.inner().tx().gas_limit,
            Self::Eip7702(tx) => tx.inner().gas_limit,
        }
    }

    /// Returns the transaction's block regular-gas allowance.
    pub fn block_regular_gas_limit(&self, version: &Version) -> u64 {
        let gas_limit = self.gas_limit();
        if !version.feature(EvmFeatures::EIP8037) {
            return gas_limit;
        }

        let create_state_gas = |to: TxKind| {
            if to.is_create() { u64::from(version.gas_params.get(GasId::CreateState)) } else { 0 }
        };
        let intrinsic_state = match self {
            Self::Legacy(tx) => create_state_gas(tx.inner().to),
            Self::Eip2930(tx) => create_state_gas(tx.inner().to),
            Self::Eip1559(tx) => create_state_gas(tx.inner().to),
            Self::Eip4844(tx) => create_state_gas(tx.inner().tx().to.into()),
            Self::Eip7702(tx) => {
                let auth_state = u64::from(version.gas_params.get(GasId::TxEip7702PerAuthState));
                u64::try_from(tx.inner().authorization_list.len())
                    .unwrap_or(u64::MAX)
                    .saturating_mul(auth_state)
            }
        };

        gas_limit.saturating_sub(intrinsic_state).min(version.tx_gas_limit_cap)
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

/// Returns the Ethereum transaction registry for `spec_id`.
pub fn ethereum_tx_registry<T: EvmTypes<Host = Evm<T>>>(
    spec_id: SpecId,
) -> TxRegistry<RecoveredTxEnvelope, TxResult<T>, Evm<T>> {
    let mut registry =
        TxRegistry::new().with_handler(0, RecoveredTxEnvelope::as_legacy, legacy::handle::<T>);

    if spec_id.enables(SpecId::BERLIN) {
        registry.register(1, RecoveredTxEnvelope::as_eip2930, eip2930::handle::<T>);
    }
    if spec_id.enables(SpecId::LONDON) {
        registry.register(2, RecoveredTxEnvelope::as_eip1559, eip1559::handle::<T>);
    }
    if spec_id.enables(SpecId::CANCUN) {
        registry.register(3, RecoveredTxEnvelope::as_eip4844, eip4844::handle::<T>);
    }
    if spec_id.enables(SpecId::PRAGUE) {
        registry.register(4, RecoveredTxEnvelope::as_eip7702, eip7702::handle::<T>);
    }

    registry
}

pub(super) fn validate_gas_price(
    version: &Version,
    gas_price: U256,
    basefee: U256,
) -> HandlerResult<()> {
    if version.feature(EvmFeatures::BASE_FEE_CHECK) && gas_price < basefee {
        return Err(HandlerError::FeeCapLessThanBaseFee {
            max_fee_per_gas: gas_price,
            base_fee: basefee,
        });
    }
    Ok(())
}

pub(super) fn validate_priority_fee(
    version: &Version,
    max_fee_per_gas: U256,
    max_priority_fee_per_gas: U256,
) -> HandlerResult<()> {
    if version.feature(EvmFeatures::PRIORITY_FEE_CHECK)
        && max_priority_fee_per_gas > max_fee_per_gas
    {
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
    version: &Version,
    tx_gas_limit: u64,
    block_gas_limit: U256,
) -> HandlerResult<()> {
    if version.feature(EvmFeatures::BLOCK_GAS_LIMIT_CHECK)
        && U256::from(tx_gas_limit) > block_gas_limit
    {
        return Err(HandlerError::GasLimitMoreThanBlock {
            gas_limit: tx_gas_limit,
            block_gas_limit,
        });
    }
    Ok(())
}

pub(super) const fn validate_tx_gas_limit_cap(
    version: &Version,
    tx_gas_limit: u64,
) -> HandlerResult<()> {
    // EIP-7825 caps each transaction gas limit to 2^24 in Osaka. Amsterdam/EIP-8037
    // replaces this with a regular-gas cap while allowing extra transaction gas to serve as
    // the state-gas reservoir.
    let cap = version.tx_gas_limit_cap;
    if !version.feature(EvmFeatures::EIP8037) && tx_gas_limit > cap {
        return Err(HandlerError::TxGasLimitGreaterThanCap { gas_limit: tx_gas_limit, cap });
    }
    Ok(())
}

pub(super) const fn validate_regular_gas_limit_cap(
    version: &Version,
    tx_gas_limit: u64,
    intrinsic: u64,
    floor_gas: u64,
) -> HandlerResult<()> {
    let cap = version.tx_gas_limit_cap;
    if version.feature(EvmFeatures::EIP8037) && tx_gas_limit > cap {
        let required_regular_gas = if intrinsic > floor_gas { intrinsic } else { floor_gas };
        if required_regular_gas > cap {
            return Err(HandlerError::TxGasLimitGreaterThanCap {
                gas_limit: required_regular_gas,
                cap,
            });
        }
    }
    Ok(())
}

pub(super) fn initial_execution_gas(
    version: &Version,
    tx_gas_limit: u64,
    intrinsic: u64,
    intrinsic_state: u64,
) -> (u64, u64) {
    let execution_gas = tx_gas_limit.saturating_sub(intrinsic);
    if !version.feature(EvmFeatures::EIP8037) {
        return (execution_gas, 0);
    }

    let intrinsic_regular = intrinsic.saturating_sub(intrinsic_state);
    let regular_budget = version.tx_gas_limit_cap.saturating_sub(intrinsic_regular);
    let regular_gas = execution_gas.min(regular_budget);
    let reservoir = execution_gas.saturating_sub(regular_gas);

    let _ = intrinsic_state;

    (regular_gas, reservoir)
}

pub(super) fn initial_execution_gas_with_state_refund(
    version: &Version,
    tx_gas_limit: u64,
    intrinsic_regular: u64,
    intrinsic_state: u64,
    state_refund: u64,
) -> (u64, u64) {
    if !version.feature(EvmFeatures::EIP8037) {
        return (tx_gas_limit.saturating_sub(intrinsic_regular), 0);
    }

    let intrinsic = intrinsic_regular.saturating_add(intrinsic_state).saturating_sub(state_refund);
    let execution_gas = tx_gas_limit.saturating_sub(intrinsic);
    let mut regular_gas =
        tx_gas_limit.min(version.tx_gas_limit_cap).saturating_sub(intrinsic_regular);
    let mut reservoir = execution_gas.saturating_sub(regular_gas);

    if reservoir >= intrinsic_state {
        reservoir -= intrinsic_state;
    } else {
        regular_gas = regular_gas.saturating_sub(intrinsic_state - reservoir);
        reservoir = 0;
    }
    reservoir = reservoir.saturating_add(state_refund);

    (regular_gas, reservoir)
}

pub(super) fn intrinsic_state_gas(version: &Version, to: TxKind) -> u64 {
    if to.is_create() && version.feature(EvmFeatures::EIP8037) {
        return u64::from(version.gas_params.get(GasId::CreateState));
    }
    0
}

pub(super) const fn validate_chain_id(
    version: &Version,
    chain_id: Option<u64>,
    allow_missing: bool,
) -> HandlerResult<()> {
    if !version.feature(EvmFeatures::TX_CHAIN_ID_CHECK) {
        return Ok(());
    }
    let Some(chain_id) = chain_id else {
        return if allow_missing { Ok(()) } else { Err(HandlerError::MissingChainId) };
    };
    if chain_id != version.chain_id {
        return Err(HandlerError::InvalidChainId { expected: version.chain_id, got: chain_id });
    }
    Ok(())
}

pub(super) fn validate_create_initcode(
    version: &Version,
    to: TxKind,
    input: &Bytes,
) -> HandlerResult<()> {
    if version.feature(EvmFeatures::EIP3860)
        && to.is_create()
        && input.len() > version.max_initcode_size
    {
        return Err(HandlerError::CreateInitCodeSizeLimit {
            limit: version.max_initcode_size,
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
    let sender_info = host
        .state
        .account_info(&caller)
        .map_err(|code| host.db_error_handler(code))?
        .unwrap_or_default();
    if host.feature(EvmFeatures::EIP3607) && sender_info.code_hash != KECCAK256_EMPTY {
        let code = host.state.get_code(&caller).map_err(|code| host.db_error_handler(code))?;
        if !code.is_empty() && !code.is_eip7702() {
            return Err(HandlerError::RejectCallerWithCode);
        }
    }
    if host.feature(EvmFeatures::NONCE_CHECK) && sender_info.nonce != nonce {
        return Err(HandlerError::InvalidNonce { expected: sender_info.nonce, got: nonce });
    }
    if host.feature(EvmFeatures::BALANCE_CHECK) && sender_info.balance < max_upfront {
        return Err(HandlerError::InsufficientFunds);
    }
    if !host.feature(EvmFeatures::BALANCE_CHECK) && sender_info.balance < max_upfront {
        host.state
            .add_balance(&caller, &(max_upfront - sender_info.balance))
            .map_err(|code| host.db_error_handler(code))?;
    }
    Ok(sender_info)
}

pub(super) fn warm_base_accounts<T: EvmTypes<Host = Evm<T>>>(
    host: &mut Evm<T>,
    caller: Address,
    to: TxKind,
) {
    host.state.warm_account_non_revertible(&caller);
    if host.feature(EvmFeatures::EIP3651) {
        host.state.warm_account_non_revertible(&host.block.beneficiary);
        let beneficiary = host.block.beneficiary;
        host.state.record_account_access(&beneficiary);
    }
    if let TxKind::Call(to) = to {
        host.state.warm_account_non_revertible(&to);
    }
    host.state.warm_accounts_non_revertible(host.precompiles().warm_addresses());
}

pub(super) fn warm_access_list<T: EvmTypes<Host = Evm<T>>>(
    host: &mut Evm<T>,
    access_list: &AccessList,
) {
    for item in access_list.iter() {
        host.state.warm_account_non_revertible(&item.address);
        for key in &item.storage_keys {
            let key = U256::from_be_bytes(key.0);
            let _ = host.state.warm_storage_non_revertible(&item.address, &key);
        }
    }
}

pub(super) fn charge_upfront<T: EvmTypes<Host = Evm<T>>>(
    host: &mut Evm<T>,
    caller: Address,
    max_gas_cost: U256,
) -> HandlerResult<()> {
    if !host.feature(EvmFeatures::FEE_CHARGE) {
        return Ok(());
    }
    host.state
        .add_balance(&caller, &Word::ZERO.wrapping_sub(max_gas_cost))
        .map_err(|code| host.db_error_handler(code))?;
    Ok(())
}

pub(crate) fn initial_message<T: EvmTypes<Host = Evm<T>>>(
    host: &mut Evm<T>,
    caller: Address,
    nonce: u64,
    to: TxKind,
    input: &Bytes,
    value: U256,
    gas_limit: u64,
) -> HandlerResult<(Bytecode, Message<T>)> {
    let r = match to {
        TxKind::Call(to) => {
            let initial_code = initial_call_code(host, to)?;
            let message = Message {
                kind: MessageKind::Call,
                depth: 0,
                gas_limit,
                gas_reservoir: 0,
                destination: to,
                caller,
                input: input.clone(),
                value,
                code_address: initial_code.code_address,
                disable_precompiles: initial_code.disable_precompiles,
                salt: B256::ZERO,
                ext: T::MessageExt::default(),
                _non_exhaustive: (),
            };
            (initial_code.code, message)
        }
        TxKind::Create => {
            let address = caller.create(nonce);
            let message = Message {
                kind: MessageKind::Create,
                depth: 0,
                gas_limit,
                gas_reservoir: 0,
                destination: address,
                caller,
                input: Bytes::new(),
                value,
                code_address: address,
                disable_precompiles: false,
                salt: B256::ZERO,
                ext: T::MessageExt::default(),
                _non_exhaustive: (),
            };
            (Bytecode::new_legacy(input.clone()), message)
        }
    };
    debug_assert_eq!(r.1.depth, 0);
    Ok(r)
}

struct InitialCallCode {
    code: Bytecode,
    code_address: Address,
    disable_precompiles: bool,
}

fn initial_call_code<T: EvmTypes<Host = Evm<T>>>(
    host: &mut Evm<T>,
    to: Address,
) -> HandlerResult<InitialCallCode> {
    let code = host.state.get_code(&to).map_err(|code| host.db_error_handler(code))?;
    if host.spec_id().enables(SpecId::PRAGUE)
        && let Some(delegated_address) = code.eip7702_address()
    {
        let _ = host.state.warm_account(&delegated_address);
        return Ok(InitialCallCode {
            code: host
                .state
                .get_code(&delegated_address)
                .map_err(|code| host.db_error_handler(code))?,
            code_address: delegated_address,
            disable_precompiles: true,
        });
    }
    Ok(InitialCallCode { code, code_address: to, disable_precompiles: false })
}

pub(super) fn rollback_failed_execution<T: EvmTypes<Host = Evm<T>>>(
    host: &mut Evm<T>,
    checkpoint: StateCheckpoint,
    result: &mut MessageResult<T>,
) {
    if !result.stop.is_success() {
        host.state.rollback(checkpoint, host.spec_id());
        if result.stop.is_halt() {
            result.gas.set_remaining(0);
        }
    }
}

#[expect(clippy::too_many_arguments)]
pub(super) fn settle_gas<T: EvmTypes<Host = Evm<T>>>(
    host: &mut Evm<T>,
    caller: Address,
    gas_price: U256,
    tx_gas_limit: u64,
    floor_gas: u64,
    _intrinsic_gas: u64,
    intrinsic_state_gas: u64,
    failure_intrinsic_state_refund: u64,
    top_level_refund: u64,
    result: MessageResult<T>,
) -> HandlerResult<TxResult<T>> {
    let gas = final_tx_gas(
        &result,
        tx_gas_limit,
        host.feature(EvmFeatures::EIP3529),
        floor_gas,
        _intrinsic_gas,
        intrinsic_state_gas,
        failure_intrinsic_state_refund,
        top_level_refund,
    );
    if host.feature(EvmFeatures::FEE_CHARGE) {
        host.state
            .add_balance(&caller, &(U256::from(gas.remaining) * gas_price))
            .map_err(|code| host.db_error_handler(code))?;
        let beneficiary_gas_price = if host.feature(EvmFeatures::BASE_FEE_CHECK) {
            gas_price.saturating_sub(host.block.basefee)
        } else {
            gas_price
        };
        host.state
            .add_balance(&host.block.beneficiary, &(U256::from(gas.used) * beneficiary_gas_price))
            .map_err(|code| host.db_error_handler(code))?;
    }
    Ok(TxResult {
        status: result.stop.is_success(),
        gas_used: gas.used,
        block_gas_used: gas.block_regular_used,
        state_gas_used: gas.state_used,
        stop: result.stop,
        output: result.output,
        ext: T::TxResultExt::default(),
        ..TxResult::default()
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct FinalTxGas {
    remaining: u64,
    used: u64,
    block_regular_used: u64,
    state_used: u64,
}

#[expect(clippy::too_many_arguments)]
const fn final_tx_gas<T: EvmTypes>(
    result: &MessageResult<T>,
    tx_gas_limit: u64,
    is_eip3529: bool,
    floor_gas: u64,
    _intrinsic_gas: u64,
    intrinsic_state_gas: u64,
    failure_intrinsic_state_refund: u64,
    top_level_refund: u64,
) -> FinalTxGas {
    let mut reservoir = result.gas.reservoir();
    let failure_intrinsic_state_refund =
        if !result.stop.is_success() { failure_intrinsic_state_refund } else { 0 };
    if !result.stop.is_success() {
        reservoir = reservoir
            .saturating_add_signed(result.gas.state_gas_spent())
            .saturating_add(failure_intrinsic_state_refund);
    }
    let spent = tx_gas_limit.saturating_sub(result.gas.remaining()).saturating_sub(reservoir);
    let execution_refund = if result.stop.is_success() && result.gas.refunded() > 0 {
        result.gas.refunded() as u64
    } else {
        0
    };
    let refund = execution_refund.saturating_add(top_level_refund);
    let refund = if refund > 0 {
        let max_refund_quotient = if is_eip3529 { 5 } else { 2 };
        let cap = spent / max_refund_quotient;
        if refund < cap { refund } else { cap }
    } else {
        0
    };
    let gas_remaining = result.gas.remaining().saturating_add(reservoir).saturating_add(refund);
    let gas_remaining = if gas_remaining < tx_gas_limit { gas_remaining } else { tx_gas_limit };
    let mut gas_used = tx_gas_limit.saturating_sub(gas_remaining);
    let state_used = if result.stop.is_success() {
        intrinsic_state_gas.saturating_add_signed(result.gas.state_gas_spent())
    } else {
        intrinsic_state_gas.saturating_sub(failure_intrinsic_state_refund)
    };
    let mut block_regular_used = spent.saturating_sub(state_used);
    // EIP-7623 charges at least the calldata floor after applying refunds.
    if gas_used < floor_gas {
        gas_used = floor_gas;
        block_regular_used = floor_gas;
        return FinalTxGas {
            remaining: tx_gas_limit.saturating_sub(floor_gas),
            used: gas_used,
            block_regular_used,
            state_used,
        };
    }
    FinalTxGas { remaining: gas_remaining, used: gas_used, block_regular_used, state_used }
}

pub(super) fn access_list_counts(access_list: &AccessList) -> (u64, u64) {
    (access_list.len() as u64, access_list.storage_keys_count() as u64)
}

const ACCESS_LIST_ADDRESS_FLOOR_TOKENS: u64 = 80;
const ACCESS_LIST_STORAGE_KEY_FLOOR_TOKENS: u64 = 128;

const fn access_list_floor_tokens(
    version: &Version,
    access_list_accounts: u64,
    access_list_storage_keys: u64,
) -> u64 {
    if !version.feature(EvmFeatures::EIP7981) {
        return 0;
    }
    access_list_accounts * ACCESS_LIST_ADDRESS_FLOOR_TOKENS
        + access_list_storage_keys * ACCESS_LIST_STORAGE_KEY_FLOOR_TOKENS
}

/// Calculates transaction calldata floor gas.
pub(super) fn floor_gas(
    version: &Version,
    input: &Bytes,
    access_list_accounts: u64,
    access_list_storage_keys: u64,
) -> u64 {
    if !version.feature(EvmFeatures::EIP7623) {
        return 0;
    }
    let params = &version.gas_params;
    let floor_cost_per_token = u64::from(params.get(GasId::TxFloorCostPerToken));
    if floor_cost_per_token == 0 {
        return 0;
    }

    let non_zero_multiplier = u64::from(params.get(GasId::TxTokenNonZeroByteMultiplier));
    let mut tokens =
        access_list_floor_tokens(version, access_list_accounts, access_list_storage_keys);
    if version.feature(EvmFeatures::EIP8037) {
        tokens += input.len() as u64 * u64::from(params.get(GasId::TxTokenCost));
    } else {
        for byte in input {
            tokens += if *byte == 0 { 1 } else { non_zero_multiplier };
        }
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
    let params = &version.gas_params;
    let non_zero_multiplier = if version.feature(EvmFeatures::EIP2028) { 16 } else { 68 };
    let mut gas = 21_000;
    for byte in input {
        gas += if *byte == 0 { 4 } else { non_zero_multiplier };
    }
    gas += access_list_accounts * u64::from(params.get(GasId::TxAccessListAddressCost));
    gas += access_list_storage_keys * u64::from(params.get(GasId::TxAccessListStorageKeyCost));
    gas += access_list_floor_tokens(version, access_list_accounts, access_list_storage_keys)
        * u64::from(params.get(GasId::TxFloorCostPerToken));
    if to.is_create() && version.feature(EvmFeatures::EIP2) {
        gas += u64::from(params.get(GasId::TxCreateCost));
    }
    if to.is_create() && version.feature(EvmFeatures::EIP3860) {
        gas += u64::from(params.get(GasId::TxInitcodeCost)) * num_words(input.len()) as u64;
    }
    if to.is_create() && version.feature(EvmFeatures::EIP8037) {
        gas += u64::from(params.get(GasId::CreateState));
    }
    gas
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BaseEvmTypes, ExecutionConfig, Precompiles,
        env::{BlockEnv, TxEnv},
        evm::InMemoryDB,
        interpreter::{GasTracker, Host, InstrStop, op},
        registry::TxRegistry,
    };
    use alloc::vec;
    use alloy_consensus::{TxEip2930, transaction::Recovered};
    use alloy_eips::eip2930::AccessList;

    #[test]
    fn intrinsic_gas_charges_shanghai_create_initcode_words() {
        let input = Bytes::from(vec![1; 74]);

        assert_eq!(
            intrinsic_gas(Version::base(SpecId::LONDON), TxKind::Create, &input, 0, 0),
            21_000 + 32_000 + 74 * 16
        );
        assert_eq!(
            intrinsic_gas(Version::base(SpecId::SHANGHAI), TxKind::Create, &input, 0, 0),
            21_000 + 32_000 + 74 * 16 + 3 * 2
        );
    }

    #[test]
    fn intrinsic_gas_charges_access_list_items() {
        let input = Bytes::new();

        assert_eq!(
            intrinsic_gas(Version::base(SpecId::BERLIN), TxKind::Call(Address::ZERO), &input, 2, 3),
            21_000 + 2 * 2400 + 3 * 1900
        );
        assert_eq!(
            intrinsic_gas(
                Version::base(SpecId::AMSTERDAM),
                TxKind::Call(Address::ZERO),
                &input,
                1,
                1
            ),
            21_000 + 2400 + 1900 + (80 + 128) * 16
        );
    }

    #[test]
    fn eip2930_rejects_gas_below_intrinsic() {
        let caller = Address::with_last_byte(0xaa);
        let mut database = InMemoryDB::default();
        database.insert_account_info(
            &caller,
            AccountInfo::default().with_balance(U256::from(1_000_000_000u64)),
        );
        let tx = RecoveredTxEnvelope::Eip2930(Recovered::new_unchecked(
            TxEip2930 {
                chain_id: 1,
                nonce: 0,
                gas_price: 1,
                gas_limit: 20_999,
                to: TxKind::Call(Address::with_last_byte(0xbb)),
                value: U256::ZERO,
                input: Bytes::new(),
                access_list: AccessList::default(),
            },
            caller,
        ));
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::BERLIN,
            BlockEnv::default(),
            ethereum_tx_registry(SpecId::BERLIN),
            database,
            Precompiles::base(SpecId::BERLIN),
        );

        assert_eq!(
            evm.transact(&tx),
            Err(HandlerError::IntrinsicGasTooLow { required: 21_000, got: 20_999 })
        );
    }

    #[test]
    fn floor_gas_charges_prague_calldata_tokens() {
        let input = Bytes::from_static(&[0, 1, 2]);
        let mut prague_without_eip7623 = Version::new(SpecId::PRAGUE);
        prague_without_eip7623.features.remove(EvmFeatures::EIP7623);

        assert_eq!(floor_gas(Version::base(SpecId::SHANGHAI), &input, 0, 0), 0);
        assert_eq!(floor_gas(Version::base(SpecId::PRAGUE), &input, 0, 0), 21_000 + 9 * 10);
        assert_eq!(floor_gas(&prague_without_eip7623, &input, 0, 0), 0);
    }

    #[test]
    fn floor_gas_charges_amsterdam_access_list_tokens() {
        let input = Bytes::from(vec![1; 1000]);

        assert_eq!(
            floor_gas(Version::base(SpecId::AMSTERDAM), &input, 1, 1),
            21_000 + (1000 * 4 + 80 + 128) * 16
        );
    }

    #[test]
    fn features_gate_transaction_validation() {
        let mut london = Version::new(SpecId::LONDON);
        assert_eq!(
            validate_gas_price(&london, U256::ZERO, U256::ONE),
            Err(HandlerError::FeeCapLessThanBaseFee {
                max_fee_per_gas: U256::ZERO,
                base_fee: U256::ONE,
            })
        );
        london.features.remove(EvmFeatures::BASE_FEE_CHECK);
        assert_eq!(validate_gas_price(&london, U256::ZERO, U256::ONE), Ok(()));

        let mut prague = Version::new(SpecId::PRAGUE);
        assert_eq!(
            validate_priority_fee(&prague, U256::ONE, U256::from(2)),
            Err(HandlerError::PriorityFeeGreaterThanMaxFee)
        );
        prague.features.remove(EvmFeatures::PRIORITY_FEE_CHECK);
        assert_eq!(validate_priority_fee(&prague, U256::ONE, U256::from(2)), Ok(()));

        assert_eq!(
            validate_block_gas_limit(&prague, 2, U256::ONE),
            Err(HandlerError::GasLimitMoreThanBlock { gas_limit: 2, block_gas_limit: U256::ONE })
        );
        prague.features.remove(EvmFeatures::BLOCK_GAS_LIMIT_CHECK);
        assert_eq!(validate_block_gas_limit(&prague, 2, U256::ONE), Ok(()));

        let mut version = Version::new(SpecId::OSAKA);
        version.chain_id = 10;
        assert_eq!(validate_chain_id(&version, Some(10), false), Ok(()));
        assert_eq!(
            validate_chain_id(&version, Some(1), false),
            Err(HandlerError::InvalidChainId { expected: 10, got: 1 })
        );
        assert_eq!(validate_chain_id(&version, None, false), Err(HandlerError::MissingChainId));
        assert_eq!(validate_chain_id(&version, None, true), Ok(()));
        version.features.remove(EvmFeatures::TX_CHAIN_ID_CHECK);
        assert_eq!(validate_chain_id(&version, Some(1), false), Ok(()));
    }

    #[test]
    fn features_gate_sender_validation() {
        let caller = Address::with_last_byte(0xaa);
        let mut database = InMemoryDB::default();
        database.insert_account_info(
            &caller,
            AccountInfo::default()
                .with_nonce(7)
                .with_code(Bytecode::new_legacy(Bytes::from_static(&[op::STOP]))),
        );

        let mut version = Version::new(SpecId::OSAKA);
        version.features.remove(EvmFeatures::EIP3607);
        version.features.remove(EvmFeatures::NONCE_CHECK);
        version.features.remove(EvmFeatures::BALANCE_CHECK);
        let mut evm = Evm::<BaseEvmTypes>::new_with_execution_config(
            ExecutionConfig::for_spec_and_version(SpecId::OSAKA, version),
            SpecId::OSAKA,
            BlockEnv::default(),
            TxRegistry::new(),
            database,
            Precompiles::base(SpecId::OSAKA),
        );

        assert!(validate_sender(&mut evm, caller, 0, U256::from(100)).is_ok());
        assert_eq!(evm.state.account_info(&caller).unwrap().unwrap().balance, U256::from(100));
    }

    #[test]
    fn final_tx_gas_charges_calldata_floor_after_refund() {
        let result = MessageResult::<BaseEvmTypes> {
            stop: crate::interpreter::InstrStop::Return,
            gas: {
                let mut gas = GasTracker::new_used_gas(100_000, 50_000, 0);
                gas.set_refunded(10_000);
                gas
            },
            ..MessageResult::<BaseEvmTypes>::default()
        };

        assert_eq!(
            final_tx_gas(&result, 100_000, true, 60_000, 0, 0, 0, 0),
            FinalTxGas {
                remaining: 40_000,
                used: 60_000,
                block_regular_used: 60_000,
                state_used: 0,
            }
        );
    }

    #[test]
    fn final_tx_gas_applies_top_level_refund_on_revert() {
        let result = MessageResult::<BaseEvmTypes> {
            stop: crate::interpreter::InstrStop::Revert,
            gas: {
                let mut gas = GasTracker::new_used_gas(100_000, 50_000, 0);
                gas.set_refunded(40_000);
                gas
            },
            ..MessageResult::<BaseEvmTypes>::default()
        };

        assert_eq!(
            final_tx_gas(&result, 100_000, true, 21_000, 0, 0, 0, 10_000),
            FinalTxGas {
                remaining: 60_000,
                used: 40_000,
                block_regular_used: 50_000,
                state_used: 0,
            }
        );
    }

    #[test]
    fn final_tx_gas_refunds_failed_create_state_gas_on_halt() {
        let result = MessageResult::<BaseEvmTypes> {
            stop: crate::interpreter::InstrStop::OutOfGas,
            gas: GasTracker::new_used_gas(100_000, 100_000, 0),
            ..MessageResult::<BaseEvmTypes>::default()
        };

        assert_eq!(
            final_tx_gas(&result, 100_000, true, 21_000, 0, 5_000, 5_000, 0),
            FinalTxGas {
                remaining: 5_000,
                used: 95_000,
                block_regular_used: 95_000,
                state_used: 0,
            }
        );
    }

    #[test]
    fn final_tx_gas_applies_top_level_refund_on_halt() {
        let result = MessageResult::<BaseEvmTypes> {
            stop: crate::interpreter::InstrStop::OutOfGas,
            gas: GasTracker::new_used_gas(100_000, 100_000, 0),
            ..MessageResult::<BaseEvmTypes>::default()
        };

        assert_eq!(
            final_tx_gas(&result, 100_000, true, 21_000, 0, 0, 0, 10_000),
            FinalTxGas {
                remaining: 10_000,
                used: 90_000,
                block_regular_used: 100_000,
                state_used: 0,
            }
        );
    }

    #[test]
    fn final_tx_gas_preserves_higher_actual_usage() {
        let result = MessageResult::<BaseEvmTypes> {
            stop: crate::interpreter::InstrStop::Return,
            gas: GasTracker::new_used_gas(100_000, 70_000, 0),
            ..MessageResult::<BaseEvmTypes>::default()
        };

        assert_eq!(
            final_tx_gas(&result, 100_000, true, 60_000, 0, 0, 0, 0),
            FinalTxGas {
                remaining: 30_000,
                used: 70_000,
                block_regular_used: 70_000,
                state_used: 0,
            }
        );
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
            &target,
            AccountInfo::default().with_code(Bytecode::new_eip7702(delegated)),
        );
        database.insert_account_info(&delegated, AccountInfo::default().with_code(delegated_code));
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::PRAGUE,
            BlockEnv::default(),
            TxRegistry::new(),
            database,
            Precompiles::base(SpecId::PRAGUE),
        );

        let (bytecode, mut message) = initial_message(
            &mut evm,
            caller,
            0,
            TxKind::Call(target),
            &Bytes::new(),
            U256::ZERO,
            100_000,
        )
        .unwrap();
        assert_eq!(message.destination, target);
        assert_eq!(message.code_address, delegated);
        assert!(message.disable_precompiles);

        let result =
            Host::execute_message(&mut evm, &TxEnv::default(), bytecode, &mut message, false);

        assert_eq!(result.stop, InstrStop::Return);
        assert_eq!(result.output.len(), 32);
        assert_eq!(result.output[31], 0x2a);
    }

    #[test]
    fn amsterdam_allows_total_gas_above_osaka_cap_when_regular_gas_fits() {
        let osaka = Version::base(SpecId::OSAKA);
        let amsterdam = Version::base(SpecId::AMSTERDAM);
        let tx_gas_limit = osaka.tx_gas_limit_cap + 1;
        let intrinsic = 21_000;
        let floor_gas = 21_000;

        assert_eq!(
            validate_tx_gas_limit_cap(osaka, tx_gas_limit),
            Err(HandlerError::TxGasLimitGreaterThanCap {
                gas_limit: tx_gas_limit,
                cap: osaka.tx_gas_limit_cap
            })
        );
        assert_eq!(validate_tx_gas_limit_cap(amsterdam, tx_gas_limit), Ok(()));
        assert_eq!(
            validate_regular_gas_limit_cap(amsterdam, tx_gas_limit, intrinsic, floor_gas),
            Ok(())
        );
        assert_eq!(
            validate_regular_gas_limit_cap(
                amsterdam,
                tx_gas_limit,
                amsterdam.tx_gas_limit_cap + 1,
                floor_gas,
            ),
            Err(HandlerError::TxGasLimitGreaterThanCap {
                gas_limit: amsterdam.tx_gas_limit_cap + 1,
                cap: amsterdam.tx_gas_limit_cap
            })
        );
        assert_eq!(
            validate_regular_gas_limit_cap(
                amsterdam,
                tx_gas_limit,
                amsterdam.tx_gas_limit_cap,
                floor_gas,
            ),
            Ok(())
        );

        let mut amsterdam_without_eip8037 = Version::new(SpecId::AMSTERDAM);
        amsterdam_without_eip8037.features.remove(EvmFeatures::EIP8037);
        assert_eq!(
            validate_tx_gas_limit_cap(&amsterdam_without_eip8037, tx_gas_limit),
            Err(HandlerError::TxGasLimitGreaterThanCap {
                gas_limit: tx_gas_limit,
                cap: amsterdam_without_eip8037.tx_gas_limit_cap,
            })
        );
    }
}
