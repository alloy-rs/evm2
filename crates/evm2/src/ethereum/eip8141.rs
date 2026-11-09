//! Frame transaction preparation and execution.

use super::{validate_block_gas_limit, validate_chain_id};
use crate::{
    Evm, EvmFeatures, EvmTypes, HostError, TxResult, TxResultExt, Version,
    bytecode::Bytecode,
    env::{TxEnv, TxEnvExt},
    evm::{
        FrameTransactionResult, StateCheckpoint,
        frame::FrameTransactionRuntime,
        handler::{DefaultTxHandlerHooks, TxHandlerHooks},
    },
    interpreter::{
        GasTracker, Host, InstrStop, Message, MessageExt, MessageKind, MessageResultExt,
    },
    precompiles,
    registry::{FrameTransactionError, HandlerError, HandlerResult, TxHandler, TxRequest},
    version::GasId,
};
use alloc::{sync::Arc, vec::Vec};
use alloy_consensus::TxEip8141;
use alloy_eips::{
    eip4844::{DATA_GAS_PER_BLOB, VERSIONED_HASH_VERSION_KZG},
    eip8141::{
        ENTRY_POINT, Eip8141Error, FRAME_TX_INTRINSIC_COST, FRAME_TX_PER_FRAME_COST, Frame,
        FrameGasUsed, FrameMode, FrameReceipt, FrameStatus, MAX_FRAMES, MAX_NONCE_KEYS,
        NONCE_MANAGER, SignatureScheme, nonce_calldata, nonce_slot, validate_nonce_keys,
    },
};
use alloy_primitives::{Address, B256, Bytes, U256};
use core::marker::PhantomData;
use derive_where::derive_where;

/// Gas reservations for a frame transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameGas {
    /// Intrinsic execution cost, including all signature verification.
    pub intrinsic: u64,
    /// Calldata floor in the execution dimension.
    pub floor: u64,
    /// Worst-case execution dimension for block admission.
    pub execution: u64,
    /// Worst-case state dimension for block admission.
    pub state: u64,
    /// Combined maximum gas funded by payment approval.
    pub maximum: u64,
    /// Protocol signature verification work.
    pub signatures: u64,
}

/// A frame reservation computed at block admission for the corresponding payload.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FrameGasReservation<'a> {
    pub(crate) tx: &'a TxEip8141,
    pub(crate) gas: FrameGas,
}

/// Prepared frame transaction.
#[derive(Clone, Debug)]
pub struct PreparedFrameTx {
    pub(super) payload: Arc<TxEip8141>,
    pub(super) signature_hash: B256,
    pub(super) gas: FrameGas,
    /// Transaction-wide lower bound returned by GASPRICE.
    pub(super) gas_price: U256,
    /// Base fee used for accounting, including simulation overrides.
    pub(super) base_fee: U256,
    /// Blob fee charged at the current block's blob base fee.
    pub(super) blob_charge: U256,
    /// Amount escrowed at payment approval, bounded by the signed max_fee.
    pub(super) max_cost: U256,
    /// Legacy account nonce before any frame executes.
    pub(super) legacy_nonce: u64,
}

/// Result of frame prefix validation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameValidationResult {
    /// Account that successfully approved payment.
    pub payer: Address,
    /// Maximum approved payment exposure.
    pub max_cost: U256,
    /// Exclusive index after the successful payment frame.
    pub prefix_end: usize,
    /// Whether execution as the sender was approved.
    pub sender_approved: bool,
    /// Signature verification gas for the complete transaction.
    pub signature_gas: u64,
    /// Execution gas consumed by the prefix frames.
    pub execution_gas: u64,
    /// State gas consumed by the prefix frames.
    pub state_gas: u64,
}

struct Batch {
    state: StateCheckpoint,
    approval: crate::evm::frame::FrameApprovalState,
    gas: usize,
    start: usize,
    warmth: Option<Vec<(Address, Vec<U256>)>>,
}

/// Wraps a typed validation reason as a frame transaction error.
const fn invalid(reason: FrameTransactionError) -> HandlerError {
    HandlerError::InvalidFrameTransaction(reason)
}

impl FrameGas {
    /// Computes gas reservations using the active schedule.
    pub fn calculate(tx: &TxEip8141, version: &Version) -> HandlerResult<Self> {
        let gp = &version.gas_params;
        let nonce_data =
            tx.nonce_keys.as_deref().map(|keys| nonce_calldata(keys, tx.nonce)).unwrap_or_default();
        let sum = |a: u64, b: u64| {
            a.checked_add(b).ok_or(invalid(FrameTransactionError::GasLimitOverflow))
        };
        let mul = |a: u64, b: u64| {
            a.checked_mul(b).ok_or(invalid(FrameTransactionError::GasLimitOverflow))
        };
        let count = tx.nonce_keys.as_deref().map_or(0, |keys| keys.len() as u64);
        let access = mul(
            count,
            sum(
                sum(gp.get(GasId::WarmStorageReadCost), gp.get(GasId::ColdStorageAdditionalCost))?,
                gp.get(GasId::SstoreSetWithoutLoadCost),
            )?,
        )?;
        let signatures = tx.signature_verification_gas();
        let value_transfers = tx
            .frames
            .iter()
            .filter(|frame| !frame.value.is_zero() && frame.resolved_target(tx.sender) != tx.sender)
            .count() as u64;
        let value_transfer_gas = mul(value_transfers, gp.get(GasId::TxValueCost))?;
        let base = sum(
            sum(
                sum(
                    FRAME_TX_INTRINSIC_COST,
                    mul(tx.frames.len() as u64, FRAME_TX_PER_FRAME_COST)?,
                )?,
                signatures,
            )?,
            sum(value_transfer_gas, access)?,
        )?;
        let mut fields = core::iter::once(nonce_data.as_slice())
            .chain(tx.frames.iter().map(|frame| frame.data.as_ref()))
            .chain(tx.signatures.iter().flat_map(|sig| {
                [
                    sig.signer.as_bytes(),
                    sig.msg.as_bytes(),
                    if sig.signs_transaction_hash() { &[] } else { sig.signature.as_ref() },
                ]
            }));
        let (tokens, floor_tokens) = fields.try_fold((0, 0), |(tokens, floor), bytes| {
            let (data_tokens, floor_tokens) = gp
                .checked_tx_calldata_tokens(bytes)
                .ok_or(invalid(FrameTransactionError::GasLimitOverflow))?;
            Ok::<_, HandlerError>((sum(tokens, data_tokens)?, sum(floor, floor_tokens)?))
        })?;
        // The signing hash commits to an empty-message witness's length, so its intrinsic
        // price must depend on that length alone, including when its bytes are all zero.
        let elided_tokens = tx
            .signatures
            .iter()
            .filter(|sig| sig.signs_transaction_hash())
            .try_fold(0, |s, sig| {
                sum(
                    s,
                    mul(sig.signature.len() as u64, gp.get(GasId::TxTokenNonZeroByteMultiplier))?,
                )
            })?;
        let tokens = sum(tokens, elided_tokens)?;
        let floor_tokens = sum(floor_tokens, elided_tokens)?;
        let intrinsic = sum(base, mul(tokens, gp.get(GasId::TxTokenCost))?)?;
        let floor = sum(base, mul(floor_tokens, gp.get(GasId::TxFloorCostPerToken))?)?;
        // Positive intrinsic gas makes checked totals reject saturation in the shared frame sums.
        let execution = sum(intrinsic, tx.total_frame_execution_gas_limit())?.max(floor);
        let state = tx.total_frame_state_gas_limit();
        Ok(Self { intrinsic, floor, execution, state, maximum: sum(execution, state)?, signatures })
    }
}

/// Executes an EIP-8141 transaction using Ethereum rules.
pub fn handle<T: EvmTypes>(req: TxRequest<'_, '_, T, TxEip8141>) -> HandlerResult<TxResult<T>> {
    handle_with_hooks::<T, DefaultTxHandlerHooks>(req)
}

/// Executes an EIP-8141 transaction using Ethereum rules and custom handler hooks.
pub fn handle_with_hooks<T: EvmTypes, H: TxHandlerHooks<T>>(
    mut req: TxRequest<'_, '_, T, TxEip8141>,
) -> HandlerResult<TxResult<T>> {
    let prepared = prepare_with_hooks::<T, H>(&mut req)?;
    execute_prepared::<T, H>(req, prepared)
}

/// Validates an EIP-8141 transaction and applies its pre-execution handler hook.
pub fn prepare_with_hooks<T: EvmTypes, H: TxHandlerHooks<T>>(
    req: &mut TxRequest<'_, '_, T, TxEip8141>,
) -> HandlerResult<PreparedFrameTx> {
    let host = &mut *req.host;
    let tx = *req.tx.inner();
    validate_chain_id(host.version(), Some(tx.chain_id), false)?;
    if !host.feature(EvmFeatures::EIP8141) {
        return Err(HandlerError::UnsupportedTransactionType(6));
    }
    // Match Revm's environment validation before frame structure and signatures.
    let gas = match req.frame_gas {
        Some(reservation) if reservation.tx == tx => reservation.gas,
        _ => FrameGas::calculate(tx, host.version())
            .map_err(|_| invalid(FrameTransactionError::PaymentOverflow))?,
    };
    let blob_charge = U256::from(tx.blob_versioned_hashes.len())
        .checked_mul(U256::from(DATA_GAS_PER_BLOB))
        .and_then(|v| v.checked_mul(host.block.blob_basefee))
        .ok_or(invalid(FrameTransactionError::BaseFeeOverflow))?;
    let max_gas = U256::from(gas.maximum);
    let base_fee =
        if host.feature(EvmFeatures::BASE_FEE_CHECK) { host.block.basefee } else { U256::ZERO };
    let required_max_fee = max_gas
        .checked_mul(base_fee)
        .and_then(|v| v.checked_add(blob_charge))
        .ok_or(invalid(FrameTransactionError::BaseFeeOverflow))?;
    let budget_left = tx.fees.max_fee.checked_sub(required_max_fee).ok_or(
        HandlerError::FrameFeeBudgetTooLow { required: required_max_fee, got: tx.fees.max_fee },
    )?;
    if tx.blob_versioned_hashes.iter().any(|h| h[0] != VERSIONED_HASH_VERSION_KZG) {
        return Err(HandlerError::BlobVersionNotSupported);
    }
    if tx.blob_versioned_hashes.len() > host.version().max_blobs_per_tx {
        return Err(HandlerError::TooManyBlobs {
            have: tx.blob_versioned_hashes.len(),
            max: host.version().max_blobs_per_tx,
        });
    }
    let keyed_nonces = host.feature(EvmFeatures::EIP8250);
    if tx.nonce == u64::MAX
        && !(keyed_nonces && tx.nonce_keys.as_ref().is_some_and(|keys| !keys.is_empty()))
    {
        return Err(HandlerError::NonceOverflow);
    }
    if tx.nonce_keys.is_some() != keyed_nonces {
        return Err(invalid(FrameTransactionError::NoncePayloadMismatch));
    }
    if let Some(keys) = &tx.nonce_keys {
        validate_nonce_keys(keys).map_err(|_| {
            invalid(if keys.len() > MAX_NONCE_KEYS {
                FrameTransactionError::TooManyNonceKeys
            } else {
                FrameTransactionError::NonceKeysNotIncreasing
            })
        })?;
    }
    if tx.frames.is_empty() || tx.frames.len() > MAX_FRAMES {
        return Err(invalid(FrameTransactionError::InvalidFrameCount));
    }
    if gas.execution > host.version().tx_gas_limit_cap {
        return Err(HandlerError::TxGasLimitGreaterThanCap {
            gas_limit: gas.execution,
            cap: host.version().tx_gas_limit_cap,
        });
    }
    validate_block_gas_limit(host.version(), gas.execution.max(gas.state), host.block.gas_limit)?;
    let placeholders = host.feature(EvmFeatures::FRAME_SIGNATURE_PLACEHOLDERS);
    validate_structure(tx, placeholders, host.feature(EvmFeatures::EIP7906))?;
    // The requested tip product can exceed U256 even when the signed budget is valid.
    // Saturating before the minimum preserves the arbitrary-precision result.
    let max_priority_fee =
        tx.fees.max_priority_fee_per_gas.saturating_mul(max_gas).min(budget_left);
    let max_cost = required_max_fee + max_priority_fee;
    let gas_price = base_fee + max_priority_fee / max_gas;
    let signature_hash = tx.signature_hash();
    validate_signatures(tx, signature_hash, placeholders)?;
    if host.feature(EvmFeatures::NONCE_CHECK)
        && let Some(keys) = tx.nonce_keys.as_deref().filter(|keys| !keys.is_empty())
    {
        for key in keys {
            if host.state.storage(&NONCE_MANAGER)?.into_slot(nonce_slot(tx.sender, *key))?.current()
                != U256::from(if tx.nonce == u64::MAX { 0 } else { tx.nonce })
            {
                return Err(invalid(FrameTransactionError::NonceSequenceMismatch));
            }
        }
    } else if host.feature(EvmFeatures::NONCE_CHECK) {
        let expected = host.state.account(&tx.sender)?.nonce();
        if expected != tx.nonce {
            return Err(HandlerError::InvalidNonce { expected, got: tx.nonce });
        }
    }
    let legacy_nonce = host.state.account(&tx.sender)?.nonce();
    let prepared = PreparedFrameTx {
        payload: Arc::new(tx.clone()),
        signature_hash,
        gas,
        gas_price,
        base_fee,
        blob_charge,
        max_cost,
        legacy_nonce,
    };
    H::before_frame_execution(host, req.envelope, &prepared)?;
    Ok(prepared)
}

/// Executes and settles a prepared EIP-8141 transaction.
pub fn execute_prepared<T: EvmTypes, H: TxHandlerHooks<T>>(
    req: TxRequest<'_, '_, T, TxEip8141>,
    prepared: PreparedFrameTx,
) -> HandlerResult<TxResult<T>> {
    req.host.with_frame_execution(|host| {
        let result = execute_frames(host, &prepared, None)?;
        let result = H::settle_frame_transaction(host, req.envelope, &prepared, result)?;
        host.inspect_frame(|inspector, host| inspector.frame_transaction_end(&result, host));
        Ok(result)
    })
}

/// Native frame transaction handler with executable prefix validation.
#[derive_where(Clone, Copy, Debug, Default)]
pub struct FrameHandler<H = DefaultTxHandlerHooks>(PhantomData<fn() -> H>);

impl<H> FrameHandler<H> {
    /// Creates a frame handler using `H` for preparation and settlement.
    pub const fn new() -> Self {
        Self(PhantomData)
    }
}

impl<T: EvmTypes, H: TxHandlerHooks<T>> TxHandler<T, TxEip8141, TxResult<T>> for FrameHandler<H> {
    fn validate(&self, mut req: TxRequest<'_, '_, T, TxEip8141>) -> HandlerResult<()> {
        prepare_with_hooks::<T, H>(&mut req).map(|_| ())
    }

    fn execute(&self, req: TxRequest<'_, '_, T, TxEip8141>) -> HandlerResult<TxResult<T>> {
        handle_with_hooks::<T, H>(req)
    }

    fn validate_frame_prefix(
        &self,
        req: TxRequest<'_, '_, T, TxEip8141>,
        prefix_end: usize,
    ) -> HandlerResult<FrameValidationResult> {
        validate_prefix_with_hooks::<T, H>(req, prefix_end)
    }
}

/// Validates payment approval through `prefix_end` without settling fees; caller discards effects.
fn validate_prefix_with_hooks<T: EvmTypes, H: TxHandlerHooks<T>>(
    mut req: TxRequest<'_, '_, T, TxEip8141>,
    prefix_end: usize,
) -> HandlerResult<FrameValidationResult> {
    if prefix_end == 0 || prefix_end > req.tx.inner().frames.len() {
        return Err(invalid(FrameTransactionError::PrefixOutOfBounds));
    }
    let prepared = prepare_with_hooks::<T, H>(&mut req)?;
    req.host.with_frame_execution(|host| {
        let result = execute_frames(host, &prepared, Some(prefix_end))?;
        host.inspect_frame(|inspector, host| inspector.frame_transaction_end(&result, host));
        let frame = result.frame.as_ref().unwrap();
        if frame.receipts.len() != prefix_end {
            return Err(invalid(FrameTransactionError::PrefixBoundaryMismatch));
        }
        Ok(FrameValidationResult {
            payer: frame.payer,
            max_cost: prepared.max_cost,
            prefix_end,
            sender_approved: host.frame_tx.as_ref().unwrap().approval.sender_approved,
            signature_gas: prepared.gas.signatures,
            execution_gas: frame.receipts.iter().map(|receipt| receipt.gas_used.execution).sum(),
            state_gas: result.state_gas_spent,
        })
    })
}

/// Verifies protocol signatures and signer identities, honoring simulation placeholders.
fn validate_signatures(tx: &TxEip8141, hash: B256, placeholders: bool) -> HandlerResult<()> {
    for sig in &tx.signatures {
        if placeholders && sig.scheme != SignatureScheme::Arbitrary && sig.signature.is_empty() {
            continue;
        }
        let msg = if sig.signs_transaction_hash() {
            hash.0
        } else {
            sig.explicit_message().ok_or(invalid(FrameTransactionError::InvalidSignatureMessage))?.0
        };
        let expected = if sig.signer.is_empty() {
            tx.sender
        } else {
            sig.signer_address().unwrap_or_default()
        };
        let valid = match sig.scheme {
            SignatureScheme::Arbitrary => true,
            SignatureScheme::Secp256k1 => {
                let recovered = precompiles::crypto()
                    .secp256k1_ecrecover(
                        sig.signature[1..].try_into().unwrap(),
                        sig.signature[0],
                        &msg,
                    )
                    .ok()
                    .map(|v| Address::from_slice(&v[12..]));
                if recovered.is_some_and(|address| address != expected) {
                    return Err(invalid(FrameTransactionError::SignatureSignerMismatch));
                }
                recovered == Some(expected)
            }
            SignatureScheme::P256 => precompiles::crypto().secp256r1_verify_signature(
                &msg,
                sig.signature[..64].try_into().unwrap(),
                sig.signature[64..].try_into().unwrap(),
            ),
        };
        if !valid {
            return Err(invalid(FrameTransactionError::SignatureValidationFailed));
        }
    }
    Ok(())
}

/// Checks frame flags, batching, assertion ordering, expiry fields, and signature structure.
fn validate_structure(tx: &TxEip8141, placeholders: bool, assertions: bool) -> HandlerResult<()> {
    // Shared wire validation also applies a fixed gas schedule. Native execution must use
    // the selected Version, so validate structure here and calculate reservations separately.
    if tx.frames.iter().any(Frame::has_reserved_flags) {
        return Err(invalid(FrameTransactionError::ReservedFrameFlag));
    }
    if !assertions && tx.frames.iter().any(Frame::is_assertion) {
        return Err(invalid(FrameTransactionError::AssertionsNotActive));
    }
    if !alloy_eips::eip8141::has_valid_assertion_suffix(&tx.frames) {
        return Err(invalid(FrameTransactionError::InvalidAssertionSuffix));
    }
    let mut expiry = false;
    for (index, frame) in tx.frames.iter().enumerate() {
        let previous_atomic = index > 0 && tx.frames[index - 1].is_atomic_batch();
        if frame.mode != FrameMode::Sender && !frame.value.is_zero() {
            return Err(invalid(FrameTransactionError::ValueTransferNotAllowed));
        }
        if frame.is_assertion() && frame.is_atomic_batch() {
            return Err(invalid(FrameTransactionError::AtomicAssertion));
        }
        if frame.is_assertion() && previous_atomic {
            return Err(invalid(FrameTransactionError::AtomicBatchEndsOnAssertion));
        }
        if frame.allowed_scope().allows_execution() && frame.resolved_target(tx.sender) != tx.sender
        {
            return Err(invalid(FrameTransactionError::InvalidExecutionApprovalTarget));
        }
        if frame.is_atomic_batch() && frame.mode == FrameMode::Verify {
            return Err(invalid(FrameTransactionError::AtomicVerify));
        }
        if frame.is_atomic_batch()
            && (index + 1 == tx.frames.len() || tx.frames[index + 1].mode == FrameMode::Verify)
        {
            return Err(invalid(FrameTransactionError::InvalidAtomicBatchEnd));
        }
        if (frame.is_atomic_batch() || previous_atomic) && !frame.allowed_scope().is_none() {
            return Err(invalid(FrameTransactionError::AtomicBatchApproval));
        }
        if frame.is_expiry_verifier() {
            if !frame.has_valid_expiry_verifier_fields() {
                return Err(invalid(FrameTransactionError::MalformedExpiryVerifier));
            }
            if expiry {
                return Err(invalid(FrameTransactionError::MultipleExpiryVerifiers));
            }
            expiry = true;
        }
    }
    for signature in &tx.signatures {
        // The shared message enum can be constructed directly with the reserved zero digest.
        // Its scalar/length validator does not check that invariant, including for arbitrary
        // witnesses. Simulation placeholders waive cryptography only, never message validity.
        if !signature.signs_transaction_hash()
            && signature.explicit_message().is_none_or(|message| message.is_zero())
        {
            return Err(invalid(FrameTransactionError::InvalidSignatureMessage));
        }
        if placeholders
            && signature.scheme != SignatureScheme::Arbitrary
            && signature.signature.is_empty()
        {
            continue;
        }
        signature.validate_structure_with_sender(tx.sender).map_err(|error| {
            invalid(match error {
                Eip8141Error::UnexpectedSigner => FrameTransactionError::UnexpectedSignatureSigner,
                Eip8141Error::P256SignerMismatch { .. } => {
                    FrameTransactionError::SignatureSignerMismatch
                }
                _ => FrameTransactionError::SignatureValidationFailed,
            })
        })?;
    }
    Ok(())
}

/// Runs frames with approval and rollback tracking, producing gas usage and receipts.
fn execute_frames<T: EvmTypes>(
    host: &mut Evm<'_, T>,
    prepared: &PreparedFrameTx,
    prefix: Option<usize>,
) -> HandlerResult<TxResult<T>> {
    let tx = &prepared.payload;
    *host.tx_env_mut() = TxEnvExt {
        origin: ENTRY_POINT,
        gas_price: prepared.gas_price,
        chain_id: U256::from(host.version().chain_id),
        blob_hashes: tx
            .blob_versioned_hashes
            .iter()
            .map(|hash| U256::from_be_bytes(hash.0))
            .collect(),
        ..TxEnv::<T>::default()
    };
    host.frame_tx = Some(FrameTransactionRuntime::new(
        tx.clone(),
        prepared.signature_hash,
        prepared.max_cost,
        prepared.legacy_nonce,
    ));
    host.inspect_frame(|inspector, host| {
        inspector.frame_transaction_start(tx, &prepared.gas, host)
    });
    host.state.account(&tx.sender)?.warm();
    if host.feature(EvmFeatures::EIP3651) {
        host.state.prewarm(&host.block.beneficiary);
    }
    host.warm_precompiles();
    let mut receipts = Vec::<FrameReceipt>::with_capacity(tx.frames.len());
    let mut outputs = Vec::with_capacity(tx.frames.len());
    let mut refunds = Vec::with_capacity(tx.frames.len());
    let mut batch = None::<Batch>;
    let mut body = None::<Batch>;
    let mut failed = false;
    let mut assertion_failure = None;
    let mut rolled_back = alloc::vec![false; tx.frames.len()];
    let mut stops = alloc::vec![InstrStop::Stop; tx.frames.len()];
    let mut i = 0;
    let end = prefix.unwrap_or(tx.frames.len());
    while i < end {
        let frame = &tx.frames[i];
        if frame.mode == FrameMode::Sender
            && !host.frame_tx.as_ref().unwrap().approval.sender_approved
        {
            return Err(invalid(FrameTransactionError::SenderNotApproved));
        }
        if batch.is_none() && frame.is_atomic_batch() {
            batch = Some(checkpoint(host, receipts.len(), true));
        }
        let state = host.state.checkpoint();
        let rt = host.frame_tx.as_mut().unwrap();
        rt.current_frame_index = i;
        rt.resolved_target = frame.resolved_target(tx.sender);
        rt.state_gas_used.push(0);
        let gas_checkpoint = rt.state_gas_checkpoint();
        let log_start = host.logs().len();
        host.tx_env_mut().origin =
            if frame.mode == FrameMode::Sender { tx.sender } else { ENTRY_POINT };
        host.inspect_frame(|inspector, host| inspector.frame_start(i, frame, host));
        let (stop, execution, state_used, refund, output) = execute_frame(host, tx, frame)?;
        stops[i] = stop;
        if !stop.is_success() {
            host.state.rollback(state, host.version().features);
            host.frame_tx.as_mut().unwrap().revert_state_gas(gas_checkpoint);
        }
        host.state.clear_transient_storage();
        let logs = if stop.is_success() { host.logs()[log_start..].to_vec() } else { Vec::new() };
        let status = if stop.is_success() { FrameStatus::Success } else { FrameStatus::Failure };
        receipts.push(FrameReceipt {
            status,
            gas_used: FrameGasUsed { execution, state: state_used },
            logs,
        });
        host.inspect_frame(|inspector, host| {
            inspector.frame_end(i, receipts.last().unwrap(), &output, stop, host)
        });
        if !stop.is_success() {
            if frame.mode == FrameMode::Verify {
                return Err(invalid(FrameTransactionError::VerifyFrameFailed));
            }
            if prefix.is_some() {
                return Err(invalid(FrameTransactionError::UnsuccessfulPrefixFrame));
            }
        }
        outputs.push(output);
        refunds.push(refund);
        let rt = host.frame_tx.as_mut().unwrap();
        rt.statuses.push(status);
        rt.execution_gas_used.push(execution);
        rt.state_gas_used[i] = state_used;
        if stop.is_success() {
            rt.commit_root_approval();
        }
        if body.is_none() && stop.is_success() && rt.approval.payer.is_some() {
            body = Some(checkpoint(host, receipts.len(), false));
        }
        if !stop.is_success() && frame.mode == FrameMode::Assert {
            if let Some(c) = body.take() {
                rolled_back[c.start..receipts.len() - 1].fill(true);
                rollback(host, c, &mut receipts, &mut refunds, &mut outputs, true);
            }
            failed = true;
            assertion_failure = Some(i);
            for _ in i + 1..tx.frames.len() {
                skipped(host, &mut receipts, &mut refunds, &mut outputs);
            }
            break;
        }
        if !stop.is_success()
            && let Some(c) = batch.take()
        {
            rolled_back[c.start..receipts.len() - 1].fill(true);
            rollback(host, c, &mut receipts, &mut refunds, &mut outputs, false);
            while tx.frames[i].is_atomic_batch() {
                i += 1;
                skipped(host, &mut receipts, &mut refunds, &mut outputs);
            }
        } else if !frame.is_atomic_batch() {
            batch = None;
        }
        if let Some(prefix_end) = prefix
            && host.frame_tx.as_ref().unwrap().approval.payer.is_some()
        {
            if receipts.len() != prefix_end {
                return Err(invalid(FrameTransactionError::UnsuccessfulPrefixFrame));
            }
            break;
        }
        i += 1;
    }
    let rt = host.frame_tx.as_ref().unwrap();
    let payer = rt.approval.payer.ok_or(invalid(FrameTransactionError::MissingPayer))?;
    let precharge = rt.approval.gas_pre_charge;
    for (r, state) in receipts.iter_mut().zip(&rt.state_gas_used) {
        r.gas_used.state = *state;
    }
    let state = receipts.iter().map(|r| r.gas_used.state).sum::<u64>();
    let spent =
        prepared.gas.intrinsic + receipts.iter().map(|r| r.gas_used.execution).sum::<u64>() + state;
    let refund = refunds.into_iter().fold(0i64, i64::saturating_add).max(0) as u64;
    let refund_quotient = host.version().gas_params.get(GasId::MaxRefundQuotient).max(1);
    let refund = refund.min(spent / refund_quotient);
    let result = TxResultExt {
        status: !failed,
        total_gas_spent: spent,
        state_gas_spent: state,
        floor_gas: prepared.gas.floor,
        refunded: refund,
        stop: if failed { InstrStop::Revert } else { InstrStop::Return },
        frame: Some(FrameTransactionResult {
            payer,
            precharge,
            gas_price: prepared.gas_price,
            assertion_failure,
            rolled_back,
            stops,
            receipts,
            outputs,
        }),
        ..Default::default()
    };
    Ok(result)
}

/// Refunds unused payer escrow and pays the beneficiary's capped priority fee.
pub(crate) fn settle_fees<T: EvmTypes>(
    host: &mut Evm<'_, T>,
    prepared: &PreparedFrameTx,
    result: &TxResult<T>,
) -> HandlerResult<()> {
    if !host.feature(EvmFeatures::FEE_CHARGE) {
        return Ok(());
    }
    let used = U256::from(result.tx_gas_used());
    let base_fee_paid = used
        .checked_mul(prepared.base_fee)
        .and_then(|value| value.checked_add(prepared.blob_charge))
        .ok_or(invalid(FrameTransactionError::PaymentOverflow))?;
    let available_tip = prepared
        .max_cost
        .checked_sub(base_fee_paid)
        .ok_or(invalid(FrameTransactionError::PaymentOverflow))?;
    let priority_fee_paid =
        prepared.payload.fees.max_priority_fee_per_gas.saturating_mul(used).min(available_tip);
    let actual = base_fee_paid + priority_fee_paid;
    let frame = result.frame.as_ref().expect("frame handler produces frame results");
    host.state.account(&frame.payer)?.add_balance(frame.precharge.saturating_sub(actual));
    let beneficiary = host.block.beneficiary;
    host.state.account(&beneficiary)?.add_balance(priority_fee_paid);
    Ok(())
}

/// Snapshots batch state, approval, state gas, and optional account/storage warmth.
fn checkpoint<T: EvmTypes>(host: &Evm<'_, T>, start: usize, warmth: bool) -> Batch {
    let rt = host.frame_tx.as_ref().unwrap();
    Batch {
        state: host.state.checkpoint(),
        approval: rt.approval,
        gas: rt.state_gas_checkpoint(),
        start,
        warmth: warmth.then(|| host.state.frame_warmth_snapshot()),
    }
}

/// Restores a batch checkpoint and clears reverted logs, state gas, refunds, and optional outputs.
fn rollback<T: EvmTypes>(
    host: &mut Evm<'_, T>,
    c: Batch,
    receipts: &mut [FrameReceipt],
    refunds: &mut [i64],
    outputs: &mut [Bytes],
    clear_outputs: bool,
) {
    host.state.rollback(c.state, host.version().features);
    if let Some(warmth) = c.warmth {
        host.state.restore_frame_warmth(&warmth);
    }
    let rt = host.frame_tx.as_mut().unwrap();
    rt.restore_approval(c.approval);
    rt.revert_state_gas(c.gas);
    for j in c.start..receipts.len().saturating_sub(1) {
        receipts[j].logs.clear();
        receipts[j].gas_used.state = 0;
        refunds[j] = 0;
        rt.state_gas_used[j] = 0;
        if clear_outputs {
            outputs[j] = Bytes::new();
        }
    }
}

/// Records a skipped frame in receipts and runtime bookkeeping and notifies the inspector.
fn skipped<T: EvmTypes>(
    host: &mut Evm<'_, T>,
    receipts: &mut Vec<FrameReceipt>,
    refunds: &mut Vec<i64>,
    outputs: &mut Vec<Bytes>,
) {
    let status = FrameStatus::SkippedAtomicBatch;
    receipts.push(FrameReceipt {
        status,
        gas_used: FrameGasUsed { execution: 0, state: 0 },
        logs: Vec::new(),
    });
    refunds.push(0);
    outputs.push(Bytes::new());
    let index = receipts.len() - 1;
    host.inspect_frame(|inspector, host| {
        inspector.frame_end(index, receipts.last().unwrap(), &Bytes::new(), InstrStop::Stop, host)
    });
    let rt = host.frame_tx.as_mut().unwrap();
    rt.statuses.push(status);
    rt.execution_gas_used.push(0);
    rt.state_gas_used.push(0);
}

/// Executes one frame with isolated execution and state gas budgets, returning its outcome.
fn execute_frame<T: EvmTypes>(
    host: &mut Evm<'_, T>,
    tx: &TxEip8141,
    frame: &Frame,
) -> HandlerResult<(InstrStop, u64, u64, i64, Bytes)> {
    let target = frame.resolved_target(tx.sender);
    let mut gas = GasTracker::new_isolated(frame.limits.execution, frame.limits.state);
    let run = (|| -> Result<MessageResultExt<T::MessageResultExt>, HostError> {
        let warm = host.version().gas_params.get(GasId::WarmStorageReadCost);
        let cold = host.version().gas_params.cold_account_additional_cost();
        gas.spend(warm)?;
        let load = host.load_account(&target, false, gas.remaining() < cold)?;
        if load.is_cold {
            gas.spend(cold)?;
        }
        if !frame.value.is_zero() && host.state.account(&tx.sender)?.balance() < frame.value {
            return Err(InstrStop::OutOfFunds.into());
        }
        if !frame.value.is_zero() && load.is_empty {
            gas.spend_state(host.version().gas_params.new_account_state_gas())?;
        }
        let precompile = host.precompiles().contains(&target);
        let mut code = if precompile {
            Bytecode::default()
        } else {
            host.state.account(&target)?.load_code()?
        };
        let mut code_address = target;
        if !precompile && let Some(delegate) = code.eip7702_address() {
            gas.spend(warm)?;
            let load = host.load_account(&delegate, true, gas.remaining() < cold)?;
            if load.is_cold {
                gas.spend(cold)?;
            }
            code = load.code;
            code_address = delegate;
        }
        let mut message = MessageExt {
            kind: if matches!(frame.mode, FrameMode::Verify | FrameMode::Assert) {
                MessageKind::StaticCall
            } else {
                MessageKind::Call
            },
            gas_limit: gas.remaining(),
            reservoir: gas.reservoir(),
            destination: target,
            call_target: target,
            caller: if frame.mode == FrameMode::Sender { tx.sender } else { ENTRY_POINT },
            depth: host.version().initial_call_depth,
            code,
            code_address,
            input: frame.data.clone().into(),
            value: frame.value,
            ..Message::<T>::default()
        };
        Ok(host.execute_message(&mut message)?)
    })();
    let (stop, state, refund, output) = match run {
        Ok(result) => {
            gas.set_remaining(result.gas.remaining());
            gas.set_reservoir(result.gas.reservoir());
            (
                result.stop,
                frame.limits.state.saturating_sub(result.gas.reservoir()),
                result.gas.refunded(),
                result.output,
            )
        }
        Err(HostError::Halt(stop)) => (stop, 0, 0, Bytes::new()),
        Err(HostError::Execution(error)) => return Err(error.into()),
    };
    Ok((
        stop,
        if stop.is_halt() && stop != InstrStop::OutOfFunds {
            frame.limits.execution
        } else {
            frame.limits.execution - gas.remaining()
        },
        if stop.is_success() { state } else { 0 },
        if stop.is_success() { refund } else { 0 },
        output,
    ))
}
