use super::{
    access_list_counts, charge_upfront, effective_gas_price, floor_gas, initial_message,
    intrinsic_gas, rollback_failed_execution, settle_gas, validate_block_gas_limit,
    validate_chain_id, validate_create_initcode, validate_floor_gas, validate_gas_price,
    validate_intrinsic_gas, validate_nonce_not_overflow, validate_priority_fee,
    validate_regular_gas_limit_cap, validate_sender, validate_tx_gas_limit_cap, warm_access_list,
    warm_base_accounts,
};
use crate::{
    Evm, EvmTypes, TxResult,
    bytecode::Bytecode,
    env::TxEnv,
    interpreter::Host,
    registry::{HandlerError, HandlerResult, TxRequest},
    version::GasId,
};
use alloy_consensus::transaction::Recovered;
use alloy_primitives::{Address, U256};

pub(super) fn handle<T: EvmTypes<Host = Evm<T>>>(
    req: TxRequest<'_, T, Recovered<super::LazyTxEip7702>>,
) -> HandlerResult<TxResult<T>> {
    let caller = req.tx.signer();
    let tx = req.tx.inner();
    if tx.authorization_list.is_empty() {
        return Err(HandlerError::EmptyAuthorizationList);
    }
    let max_fee_per_gas = U256::from(tx.max_fee_per_gas);
    let max_priority_fee_per_gas = U256::from(tx.max_priority_fee_per_gas);
    let gas_price =
        effective_gas_price(max_fee_per_gas, max_priority_fee_per_gas, req.host.block.basefee);

    validate_priority_fee(req.host.version(), max_fee_per_gas, max_priority_fee_per_gas)?;
    validate_gas_price(req.host.version(), gas_price, req.host.block.basefee)?;
    validate_chain_id(req.host.version(), Some(tx.chain_id), false)?;
    validate_tx_gas_limit_cap(req.host.version(), tx.gas_limit)?;
    validate_block_gas_limit(req.host.version(), tx.gas_limit, req.host.block.gas_limit)?;
    validate_create_initcode(req.host.version(), tx.to.into(), &tx.input)?;
    validate_nonce_not_overflow(tx.nonce)?;
    let (access_list_accounts, access_list_storage_keys) = access_list_counts(&tx.access_list);
    let intrinsic = intrinsic_gas(
        req.host.version(),
        tx.to.into(),
        &tx.input,
        access_list_accounts,
        access_list_storage_keys,
    ) + eip7702_authorization_gas(req.host, tx.authorization_list.len());
    validate_intrinsic_gas(tx.gas_limit, intrinsic)?;
    let floor_gas =
        floor_gas(req.host.version(), &tx.input, access_list_accounts, access_list_storage_keys);
    validate_floor_gas(tx.gas_limit, floor_gas)?;
    validate_regular_gas_limit_cap(req.host.version(), tx.gas_limit, intrinsic, floor_gas)?;

    let max_gas_cost = U256::from(tx.gas_limit) * max_fee_per_gas;
    validate_sender(req.host, caller, tx.nonce, max_gas_cost.saturating_add(tx.value))?;

    warm_base_accounts(req.host, caller, tx.to.into());
    warm_access_list(req.host, &tx.access_list);

    let effective_gas_cost = U256::from(tx.gas_limit) * gas_price;
    charge_upfront(req.host, caller, effective_gas_cost)?;
    req.host.state.increment_nonce(&caller).map_err(|code| req.host.db_error_handler(code))?;
    let chain_id = req.host.version().chain_id;
    let eip7702_refund = apply_auth_list(req.host, chain_id, &tx.authorization_list)?;
    let execution_checkpoint = req.host.state.checkpoint();

    let gas_limit = tx.gas_limit - intrinsic;
    let tx_env = TxEnv {
        origin: caller,
        gas_price,
        chain_id: U256::from(req.host.version().chain_id),
        ..TxEnv::default()
    };
    let (bytecode, mut message) =
        initial_message(req.host, caller, tx.nonce, tx.to.into(), &tx.input, tx.value, gas_limit)?;
    let mut result = req.host.execute_message(&tx_env, bytecode, &mut message, false);
    rollback_failed_execution(req.host, execution_checkpoint, &mut result);
    result.gas.set_refunded(
        result.gas.refunded().saturating_add(i64::try_from(eip7702_refund).unwrap_or(i64::MAX)),
    );

    settle_gas(req.host, caller, gas_price, tx.gas_limit, floor_gas, result)
}

fn eip7702_authorization_gas<T: EvmTypes<Host = Evm<T>>>(
    host: &Evm<T>,
    authorizations: usize,
) -> u64 {
    let per_auth = u64::from(host.version().gas_params.get(GasId::TxEip7702PerEmptyAccountCost));
    u64::try_from(authorizations).unwrap_or(u64::MAX).saturating_mul(per_auth)
}

fn apply_auth_list<T: EvmTypes<Host = Evm<T>>>(
    host: &mut Evm<T>,
    chain_id: u64,
    authorizations: &[super::LazyAuthorization],
) -> HandlerResult<u64> {
    let mut refunded_accounts = 0u64;
    for authorization in authorizations {
        if !authorization.chain_id().is_zero() && authorization.chain_id() != &U256::from(chain_id)
        {
            continue;
        }
        if authorization.nonce() == u64::MAX {
            continue;
        }

        let Some(authority) = authorization.authority() else {
            continue;
        };
        host.state.warm_account_non_revertible(&authority);
        let authority_info =
            host.state.account_info(&authority).map_err(|code| host.db_error_handler(code))?;
        let existed = authority_info.is_some();
        let authority_info = authority_info.unwrap_or_default();
        let code = host.state.get_code(&authority).map_err(|code| host.db_error_handler(code))?;
        if !code.is_empty() && !code.is_eip7702() {
            continue;
        }
        if authorization.nonce() != authority_info.nonce {
            continue;
        }

        if existed {
            refunded_accounts = refunded_accounts.saturating_add(1);
        }
        set_delegation(host, authority, *authorization.address())?;
    }

    let refund_per_auth = u64::from(host.version().gas_params.get(GasId::TxEip7702AuthRefund));
    Ok(refunded_accounts.saturating_mul(refund_per_auth))
}

fn set_delegation<T: EvmTypes<Host = Evm<T>>>(
    host: &mut Evm<T>,
    authority: Address,
    delegated_address: Address,
) -> HandlerResult<()> {
    let code = if delegated_address.is_zero() {
        Bytecode::default()
    } else {
        Bytecode::new_eip7702(delegated_address)
    };
    host.state.set_code(&authority, code).map_err(|code| host.db_error_handler(code))?;
    host.state.increment_nonce(&authority).map_err(|code| host.db_error_handler(code))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BaseEvmTypes, Precompiles, SpecId,
        env::BlockEnv,
        ethereum::{LazyTxEip7702, RecoveredTxEnvelope},
        evm::{AccountInfo, InMemoryDB},
        registry::TxRegistry,
    };
    use alloc::vec;
    use alloy_consensus::TxEip7702;
    use alloy_eips::{
        eip2930::AccessList,
        eip7702::{Authorization, RecoveredAuthority, RecoveredAuthorization, SignedAuthorization},
    };
    use alloy_primitives::{Bytes, Signature};
    use k256::ecdsa::{
        RecoveryId, Signature as K256Signature, SigningKey, signature::hazmat::PrehashSigner,
    };

    const CHAIN_ID: u64 = 1;

    fn test_evm(database: InMemoryDB) -> Evm<BaseEvmTypes> {
        Evm::new(
            SpecId::PRAGUE,
            BlockEnv::default(),
            TxRegistry::new(),
            database,
            Precompiles::base(SpecId::PRAGUE),
        )
    }

    fn tx_with_authorizations(authorization_list: Vec<SignedAuthorization>) -> TxEip7702 {
        TxEip7702 {
            chain_id: CHAIN_ID,
            nonce: 0,
            gas_limit: 100_000,
            max_fee_per_gas: 1,
            max_priority_fee_per_gas: 0,
            to: Address::with_last_byte(0xbb),
            value: U256::ZERO,
            access_list: AccessList::default(),
            authorization_list,
            input: Bytes::new(),
        }
    }

    fn signed_authorization(delegated: Address, nonce: u64) -> SignedAuthorization {
        sign_authorization(Authorization {
            chain_id: U256::from(CHAIN_ID),
            address: delegated,
            nonce,
        })
    }

    fn sign_authorization(authorization: Authorization) -> SignedAuthorization {
        let signing_key = SigningKey::from_slice(&[0x77; 32])
            .expect("hard-coded EIP-7702 test signing key should be valid");
        let (signature, recovery_id): (K256Signature, RecoveryId) = signing_key
            .sign_prehash(authorization.signature_hash().as_ref())
            .expect("signing a fixed EIP-7702 authorization prehash should succeed");
        let signature = Signature::new(
            U256::from_be_slice(signature.r().to_bytes().as_ref()),
            U256::from_be_slice(signature.s().to_bytes().as_ref()),
            recovery_id.is_y_odd(),
        );
        authorization.into_signed(signature)
    }

    fn assert_delegation(
        evm: &mut Evm<BaseEvmTypes>,
        authority: Address,
        delegated: Address,
        nonce: u64,
    ) {
        let code = evm.state.get_code(&authority).expect("authority code lookup should succeed");
        assert_eq!(code.eip7702_address(), Some(delegated));
        let info = evm
            .state
            .account_info(&authority)
            .expect("authority account lookup should succeed")
            .expect("authority account should exist after applying authorization");
        assert_eq!(info.nonce, nonce);
    }

    #[test]
    fn signed_auth_list_still_recovers_lazily() {
        let delegated = Address::with_last_byte(0x42);
        let signed = signed_authorization(delegated, 0);
        let authority = signed.recover_authority().expect("test authorization should recover");
        let mut database = InMemoryDB::default();
        database.insert_account_info(&authority, AccountInfo::default());

        super::super::reset_authority_recovery_attempts();
        let tx = LazyTxEip7702::from(tx_with_authorizations(vec![signed]));
        assert!(tx.authorization_list[0].as_signed().is_some());
        assert_eq!(super::super::authority_recovery_attempts(), 0);

        let mut evm = test_evm(database);
        let refund = apply_auth_list(&mut evm, CHAIN_ID, &tx.authorization_list)
            .expect("authorization list application should succeed");

        assert_eq!(super::super::authority_recovery_attempts(), 1);
        assert!(refund > 0);
        assert_delegation(&mut evm, authority, delegated, 1);
    }

    #[test]
    fn recovered_auth_list_avoids_recovery_during_apply() {
        let delegated = Address::with_last_byte(0x43);
        let signed = signed_authorization(delegated, 0);
        let authority = signed.recover_authority().expect("test authorization should recover");
        let mut database = InMemoryDB::default();
        database.insert_account_info(&authority, AccountInfo::default());
        let tx = LazyTxEip7702::from_recovered_authorizations(tx_with_authorizations(vec![signed]));
        assert!(tx.authorization_list[0].as_recovered().is_some());

        super::super::reset_authority_recovery_attempts();
        let mut evm = test_evm(database);
        let refund = apply_auth_list(&mut evm, CHAIN_ID, &tx.authorization_list)
            .expect("authorization list application should succeed");

        assert_eq!(super::super::authority_recovery_attempts(), 0);
        assert!(refund > 0);
        assert_delegation(&mut evm, authority, delegated, 1);
    }

    #[test]
    fn invalid_recovered_authorization_matches_failed_recovery() {
        let delegated = Address::with_last_byte(0x44);
        let inner = Authorization { chain_id: U256::from(CHAIN_ID), address: delegated, nonce: 0 };
        let bad_signed = SignedAuthorization::new_unchecked(inner, 2, U256::ONE, U256::ONE);
        let signed_tx = LazyTxEip7702::from(tx_with_authorizations(vec![bad_signed.clone()]));
        let recovered_tx =
            LazyTxEip7702::from_recovered_authorizations(tx_with_authorizations(vec![bad_signed]));
        assert!(
            recovered_tx.authorization_list[0]
                .as_recovered()
                .expect("eager recovery should produce a recovered authorization")
                .authority()
                .is_none()
        );

        super::super::reset_authority_recovery_attempts();
        let mut signed_evm = test_evm(InMemoryDB::default());
        let signed_refund =
            apply_auth_list(&mut signed_evm, CHAIN_ID, &signed_tx.authorization_list)
                .expect("signed invalid authorization should be skipped");
        assert_eq!(super::super::authority_recovery_attempts(), 1);

        super::super::reset_authority_recovery_attempts();
        let mut recovered_evm = test_evm(InMemoryDB::default());
        let recovered_refund =
            apply_auth_list(&mut recovered_evm, CHAIN_ID, &recovered_tx.authorization_list)
                .expect("recovered invalid authorization should be skipped");
        assert_eq!(super::super::authority_recovery_attempts(), 0);

        assert_eq!(signed_refund, recovered_refund);
        assert_eq!(signed_refund, 0);
        assert!(
            signed_evm
                .state
                .get_code(&delegated)
                .expect("delegated code lookup should succeed")
                .is_empty()
        );
        assert!(
            recovered_evm
                .state
                .get_code(&delegated)
                .expect("delegated code lookup should succeed")
                .is_empty()
        );
    }

    #[test]
    fn recovered_tx_envelope_converts_normal_eip7702_transaction() {
        let caller = Address::with_last_byte(0xaa);
        let signed = signed_authorization(Address::with_last_byte(0x45), 0);
        let envelope = RecoveredTxEnvelope::from(Recovered::new_unchecked(
            tx_with_authorizations(vec![signed.clone()]),
            caller,
        ));

        let tx = envelope.as_eip7702().expect("envelope should contain an EIP-7702 transaction");
        assert_eq!(tx.signer(), caller);
        assert_eq!(tx.authorization_list.len(), 1);
        assert_eq!(tx.authorization_list[0].as_signed(), Some(&signed));
    }

    #[test]
    fn invalid_cached_recovered_authorization_is_skipped() {
        let delegated = Address::with_last_byte(0x46);
        let authority = Address::with_last_byte(0x47);
        let authorization =
            Authorization { chain_id: U256::from(CHAIN_ID), address: delegated, nonce: 0 };
        let recovered =
            RecoveredAuthorization::new_unchecked(authorization, RecoveredAuthority::Invalid);
        let tx = LazyTxEip7702::from_cached_recovered_authorizations(
            tx_with_authorizations(Vec::new()),
            vec![recovered],
        );
        let mut database = InMemoryDB::default();
        database.insert_account_info(&authority, AccountInfo::default());

        super::super::reset_authority_recovery_attempts();
        let mut evm = test_evm(database);
        let refund = apply_auth_list(&mut evm, CHAIN_ID, &tx.authorization_list)
            .expect("invalid cached recovered authorization should be skipped");

        assert_eq!(super::super::authority_recovery_attempts(), 0);
        assert_eq!(refund, 0);
        assert!(
            evm.state
                .get_code(&authority)
                .expect("authority code lookup should succeed")
                .is_empty()
        );
    }
}
