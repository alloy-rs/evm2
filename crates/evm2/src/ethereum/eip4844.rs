use super::{
    access_list_counts, charge_upfront, create_initial_state_gas, effective_gas_price, floor_gas,
    initial_gas_and_reservoir, initial_message, intrinsic_gas, rollback_failed_execution,
    settle_gas, validate_block_gas_limit, validate_chain_id, validate_create_initcode,
    validate_floor_gas, validate_gas_price, validate_intrinsic_gas, validate_nonce_not_overflow,
    validate_priority_fee, validate_regular_gas_limit_cap, validate_sender,
    validate_tx_gas_limit_cap, warm_access_list, warm_base_accounts,
};
use crate::{
    EvmTypes, TxResult,
    env::TxEnvExt,
    evm::error_handler,
    interpreter::Host,
    registry::{HandlerError, HandlerResult, TxRequest},
    utils::b256_to_word,
};
use alloy_consensus::transaction::TxEip4844Variant;
use alloy_eips::eip4844::{DATA_GAS_PER_BLOB, VERSIONED_HASH_VERSION_KZG};
use alloy_primitives::U256;

/// Executes an EIP-4844 transaction using Ethereum rules.
pub fn handle<T: EvmTypes>(
    req: TxRequest<'_, '_, T, TxEip4844Variant>,
) -> HandlerResult<TxResult<T>> {
    let caller = req.tx.signer();
    let tx = req.tx.inner().tx();
    let max_fee_per_gas = U256::from(tx.max_fee_per_gas);
    let max_priority_fee_per_gas = U256::from(tx.max_priority_fee_per_gas);
    let gas_price =
        effective_gas_price(max_fee_per_gas, max_priority_fee_per_gas, req.host.block.basefee);
    let max_fee_per_blob_gas = U256::from(tx.max_fee_per_blob_gas);

    validate_priority_fee(req.host.version(), max_fee_per_gas, max_priority_fee_per_gas)?;
    validate_gas_price(req.host.version(), gas_price, req.host.block.basefee)?;
    validate_chain_id(req.host.version(), Some(tx.chain_id), false)?;
    validate_blob_fee(max_fee_per_blob_gas, req.host.block.blob_basefee)?;
    validate_blobs(&tx.blob_versioned_hashes, req.host.version().max_blobs_per_tx)?;
    validate_tx_gas_limit_cap(req.host.version(), tx.gas_limit)?;
    validate_block_gas_limit(req.host.version(), tx.gas_limit, req.host.block.gas_limit)?;
    validate_create_initcode(req.host.version(), tx.to.into(), &tx.input)?;
    validate_nonce_not_overflow(tx.nonce)?;
    let (access_list_accounts, access_list_storage_keys) = access_list_counts(&tx.access_list);
    let intrinsic = intrinsic_gas(
        req.host.version(),
        caller,
        tx.to.into(),
        &tx.input,
        access_list_accounts,
        access_list_storage_keys,
        tx.value,
    );
    let initial_state_gas = create_initial_state_gas(req.host.version(), false);
    validate_intrinsic_gas(tx.gas_limit, intrinsic, initial_state_gas)?;
    let floor_gas =
        floor_gas(req.host.version(), &tx.input, access_list_accounts, access_list_storage_keys);
    validate_floor_gas(tx.gas_limit, floor_gas)?;
    validate_regular_gas_limit_cap(req.host.version(), tx.gas_limit, intrinsic, floor_gas)?;

    let blob_gas_cost = U256::from(DATA_GAS_PER_BLOB) * U256::from(tx.blob_versioned_hashes.len());
    let max_blob_gas_cost = blob_gas_cost * max_fee_per_blob_gas;
    let max_gas_cost = U256::from(tx.gas_limit) * max_fee_per_gas;
    validate_sender(
        req.host,
        caller,
        tx.nonce,
        max_gas_cost.saturating_add(max_blob_gas_cost).saturating_add(tx.value),
    )?;

    warm_base_accounts(req.host, caller, tx.to.into());
    warm_access_list(req.host, &tx.access_list);

    let effective_gas_cost = U256::from(tx.gas_limit) * gas_price;
    let blob_basefee_cost = blob_gas_cost * req.host.block.blob_basefee;
    charge_upfront(req.host, caller, effective_gas_cost + blob_basefee_cost)?;
    req.host.state.account(&caller, false).map_err(error_handler!(req.host))?.bump_nonce();
    let execution_checkpoint = req.host.state.checkpoint();

    // Blob transactions are always calls, never creates.
    let (gas_limit, reservoir) = initial_gas_and_reservoir(
        req.host.version(),
        tx.gas_limit,
        intrinsic,
        initial_state_gas,
        0,
    );
    let tx_env = TxEnvExt {
        origin: caller,
        gas_price,
        chain_id: U256::from(req.host.version().chain_id),
        blob_hashes: tx.blob_versioned_hashes.iter().copied().map(b256_to_word).collect(),
        ext: T::TxEnvExt::default(),
        _non_exhaustive: (),
    };
    let (bytecode, mut message) = initial_message(
        req.host,
        caller,
        tx.nonce,
        tx.to.into(),
        &tx.input,
        tx.value,
        gas_limit,
        reservoir,
    )?;
    let mut result = req.host.execute_message(&tx_env, bytecode, &mut message);
    rollback_failed_execution(req.host, execution_checkpoint, &mut result);

    settle_gas(
        req.host,
        caller,
        gas_price,
        tx.gas_limit,
        floor_gas,
        initial_state_gas,
        0,
        false,
        result,
    )
}

fn validate_blob_fee(max_fee_per_blob_gas: U256, blob_basefee: U256) -> HandlerResult<()> {
    if max_fee_per_blob_gas < blob_basefee {
        return Err(HandlerError::BlobFeeCapLessThanBlobBaseFee {
            max_fee_per_blob_gas,
            blob_base_fee: blob_basefee,
        });
    }
    Ok(())
}

fn validate_blobs(blobs: &[alloy_primitives::B256], max_blobs: usize) -> HandlerResult<()> {
    if blobs.is_empty() {
        return Err(HandlerError::EmptyBlobs);
    }
    if blobs.len() > max_blobs {
        return Err(HandlerError::TooManyBlobs { have: blobs.len(), max: max_blobs });
    }
    if blobs.iter().any(|blob| blob[0] != VERSIONED_HASH_VERSION_KZG) {
        return Err(HandlerError::BlobVersionNotSupported);
    }
    Ok(())
}
