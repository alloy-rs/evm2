//! Ethereum transaction envelope and handlers.

/// EIP-1559 transaction handler.
pub mod eip1559;
/// EIP-2930 transaction handler.
pub mod eip2930;
/// EIP-4844 transaction handler.
pub mod eip4844;
/// EIP-7702 transaction handler.
pub mod eip7702;
mod lazy_eip7702;
/// Legacy transaction handler.
pub mod legacy;

pub use lazy_eip7702::{LazyAuthorization, LazyTxEip7702};

use crate::{
    Evm, EvmFeatures, EvmTypes, EvmTypesHost, SpecId, TxResult, Version,
    bytecode::Bytecode,
    evm::{AccountInfo, StateCheckpoint, error_handler},
    interpreter::{
        Message, MessageKind, MessageResult, Word,
        gas::{EIP2780_TX_BASE_COST, EIP8038_COLD_ACCOUNT_ACCESS},
    },
    registry::{HandlerError, HandlerResult, TxRegistry},
    utils::num_words,
    version::GasId,
};
use alloy_consensus::{
    EthereumTxEnvelope, TxEip1559, TxEip2930, TxEip4844, TxEip7702, TxLegacy,
    transaction::{Recovered, Transaction, TxEip4844Variant},
};
use alloy_eips::{eip2718::Typed2718, eip2930::AccessList};
use alloy_primitives::{Address, B256, Bytes, KECCAK256_EMPTY, TxKind, U256};

/// Ethereum transaction envelope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TxEnvelope {
    /// Legacy transaction.
    Legacy(TxLegacy),
    /// EIP-2930 access-list transaction.
    Eip2930(TxEip2930),
    /// EIP-1559 dynamic-fee transaction.
    Eip1559(TxEip1559),
    /// EIP-4844 blob transaction.
    Eip4844(TxEip4844Variant),
    /// EIP-7702 set-code transaction.
    Eip7702(LazyTxEip7702),
}

/// Recovered Ethereum transaction envelope.
pub type RecoveredTxEnvelope = Recovered<TxEnvelope>;

impl From<EthereumTxEnvelope<TxEip4844>> for TxEnvelope {
    fn from(tx: EthereumTxEnvelope<TxEip4844>) -> Self {
        match tx {
            EthereumTxEnvelope::Legacy(tx) => Self::Legacy(tx.strip_signature()),
            EthereumTxEnvelope::Eip2930(tx) => Self::Eip2930(tx.strip_signature()),
            EthereumTxEnvelope::Eip1559(tx) => Self::Eip1559(tx.strip_signature()),
            EthereumTxEnvelope::Eip4844(tx) => Self::Eip4844(tx.strip_signature().into()),
            EthereumTxEnvelope::Eip7702(tx) => {
                Self::Eip7702(LazyTxEip7702::from_recovered_authorizations(tx.strip_signature()))
            }
        }
    }
}

impl TxEnvelope {
    /// Returns the contained legacy transaction, if this is legacy.
    pub const fn as_legacy(&self) -> Option<&TxLegacy> {
        match self {
            Self::Legacy(tx) => Some(tx),
            Self::Eip2930(_) | Self::Eip1559(_) | Self::Eip4844(_) | Self::Eip7702(_) => None,
        }
    }

    /// Returns the contained EIP-2930 transaction, if this is EIP-2930.
    pub const fn as_eip2930(&self) -> Option<&TxEip2930> {
        match self {
            Self::Eip2930(tx) => Some(tx),
            Self::Legacy(_) | Self::Eip1559(_) | Self::Eip4844(_) | Self::Eip7702(_) => None,
        }
    }

    /// Returns the contained EIP-1559 transaction, if this is EIP-1559.
    pub const fn as_eip1559(&self) -> Option<&TxEip1559> {
        match self {
            Self::Eip1559(tx) => Some(tx),
            Self::Legacy(_) | Self::Eip2930(_) | Self::Eip4844(_) | Self::Eip7702(_) => None,
        }
    }

    /// Returns the contained EIP-4844 transaction, if this is EIP-4844.
    pub const fn as_eip4844(&self) -> Option<&TxEip4844Variant> {
        match self {
            Self::Eip4844(tx) => Some(tx),
            Self::Legacy(_) | Self::Eip2930(_) | Self::Eip1559(_) | Self::Eip7702(_) => None,
        }
    }

    /// Returns the contained EIP-7702 transaction, if this is EIP-7702.
    pub const fn as_eip7702(&self) -> Option<&LazyTxEip7702> {
        match self {
            Self::Eip7702(tx) => Some(tx),
            Self::Legacy(_) | Self::Eip2930(_) | Self::Eip1559(_) | Self::Eip4844(_) => None,
        }
    }
}

impl From<TxEip7702> for TxEnvelope {
    fn from(tx: TxEip7702) -> Self {
        Self::Eip7702(tx.into())
    }
}

impl From<LazyTxEip7702> for TxEnvelope {
    fn from(tx: LazyTxEip7702) -> Self {
        Self::Eip7702(tx)
    }
}

macro_rules! delegate {
    ($self:expr, $method:ident $(, $arg:expr)*) => {
        match $self {
            Self::Legacy(tx) => tx.$method($($arg),*),
            Self::Eip2930(tx) => tx.$method($($arg),*),
            Self::Eip1559(tx) => tx.$method($($arg),*),
            Self::Eip4844(tx) => tx.$method($($arg),*),
            Self::Eip7702(tx) => tx.$method($($arg),*),
        }
    };
}

impl Typed2718 for TxEnvelope {
    fn ty(&self) -> u8 {
        delegate!(self, ty)
    }
}

impl Transaction for TxEnvelope {
    fn chain_id(&self) -> Option<u64> {
        delegate!(self, chain_id)
    }

    fn nonce(&self) -> u64 {
        delegate!(self, nonce)
    }

    fn gas_limit(&self) -> u64 {
        delegate!(self, gas_limit)
    }

    fn gas_price(&self) -> Option<u128> {
        delegate!(self, gas_price)
    }

    fn max_fee_per_gas(&self) -> u128 {
        delegate!(self, max_fee_per_gas)
    }

    fn max_priority_fee_per_gas(&self) -> Option<u128> {
        delegate!(self, max_priority_fee_per_gas)
    }

    fn max_fee_per_blob_gas(&self) -> Option<u128> {
        delegate!(self, max_fee_per_blob_gas)
    }

    fn priority_fee_or_price(&self) -> u128 {
        delegate!(self, priority_fee_or_price)
    }

    fn effective_gas_price(&self, base_fee: Option<u64>) -> u128 {
        delegate!(self, effective_gas_price, base_fee)
    }

    fn is_dynamic_fee(&self) -> bool {
        delegate!(self, is_dynamic_fee)
    }

    fn kind(&self) -> TxKind {
        delegate!(self, kind)
    }

    fn is_create(&self) -> bool {
        delegate!(self, is_create)
    }

    fn value(&self) -> U256 {
        delegate!(self, value)
    }

    fn input(&self) -> &Bytes {
        delegate!(self, input)
    }

    fn access_list(&self) -> Option<&AccessList> {
        delegate!(self, access_list)
    }

    fn blob_versioned_hashes(&self) -> Option<&[B256]> {
        delegate!(self, blob_versioned_hashes)
    }

    fn authorization_list(&self) -> Option<&[alloy_eips::eip7702::SignedAuthorization]> {
        delegate!(self, authorization_list)
    }
}

/// Returns the Ethereum transaction registry for `spec_id`.
pub fn ethereum_tx_registry<T: EvmTypes<Tx = TxEnvelope>>(
    spec_id: SpecId,
) -> TxRegistry<T, TxResult<T>> {
    let mut registry =
        TxRegistry::new().with_handler(0, TxEnvelope::as_legacy, legacy::handle::<T>);

    if spec_id.enables(SpecId::BERLIN) {
        registry.register(1, TxEnvelope::as_eip2930, eip2930::handle::<T>);
    }
    if spec_id.enables(SpecId::LONDON) {
        registry.register(2, TxEnvelope::as_eip1559, eip1559::handle::<T>);
    }
    if spec_id.enables(SpecId::CANCUN) {
        registry.register(3, TxEnvelope::as_eip4844, eip4844::handle::<T>);
    }
    if spec_id.enables(SpecId::PRAGUE) {
        registry.register(4, TxEnvelope::as_eip7702, eip7702::handle::<T>);
    }

    registry
}

/// Validates the effective gas price against the block base fee.
pub fn validate_gas_price(version: &Version, gas_price: U256, basefee: U256) -> HandlerResult<()> {
    if version.feature(EvmFeatures::BASE_FEE_CHECK) && gas_price < basefee {
        return Err(HandlerError::FeeCapLessThanBaseFee {
            max_fee_per_gas: gas_price,
            base_fee: basefee,
        });
    }
    Ok(())
}

/// Validates that the priority fee does not exceed the maximum fee.
pub fn validate_priority_fee(
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

/// Calculates the effective gas price for an EIP-1559 transaction.
pub fn effective_gas_price(
    max_fee_per_gas: U256,
    max_priority_fee_per_gas: U256,
    basefee: U256,
) -> U256 {
    max_fee_per_gas.min(basefee.saturating_add(max_priority_fee_per_gas))
}

/// Validates the transaction gas limit against the block gas limit.
pub fn validate_block_gas_limit(
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

/// Validates the transaction gas limit against the active transaction cap.
pub const fn validate_tx_gas_limit_cap(version: &Version, tx_gas_limit: u64) -> HandlerResult<()> {
    // EIP-7825 caps each transaction gas limit to 2^24 in Osaka. Amsterdam/EIP-8037
    // replaces this with a regular-gas cap while allowing extra transaction gas to serve as
    // the state-gas reservoir.
    let cap = version.tx_gas_limit_cap;
    if !version.feature(EvmFeatures::EIP8037) && tx_gas_limit > cap {
        return Err(HandlerError::TxGasLimitGreaterThanCap { gas_limit: tx_gas_limit, cap });
    }
    Ok(())
}

/// Validates the regular-gas portion against the active transaction cap.
pub const fn validate_regular_gas_limit_cap(
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

/// Validates a transaction chain ID against the active chain.
pub const fn validate_chain_id(
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

/// Validates top-level create initcode against the active size limit.
pub fn validate_create_initcode(version: &Version, to: TxKind, input: &Bytes) -> HandlerResult<()> {
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

/// Rejects a nonce that cannot be incremented.
pub const fn validate_nonce_not_overflow(nonce: u64) -> HandlerResult<()> {
    if nonce == u64::MAX {
        return Err(HandlerError::NonceOverflow);
    }
    Ok(())
}

/// Validates that the gas limit covers regular and state intrinsic gas.
pub const fn validate_intrinsic_gas(
    gas_limit: u64,
    intrinsic: u64,
    initial_state_gas: u64,
) -> HandlerResult<()> {
    // EIP-8037: the gas limit must cover the regular intrinsic gas plus the upfront state gas.
    let required = intrinsic.saturating_add(initial_state_gas);
    if gas_limit < required {
        return Err(HandlerError::IntrinsicGasTooLow { required, got: gas_limit });
    }
    Ok(())
}

/// Validates that the gas limit covers the calldata floor gas.
pub const fn validate_floor_gas(gas_limit: u64, floor_gas: u64) -> HandlerResult<()> {
    if gas_limit < floor_gas {
        return Err(HandlerError::IntrinsicGasTooLow { required: floor_gas, got: gas_limit });
    }
    Ok(())
}

/// Loads and validates the sender account.
pub fn validate_sender<'a, T: EvmTypes>(
    host: &mut Evm<'a, T>,
    caller: Address,
    nonce: u64,
    max_upfront: U256,
) -> HandlerResult<AccountInfo> {
    let has_nonce_check = host.feature(EvmFeatures::NONCE_CHECK);
    let has_balance_check = host.feature(EvmFeatures::BALANCE_CHECK);
    let has_balance_top_up = host.feature(EvmFeatures::BALANCE_TOP_UP);
    let has_eip3607 = host.feature(EvmFeatures::EIP3607);

    let mut sender = host.state.account(&caller, false).map_err(error_handler!(host))?;
    if has_eip3607 && sender.code_hash() != KECCAK256_EMPTY {
        let code = sender.load_code().map_err(error_handler!(host))?;
        if !code.is_empty() && !code.is_eip7702() {
            return Err(HandlerError::RejectCallerWithCode);
        }
    }
    if has_nonce_check && sender.nonce() != nonce {
        return Err(HandlerError::InvalidNonce { expected: sender.nonce(), got: nonce });
    }
    if has_balance_check && sender.balance() < max_upfront {
        return Err(HandlerError::InsufficientFunds);
    }
    if !has_balance_check && has_balance_top_up && sender.balance() < max_upfront {
        sender.add_balance(max_upfront - sender.balance());
    }
    Ok(sender.get().cloned().unwrap_or_default())
}

/// Warms the accounts required by every transaction.
pub fn warm_base_accounts<'a, T: EvmTypes>(host: &mut Evm<'a, T>, caller: Address, to: TxKind) {
    host.state.prewarm(&caller);
    if host.feature(EvmFeatures::EIP3651) {
        host.state.prewarm(&host.block.beneficiary);
    }
    if let TxKind::Call(to) = to {
        host.state.prewarm(&to);
    }
    host.warm_precompiles();
}

/// Warms every account and storage key in an access list.
pub fn warm_access_list<'a, T: EvmTypes>(host: &mut Evm<'a, T>, access_list: &AccessList) {
    for item in access_list.iter() {
        host.state.prewarm_storage(
            &item.address,
            item.storage_keys.iter().map(|key| U256::from_be_bytes(key.0)),
        );
    }
}

/// Deducts a transaction's upfront native-token gas charge.
pub fn charge_upfront<'a, T: EvmTypes>(
    host: &mut Evm<'a, T>,
    caller: Address,
    max_gas_cost: U256,
) -> HandlerResult<()> {
    if !host.feature(EvmFeatures::FEE_CHARGE) {
        return Ok(());
    }
    host.state
        .account(&caller, false)
        .map_err(error_handler!(host))?
        .add_balance(Word::ZERO.wrapping_sub(max_gas_cost));
    Ok(())
}

/// Returns the EIP-8037 `initial_state_gas` charged before execution for `is_create` transactions
/// (the top-level create's `create_state_gas`). Zero without EIP-8037 or for non-create calls.
///
/// This is the create transaction's contribution to execution-specs `IntrinsicGas.state`; the
/// EIP-7702 authorization contribution is computed separately in the EIP-7702 handler. Keeping the
/// state-gas intrinsic distinct from the regular intrinsic ([`intrinsic_gas`]) mirrors the spec,
/// which tracks `IntrinsicGas.regular` and `.state` separately.
pub const fn create_initial_state_gas(version: &Version, is_create: bool) -> u64 {
    if version.feature(EvmFeatures::EIP8037) && is_create {
        version.gas_params.create_state_gas()
    } else {
        0
    }
}

/// Returns `(regular_gas_limit, reservoir)` for the first frame.
///
/// `initial_state_gas` is the EIP-8037 state gas charged before execution (top-level create state
/// gas and EIP-7702 authorization state gas). It is deducted from the reservoir, spilling into the
/// regular budget when the reservoir is insufficient. `state_refund` is the EIP-7702 state-gas
/// refund, credited directly back to the reservoir so it stays state gas. Both are zero without
/// EIP-8037.
///
/// `initial_state_gas` and `state_refund` are kept as separate arguments deliberately: per
/// execution-specs the state refund is added to the state-gas reservoir (`set_delegation` does
/// `state_gas_reservoir += refund`), not applied to regular gas first. Folding them into a single
/// regular-first refund — as an earlier note suggested — would diverge from the spec.
pub fn initial_gas_and_reservoir(
    version: &Version,
    tx_gas_limit: u64,
    intrinsic: u64,
    initial_state_gas: u64,
    state_refund: u64,
) -> (u64, u64) {
    if !version.feature(EvmFeatures::EIP8037) {
        return (tx_gas_limit - intrinsic, 0);
    }

    let cap = version.tx_gas_limit_cap;
    let execution_gas = tx_gas_limit - intrinsic;
    let mut regular_gas_limit = core::cmp::min(tx_gas_limit, cap).saturating_sub(intrinsic);
    let mut reservoir = execution_gas - regular_gas_limit;

    if reservoir >= initial_state_gas {
        reservoir -= initial_state_gas;
    } else {
        regular_gas_limit -= initial_state_gas - reservoir;
        reservoir = 0;
    }

    // EIP-7702 state-gas refund for existing authorities goes directly to the reservoir so it
    // stays state gas rather than being routed through the capped regular refund counter.
    reservoir += state_refund;

    (regular_gas_limit, reservoir)
}

#[allow(clippy::too_many_arguments)]
/// Creates the bytecode and top-level message for a transaction call or create.
pub fn initial_message<'a, T: EvmTypes>(
    host: &mut Evm<'a, T>,
    caller: Address,
    nonce: u64,
    to: TxKind,
    input: &Bytes,
    value: U256,
    gas_limit: u64,
    reservoir: u64,
    first_frame_state_gas: u64,
) -> HandlerResult<(Bytecode, Message<T>)> {
    let r = match to {
        TxKind::Call(to) => {
            let initial_code = initial_call_code(host, to)?;
            let message = Message {
                kind: MessageKind::Call,
                depth: 0,
                gas_limit,
                reservoir,
                first_frame_state_gas: 0,
                destination: to,
                caller,
                input: input.clone(),
                value,
                code_address: to,
                caller_is_static: false,
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
                reservoir,
                first_frame_state_gas,
                destination: address,
                caller,
                input: input.clone(),
                value,
                code_address: address,
                caller_is_static: false,
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
}

fn initial_call_code<'a, T: EvmTypes>(
    host: &mut Evm<'a, T>,
    to: Address,
) -> HandlerResult<InitialCallCode> {
    let code = host
        .state
        .account(&to, false)
        .map_err(error_handler!(host))?
        .load_code()
        .map_err(error_handler!(host))?;
    if host.feature(EvmFeatures::EIP7702)
        && let Some(delegated_address) = code.eip7702_address()
    {
        let mut account =
            host.state.account(&delegated_address, false).map_err(error_handler!(host))?;
        account.warm();
        let delegated_code = account.load_code().map_err(error_handler!(host))?;
        return Ok(InitialCallCode { code: delegated_code });
    }
    Ok(InitialCallCode { code })
}

/// Rolls back failed top-level execution and normalizes halt gas.
pub fn rollback_failed_execution<'a, T: EvmTypes>(
    host: &mut Evm<'a, T>,
    checkpoint: StateCheckpoint,
    result: &mut MessageResult<T>,
) {
    if !result.stop.is_success() {
        let features = host.version().features;
        host.state.rollback(checkpoint, features);
        if result.stop.is_halt() {
            result.gas.set_remaining(0);
        }
    }
}

/// EIP-8037: refunds a top-level CREATE's intrinsic `create_state_gas` back to the reservoir when
/// no new account leaf ends up created.
///
/// The charge was deducted upfront in [`initial_gas_and_reservoir`] (an unbalanced reservoir
/// reduction, not a `spend_state`), so the inverse is an unbalanced reservoir add. It is refunded
/// when the deployment failed (a reverted or halted deployment is rolled back, so the state gas was
/// never actually consumed) or when it succeeded at a pre-existing alive (balance-only) target (no
/// new leaf was created — execution-specs `created_target_alive`). No-op when `create_state_gas` is
/// zero (non-create or pre-Amsterdam).
pub const fn refund_create_state_gas<T: EvmTypesHost>(
    result: &mut MessageResult<T>,
    create_state_gas: u64,
) {
    if create_state_gas != 0 && (!result.stop.is_success() || result.created_target_was_alive) {
        let reservoir = result.gas.reservoir().saturating_add(create_state_gas);
        result.gas.set_reservoir(reservoir);
    }
}

#[allow(clippy::too_many_arguments)]
/// Applies Ethereum gas refunds, sender reimbursement, and beneficiary rewards.
pub fn settle_gas<'a, T: EvmTypes>(
    host: &mut Evm<'a, T>,
    caller: Address,
    gas_price: U256,
    tx_gas_limit: u64,
    floor_gas: u64,
    initial_state_gas: u64,
    state_refund: u64,
    is_create: bool,
    result: MessageResult<T>,
) -> HandlerResult<TxResult<T>> {
    if let Some(code) = host.error_code {
        return Err(HandlerError::Fatal(code));
    }

    let max_refund_quotient = u64::from(host.version().gas_params.get(GasId::MaxRefundQuotient));
    let (gas_remaining, gas_used) =
        final_tx_gas(&result, tx_gas_limit, max_refund_quotient, floor_gas);
    // Self-contained gas breakdown for the result. `total_gas_spent` is defined so that
    // `TxResult::tx_gas_used` reproduces the local `gas_used` (used here for the beneficiary
    // reward). State gas is execution state gas plus the upfront `initial_state_gas`, less the
    // EIP-7702 per-authorization `state_refund`.
    let total_gas_spent =
        tx_gas_limit.saturating_sub(result.gas.remaining()).saturating_sub(result.gas.reservoir());
    let refunded = result.final_refund(tx_gas_limit, max_refund_quotient);
    // EIP-7623: when the calldata floor exceeds spent-minus-refund, `TxResult::tx_gas_used`
    // resolves to the floor. `total_gas_spent` stays pre-refund and pre-floor: block-level
    // regular gas (EIP-7778/EIP-8037) accumulates `tx_gas_used_before_refund` per
    // execution-specs, without the floor clamp.
    // Execution state gas contributes only on success: a revert/halt rolls back its state changes.
    // A failed top-level CREATE additionally unwinds its intrinsic `create_state_gas` (refunded to
    // the reservoir by `refund_create_state_gas`), so it nets out of the block state gas.
    let exec_state_gas = if result.stop.is_success() {
        result.gas.state_gas_spent()
    } else if is_create {
        -(initial_state_gas as i64)
    } else {
        0
    };
    // A top-level CREATE that succeeds at a pre-existing alive target refunds its upfront
    // `create_state_gas` (already credited to the reservoir by `refund_create_state_gas`), so it
    // must not count toward block state gas either.
    let alive_create_refund =
        if is_create && result.stop.is_success() && result.created_target_was_alive {
            initial_state_gas
        } else {
            0
        };
    let state_gas_spent = (exec_state_gas.saturating_add_unsigned(initial_state_gas).max(0) as u64)
        .saturating_sub(state_refund)
        .saturating_sub(alive_create_refund);
    if host.feature(EvmFeatures::FEE_CHARGE) {
        let caller_refund = U256::from(gas_remaining) * gas_price;
        host.state
            .account(&caller, false)
            .map_err(error_handler!(host))?
            .add_balance(caller_refund);
        let beneficiary_gas_price = if host.feature(EvmFeatures::BASE_FEE_CHECK) {
            gas_price.saturating_sub(host.block.basefee)
        } else {
            gas_price
        };
        let beneficiary = host.block.beneficiary;
        let beneficiary_reward = U256::from(gas_used) * beneficiary_gas_price;
        host.state
            .account(&beneficiary, false)
            .map_err(error_handler!(host))?
            .add_balance(beneficiary_reward);
    }
    Ok(TxResult {
        status: result.stop.is_success(),
        total_gas_spent,
        state_gas_spent,
        refunded,
        floor_gas,
        stop: result.stop,
        output: result.output,
        created_address: result.created_address,
        ext: T::TxResultExt::default(),
        ..TxResult::default()
    })
}

const fn final_tx_gas<T: EvmTypesHost>(
    result: &MessageResult<T>,
    tx_gas_limit: u64,
    max_refund_quotient: u64,
    floor_gas: u64,
) -> (u64, u64) {
    let gas_remaining = result.gas_remaining_after_final_refund(tx_gas_limit, max_refund_quotient);
    let gas_used = result.gas_used_after_final_refund(tx_gas_limit, max_refund_quotient);
    // EIP-7623 charges at least the calldata floor after applying refunds.
    if gas_used < floor_gas {
        return (tx_gas_limit.saturating_sub(floor_gas), floor_gas);
    }
    (gas_remaining, gas_used)
}

/// Returns the account and storage-key counts in an access list.
pub fn access_list_counts(access_list: &AccessList) -> (u64, u64) {
    (access_list.len() as u64, access_list.storage_keys_count() as u64)
}

/// Calculates transaction calldata floor gas.
pub fn floor_gas(
    version: &Version,
    caller: Address,
    to: TxKind,
    value: U256,
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

    // tokens for access list
    let al_multiplier = version.gas_params.get(GasId::TxAccessListFloorByteMultiplier) as u64;
    let mut tokens = (access_list_accounts * 20 + access_list_storage_keys * 32) * al_multiplier;

    // tokens for input. EIP-7623 weights zero bytes at `TxFloorZeroByteMultiplier`
    // (1) and non-zero bytes at `TxTokenNonZeroByteMultiplier` (4); EIP-7976
    // raises the zero-byte weight to 4 so every byte counts uniformly.
    let non_zero_multiplier = u64::from(params.get(GasId::TxTokenNonZeroByteMultiplier));
    let zero_multiplier = u64::from(params.get(GasId::TxFloorZeroByteMultiplier));
    let zero_data_len = input.iter().filter(|v| **v == 0).count() as u64;
    let non_zero_data_len = input.len() as u64 - zero_data_len;
    tokens += zero_data_len * zero_multiplier + non_zero_data_len * non_zero_multiplier;

    let base = if version.feature(EvmFeatures::EIP2780) {
        let is_create = to.is_create();
        let is_self_transfer = matches!(to, TxKind::Call(to) if to == caller);
        eip2780_base_to_value_gas(version, is_create, is_self_transfer, value)
    } else {
        u64::from(params.get(GasId::TxFloorCostBase))
    };
    base + tokens * floor_cost_per_token
}

/// Calculates intrinsic transaction gas.
///
/// `caller`/`value` feed the EIP-2780 decomposed model (which branches on
/// self-transfer and whether `tx.value` is zero); the legacy model ignores them.
pub fn intrinsic_gas(
    version: &Version,
    caller: Address,
    to: TxKind,
    input: &Bytes,
    access_list_accounts: u64,
    access_list_storage_keys: u64,
    value: U256,
) -> u64 {
    let params = &version.gas_params;
    let non_zero_multiplier = if version.feature(EvmFeatures::EIP2028) { 16 } else { 68 };
    let mut gas = 0;
    for byte in input {
        gas += if *byte == 0 { 4 } else { non_zero_multiplier };
    }
    gas += access_list_accounts * u64::from(params.get(GasId::TxAccessListAddressCost));
    gas += access_list_storage_keys * u64::from(params.get(GasId::TxAccessListStorageKeyCost));

    // Base + `to`-based + `value`-based charges.
    let is_create = to.is_create();
    if version.feature(EvmFeatures::EIP2780) {
        // EIP-2780: decomposed model replacing the legacy 21,000 base.
        let is_self_transfer = matches!(to, TxKind::Call(to) if to == caller);
        gas += eip2780_base_to_value_gas(version, is_create, is_self_transfer, value);
    } else {
        gas += 21_000;
        if is_create && version.feature(EvmFeatures::EIP2) {
            gas += u64::from(params.get(GasId::TxCreateCost));
        }
    }
    if is_create && version.feature(EvmFeatures::EIP3860) {
        gas += u64::from(params.get(GasId::TxInitcodeCost)) * num_words(input.len()) as u64;
    }
    gas
}

/// EIP-2780: sum of the sender base, `tx.to`-based, and `tx.value`-based
/// regular-gas charges. Excludes calldata, access list, authorizations, and
/// initcode pieces which are added by the caller.
///
/// Per execution-specs, a self-transfer (`tx.to == sender`) pays neither the
/// `to`- nor `value`-based charge — only the base. Precompile recipients are
/// charged the same as any other account (the precompile carve-out from the
/// draft is not implemented).
fn eip2780_base_to_value_gas(
    version: &Version,
    is_create: bool,
    is_self_transfer: bool,
    value: U256,
) -> u64 {
    let params = &version.gas_params;
    let mut gas = u64::from(EIP2780_TX_BASE_COST);
    if is_create {
        gas += u64::from(params.get(GasId::TxCreateAccessCost));
        if !value.is_zero() {
            gas += u64::from(params.get(GasId::TxTransferLogCost));
        }
    } else if !is_self_transfer {
        gas += u64::from(EIP8038_COLD_ACCOUNT_ACCESS);
        if !value.is_zero() {
            gas += u64::from(params.get(GasId::TxTransferLogCost))
                + u64::from(params.get(GasId::TxValueCost));
        }
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
    use alloy_consensus::{TxEip2930, TxEip7702, TxLegacy, transaction::Recovered};
    use alloy_eips::{
        eip2930::AccessList,
        eip7702::{Authorization, RecoveredAuthority, RecoveredAuthorization},
    };

    #[test]
    fn intrinsic_gas_charges_shanghai_create_initcode_words() {
        let input = Bytes::from(vec![1; 74]);

        let sender = Address::with_last_byte(0xaa);
        assert_eq!(
            intrinsic_gas(
                Version::base(SpecId::LONDON),
                sender,
                TxKind::Create,
                &input,
                0,
                0,
                U256::ZERO
            ),
            21_000 + 32_000 + 74 * 16
        );
        assert_eq!(
            intrinsic_gas(
                Version::base(SpecId::SHANGHAI),
                sender,
                TxKind::Create,
                &input,
                0,
                0,
                U256::ZERO
            ),
            21_000 + 32_000 + 74 * 16 + 3 * 2
        );
    }

    #[test]
    fn intrinsic_gas_charges_access_list_items() {
        let input = Bytes::new();
        let sender = Address::with_last_byte(0xaa);

        assert_eq!(
            intrinsic_gas(
                Version::base(SpecId::BERLIN),
                sender,
                TxKind::Call(Address::ZERO),
                &input,
                2,
                3,
                U256::ZERO
            ),
            21_000 + 2 * 2400 + 3 * 1900
        );
        assert_eq!(
            intrinsic_gas(
                Version::base(SpecId::AMSTERDAM),
                sender,
                TxKind::Call(Address::ZERO),
                &input,
                1,
                1,
                U256::ZERO
            ),
            // EIP-2780 replaces the 21,000 base with TX_BASE (12,000) +
            // COLD_ACCOUNT_ACCESS (3,000) for the zero-value call recipient.
            // EIP-8038 sets the per-item access-list base to COLD_ACCOUNT_ACCESS /
            // COLD_STORAGE_ACCESS (both 3,000).
            (12_000 + 3000) + (3000 + 20 * 64) + (3000 + 32 * 64)
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
        let tx = Recovered::new_unchecked(
            TxEnvelope::Eip2930(TxEip2930 {
                chain_id: 1,
                nonce: 0,
                gas_price: 1,
                gas_limit: 20_999,
                to: TxKind::Call(Address::with_last_byte(0xbb)),
                value: U256::ZERO,
                input: Bytes::new(),
                access_list: AccessList::default(),
            }),
            caller,
        );
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::BERLIN,
            BlockEnv::default(),
            ethereum_tx_registry(SpecId::BERLIN),
            database,
            Precompiles::base(SpecId::BERLIN),
        );

        assert_eq!(
            evm.transact(&tx).map(|executed| executed.discard()),
            Err(HandlerError::IntrinsicGasTooLow { required: 21_000, got: 20_999 })
        );
    }

    #[test]
    fn amsterdam_create_state_gas_out_of_gas_is_runtime_failure() {
        let caller = Address::with_last_byte(0xaa);
        let mut database = InMemoryDB::default();
        database.insert_account_info(
            &caller,
            AccountInfo::default().with_balance(U256::from(1_000_000_000u64)),
        );
        let tx = Recovered::new_unchecked(
            TxEnvelope::Legacy(TxLegacy {
                gas_limit: 100_000,
                to: TxKind::Create,
                input: Bytes::from_static(&[op::STOP]),
                ..Default::default()
            }),
            caller,
        );
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::AMSTERDAM,
            BlockEnv::default(),
            ethereum_tx_registry(SpecId::AMSTERDAM),
            database,
            Precompiles::base(SpecId::AMSTERDAM),
        );

        let result = evm.transact(&tx).expect("runtime OOG is an executed transaction").discard();
        assert!(!result.status);
        assert_eq!(result.stop, InstrStop::OutOfGas);
        assert_eq!(result.tx_gas_used(), 100_000);
    }

    #[test]
    fn amsterdam_authorization_state_gas_out_of_gas_is_runtime_failure() {
        let caller = Address::with_last_byte(0xaa);
        let authority = Address::with_last_byte(0xcc);
        let mut database = InMemoryDB::default();
        database.insert_account_info(
            &caller,
            AccountInfo::default().with_balance(U256::from(1_000_000_000u64)),
        );
        let authorization = RecoveredAuthorization::new_unchecked(
            Authorization {
                chain_id: U256::from(1),
                address: Address::with_last_byte(0xdd),
                nonce: 0,
            },
            RecoveredAuthority::Valid(authority),
        );
        let tx = LazyTxEip7702::from_cached_recovered_authorizations(
            TxEip7702 {
                chain_id: 1,
                gas_limit: 100_000,
                to: Address::with_last_byte(0xbb),
                ..Default::default()
            },
            vec![authorization],
        );
        let tx = Recovered::new_unchecked(TxEnvelope::Eip7702(tx), caller);
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::AMSTERDAM,
            BlockEnv::default(),
            ethereum_tx_registry(SpecId::AMSTERDAM),
            database,
            Precompiles::base(SpecId::AMSTERDAM),
        );

        let result = evm.transact(&tx).expect("runtime OOG is an executed transaction").discard();
        assert!(!result.status);
        assert_eq!(result.stop, InstrStop::OutOfGas);
        assert_eq!(result.tx_gas_used(), 100_000);
    }

    #[test]
    fn floor_gas_charges_prague_calldata_tokens() {
        let input = Bytes::from_static(&[0, 1, 2]);
        let caller = Address::with_last_byte(0xaa);
        let to = TxKind::Call(Address::with_last_byte(0xbb));
        let mut prague_without_eip7623 = Version::new(SpecId::PRAGUE);
        prague_without_eip7623.features.remove(EvmFeatures::EIP7623);

        assert_eq!(
            floor_gas(Version::base(SpecId::SHANGHAI), caller, to, U256::ZERO, &input, 0, 0),
            0
        );
        assert_eq!(
            floor_gas(Version::base(SpecId::PRAGUE), caller, to, U256::ZERO, &input, 0, 0),
            21_000 + 9 * 10
        );
        assert_eq!(floor_gas(&prague_without_eip7623, caller, to, U256::ZERO, &input, 0, 0), 0);
    }

    #[test]
    fn floor_gas_charges_amsterdam_access_list_tokens() {
        let input = Bytes::from(vec![1; 1000]);
        let caller = Address::with_last_byte(0xaa);
        let to = TxKind::Call(Address::with_last_byte(0xbb));
        let value = U256::from(1);

        assert_eq!(
            floor_gas(Version::base(SpecId::AMSTERDAM), caller, to, value, &input, 1, 1),
            // EIP-2780 anchors the floor on sender + recipient + value charges.
            21_000 + (1000 * 4 + 80 + 128) * 16
        );

        // EIP-7976: amsterdam weights zero calldata bytes the same as non-zero
        // bytes in the floor (4 tokens each), unlike EIP-7623 (zero = 1 token).
        let zero_input = Bytes::from(vec![0; 1000]);
        assert_eq!(
            floor_gas(Version::base(SpecId::AMSTERDAM), caller, to, value, &zero_input, 1, 1),
            21_000 + (1000 * 4 + 80 + 128) * 16
        );
        // Prague keeps the EIP-7623 split: zero bytes count as one token each.
        assert_eq!(
            floor_gas(Version::base(SpecId::PRAGUE), caller, to, value, &zero_input, 0, 0),
            21_000 + 1000 * 10
        );
    }

    #[test]
    fn amsterdam_floor_uses_decomposed_value_transfer_base() {
        let caller = Address::with_last_byte(0xaa);
        let to = TxKind::Call(Address::with_last_byte(0xbb));

        assert_eq!(
            floor_gas(
                Version::base(SpecId::AMSTERDAM),
                caller,
                to,
                U256::from(1),
                &Bytes::from_static(&[0]),
                0,
                0,
            ),
            21_064
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
    fn balance_top_up_can_be_disabled_independently() {
        let caller = Address::with_last_byte(0xaa);
        let mut version = Version::new(SpecId::OSAKA);
        version.features.remove(EvmFeatures::BALANCE_CHECK);
        version.features.remove(EvmFeatures::BALANCE_TOP_UP);
        let mut evm = Evm::<BaseEvmTypes>::new_with_execution_config(
            ExecutionConfig::for_spec_and_version(SpecId::OSAKA, version),
            SpecId::OSAKA,
            BlockEnv::default(),
            TxRegistry::new(),
            InMemoryDB::default(),
            Precompiles::base(SpecId::OSAKA),
        );

        assert!(validate_sender(&mut evm, caller, 0, U256::from(100)).is_ok());
        assert!(evm.state.account_info_untracked(&caller).unwrap().is_none());
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
        assert_eq!(
            evm.state.account_info_untracked(&caller).unwrap().unwrap().balance,
            U256::from(100)
        );
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

        assert_eq!(final_tx_gas(&result, 100_000, 5, 60_000), (40_000, 60_000));
    }

    #[test]
    fn final_tx_gas_preserves_higher_actual_usage() {
        let result = MessageResult::<BaseEvmTypes> {
            stop: crate::interpreter::InstrStop::Return,
            gas: GasTracker::new_used_gas(100_000, 70_000, 0),
            ..MessageResult::<BaseEvmTypes>::default()
        };

        assert_eq!(final_tx_gas(&result, 100_000, 5, 60_000), (30_000, 70_000));
    }

    #[test]
    fn initial_delegated_call_keeps_target_code_address() {
        let caller = Address::with_last_byte(0xaa);
        let target = Address::with_last_byte(0x42);
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
            0,
            0,
        )
        .unwrap();
        assert_eq!(message.destination, target);
        assert_eq!(message.code_address, target);

        let result = Host::execute_message(&mut evm, &TxEnv::default(), bytecode, &mut message);

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
