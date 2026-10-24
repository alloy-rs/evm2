use super::{CallFees, CallFeesError};
use crate::{
    Version,
    env::BlockEnvExt,
    ethereum::{RecoveredTxEnvelope, TxEnvelope},
};
use alloc::vec::Vec;
use alloy_consensus::{
    TxEip1559, TxEip2930, TxEip4844, TxEip7702, TxLegacy, TxType,
    transaction::{Recovered, TxEip4844Variant},
};
use alloy_primitives::{Address, TxKind, U256};
use alloy_rpc_types_eth::{
    TransactionInput,
    request::{TransactionInputError, TransactionRequest},
};

/// Converts `self` into `T`.
///
/// Should create an executable transaction using [`TransactionRequest`].
pub trait TryIntoTxEnv<T, E = ()> {
    /// An associated error that can occur during the conversion.
    type Err;

    /// Performs the conversion against the active EVM version and block environment.
    fn try_into_tx_env(self, version: &Version, block_env: &BlockEnvExt<E>)
    -> Result<T, Self::Err>;
}

/// An Ethereum specific transaction error that can occur during conversion from
/// [`TransactionRequest`].
#[derive(Debug, thiserror::Error)]
pub enum EthTxEnvError {
    /// Error while decoding or validating transaction request fees.
    #[error(transparent)]
    CallFees(#[from] CallFeesError),
    /// Both data and input fields are set and not equal.
    #[error(transparent)]
    Input(#[from] TransactionInputError),
    /// An EIP-4844 transaction has no call target.
    #[error("blob transaction is a create transaction")]
    BlobTransactionIsCreate,
    /// An EIP-7702 transaction has no call target.
    #[error("EIP-7702 transaction is a create transaction")]
    AuthorizationListInvalidFields,
}

impl<E> TryIntoTxEnv<RecoveredTxEnvelope, E> for TransactionRequest {
    type Err = EthTxEnvError;

    fn try_into_tx_env(
        mut self,
        version: &Version,
        block_env: &BlockEnvExt<E>,
    ) -> Result<RecoveredTxEnvelope, Self::Err> {
        let tx_type = self.minimal_tx_type();
        let CallFees { max_priority_fee_per_gas, gas_price, max_fee_per_blob_gas } =
            normalize_transaction_request(&mut self, version, block_env)?;
        let Self {
            from,
            to,
            gas_price: _,
            max_fee_per_gas: _,
            max_priority_fee_per_gas: _,
            gas,
            value,
            input,
            nonce,
            access_list,
            chain_id,
            blob_versioned_hashes,
            max_fee_per_blob_gas: _,
            authorization_list,
            transaction_type: _,
            sidecar: _,
        } = self;
        let input = input.into_input().unwrap_or_default();
        let caller = from.unwrap_or_default();
        let to = to.unwrap_or(TxKind::Create);
        let gas_limit = gas.unwrap_or_default();
        let nonce = nonce.unwrap_or_default();
        let chain_id = chain_id.unwrap_or_default();
        let value = value.unwrap_or_default();
        let access_list = access_list.unwrap_or_default();
        let gas_price = gas_price.saturating_to();
        // Preserve flat pricing when the RPC request has no priority fee.
        let max_priority_fee_per_gas =
            max_priority_fee_per_gas.map(|fee| fee.saturating_to()).unwrap_or(gas_price);

        let tx = match tx_type {
            TxType::Legacy => TxEnvelope::Legacy(TxLegacy {
                chain_id: Some(chain_id),
                nonce,
                gas_price,
                gas_limit,
                to,
                value,
                input,
            }),
            TxType::Eip2930 => TxEnvelope::Eip2930(TxEip2930 {
                chain_id,
                nonce,
                gas_price,
                gas_limit,
                to,
                value,
                access_list,
                input,
            }),
            TxType::Eip1559 => TxEnvelope::Eip1559(TxEip1559 {
                chain_id,
                nonce,
                gas_limit,
                max_fee_per_gas: gas_price,
                max_priority_fee_per_gas,
                to,
                value,
                access_list,
                input,
            }),
            TxType::Eip4844 => TxEnvelope::Eip4844(TxEip4844Variant::TxEip4844(TxEip4844 {
                chain_id,
                nonce,
                gas_limit,
                max_fee_per_gas: gas_price,
                max_priority_fee_per_gas,
                to: to.to().copied().ok_or(EthTxEnvError::BlobTransactionIsCreate)?,
                value,
                access_list,
                blob_versioned_hashes: blob_versioned_hashes.unwrap_or_default(),
                max_fee_per_blob_gas: max_fee_per_blob_gas.unwrap_or_default().saturating_to(),
                input,
            })),
            TxType::Eip7702 => TxEnvelope::Eip7702(
                TxEip7702 {
                    chain_id,
                    nonce,
                    gas_limit,
                    max_fee_per_gas: gas_price,
                    max_priority_fee_per_gas,
                    to: to.to().copied().ok_or(EthTxEnvError::AuthorizationListInvalidFields)?,
                    value,
                    access_list,
                    authorization_list: authorization_list.unwrap_or_default(),
                    input,
                }
                .into(),
            ),
        };
        Ok(Recovered::new_unchecked(tx, caller))
    }
}

/// Fills missing fields for simulation using the selected EVM version and block environment.
///
/// Explicit fee caps and flat `gasPrice` values are preserved. The returned fees describe
/// execution pricing, which may be lower than the request's maximum fee cap. No state lookup,
/// gas estimation, or fee suggestion is performed; callers should resolve the account nonce
/// before calling this helper when it is needed.
///
/// `to` is left unchanged so callers with their own call representation can use this helper.
/// Recover signatures from the original request before filling defaults.
pub fn normalize_transaction_request<E>(
    request: &mut TransactionRequest,
    version: &Version,
    block_env: &BlockEnvExt<E>,
) -> Result<CallFees, EthTxEnvError> {
    if request.blob_versioned_hashes.as_ref().is_some_and(Vec::is_empty) {
        return Err(CallFeesError::BlobTransactionMissingBlobHashes.into());
    }
    let input = request.input.clone().try_into_unique_input()?;
    let fees = CallFees::ensure_fees(
        request.gas_price.map(U256::from),
        request.max_fee_per_gas.map(U256::from),
        request.max_priority_fee_per_gas.map(U256::from),
        block_env.basefee,
        request.blob_versioned_hashes.as_deref(),
        request.max_fee_per_blob_gas.map(U256::from),
        Some(block_env.blob_basefee),
    )?;

    if request.gas_price.is_none() {
        if matches!(request.minimal_tx_type(), TxType::Legacy | TxType::Eip2930) {
            request.gas_price = Some(fees.gas_price.saturating_to());
        } else {
            request.max_fee_per_gas.get_or_insert_with(|| fees.gas_price.saturating_to());
            request.max_priority_fee_per_gas.get_or_insert(0);
        }
    }
    if let Some(blob_fee) = fees.max_fee_per_blob_gas {
        request.max_fee_per_blob_gas.get_or_insert_with(|| blob_fee.saturating_to());
    }
    request.from.get_or_insert(Address::ZERO);
    request.nonce.get_or_insert(0);
    // Use the block gas limit like Erigon and Geth use their pre-configured gas cap, even when the
    // request leaves the gas limit unset.
    request.gas.get_or_insert_with(|| block_env.gas_limit.saturating_to());
    request.chain_id.get_or_insert(version.chain_id);
    request.value.get_or_insert(U256::ZERO);
    request.input = TransactionInput::new(input.unwrap_or_default());
    Ok(fees)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SpecId;
    use alloy_consensus::Transaction;
    use alloy_eips::{
        eip2930::{AccessList, AccessListItem},
        eip7702::{Authorization, SignedAuthorization},
    };
    use alloy_primitives::{B256, Bytes, Signature, address};

    const BASE_FEE: u64 = 15;
    const BLOB_BASE_FEE: u64 = 3;
    const BLOCK_GAS_LIMIT: u64 = 30_000_000;
    const CHAIN_ID: u64 = 5;

    fn env() -> (Version, BlockEnvExt<()>) {
        let mut version = Version::new(SpecId::PRAGUE);
        version.chain_id = CHAIN_ID;
        let block_env = BlockEnvExt {
            basefee: U256::from(BASE_FEE),
            blob_basefee: U256::from(BLOB_BASE_FEE),
            gas_limit: U256::from(BLOCK_GAS_LIMIT),
            ..Default::default()
        };
        (version, block_env)
    }

    fn convert(request: TransactionRequest) -> Result<RecoveredTxEnvelope, EthTxEnvError> {
        let (version, block_env) = env();
        request.try_into_tx_env(&version, &block_env)
    }

    fn call(to: Address) -> TransactionRequest {
        TransactionRequest { to: Some(to.into()), ..Default::default() }
    }

    fn access_list() -> AccessList {
        AccessList(vec![AccessListItem {
            address: Address::with_last_byte(9),
            storage_keys: vec![B256::with_last_byte(1)],
        }])
    }

    fn authorization() -> SignedAuthorization {
        Authorization { chain_id: U256::ONE, address: Address::with_last_byte(8), nonce: 4 }
            .into_signed(Signature::test_signature())
    }

    #[test]
    fn empty_request_is_a_zero_price_legacy_create() {
        let tx = convert(TransactionRequest::default()).unwrap();

        assert_eq!(tx.signer(), Address::ZERO);
        let TxEnvelope::Legacy(tx) = tx.inner() else { panic!("expected legacy: {tx:?}") };
        assert_eq!(tx.chain_id, Some(CHAIN_ID));
        assert_eq!(tx.nonce, 0);
        assert_eq!(tx.gas_price, 0);
        assert_eq!(tx.gas_limit, BLOCK_GAS_LIMIT);
        assert_eq!(tx.to, TxKind::Create);
        assert_eq!(tx.value, U256::ZERO);
        assert_eq!(tx.input, Bytes::new());
    }

    #[test]
    fn legacy_request_keeps_explicit_fields() {
        let from = address!("0x1111111111111111111111111111111111111111");
        let to = address!("0x2222222222222222222222222222222222222222");
        let request = TransactionRequest {
            from: Some(from),
            gas_price: Some(7),
            gas: Some(21_000),
            value: Some(U256::from(2)),
            input: Bytes::from_static(&[1, 2]).into(),
            nonce: Some(3),
            chain_id: Some(1),
            ..call(to)
        };
        let tx = convert(request).unwrap();

        assert_eq!(tx.signer(), from);
        let TxEnvelope::Legacy(tx) = tx.inner() else { panic!("expected legacy: {tx:?}") };
        assert_eq!(tx.chain_id, Some(1));
        assert_eq!(tx.nonce, 3);
        // A flat gas price is not checked against the block base fee.
        assert_eq!(tx.gas_price, 7);
        assert_eq!(tx.gas_limit, 21_000);
        assert_eq!(tx.to, TxKind::Call(to));
        assert_eq!(tx.value, U256::from(2));
        assert_eq!(tx.input, Bytes::from_static(&[1, 2]));
    }

    #[test]
    fn access_list_request_is_eip2930() {
        let request = TransactionRequest {
            gas_price: Some(7),
            access_list: Some(access_list()),
            ..call(Address::ZERO)
        };
        let tx = convert(request).unwrap();

        let TxEnvelope::Eip2930(tx) = tx.inner() else { panic!("expected 2930: {tx:?}") };
        assert_eq!(tx.chain_id, CHAIN_ID);
        assert_eq!(tx.gas_price, 7);
        assert_eq!(tx.access_list, access_list());
    }

    #[test]
    fn eip1559_request_prices_at_effective_gas_price() {
        // The effective price is min(maxFee, baseFee + tip) = min(25, 15 + 5).
        let request = TransactionRequest {
            max_fee_per_gas: Some(25),
            max_priority_fee_per_gas: Some(5),
            ..call(Address::ZERO)
        };
        let tx = convert(request).unwrap();

        let TxEnvelope::Eip1559(tx) = tx.inner() else { panic!("expected 1559: {tx:?}") };
        assert_eq!(tx.max_fee_per_gas, 20);
        assert_eq!(tx.max_priority_fee_per_gas, 5);

        // Without a tip the price is the base fee, charged flat.
        let request = TransactionRequest { max_fee_per_gas: Some(25), ..call(Address::ZERO) };
        let tx = convert(request).unwrap();
        let TxEnvelope::Eip1559(tx) = tx.inner() else { panic!("expected 1559: {tx:?}") };
        assert_eq!(tx.max_fee_per_gas, BASE_FEE as u128);
        assert_eq!(tx.max_priority_fee_per_gas, BASE_FEE as u128);

        // Zero fee caps bypass the base fee check.
        let request = TransactionRequest {
            max_fee_per_gas: Some(0),
            max_priority_fee_per_gas: Some(0),
            ..call(Address::ZERO)
        };
        let tx = convert(request).unwrap();
        assert_eq!(tx.inner().max_fee_per_gas(), 0);
    }

    #[test]
    fn fee_errors_are_reported() {
        let conflicting = TransactionRequest {
            gas_price: Some(1),
            max_fee_per_gas: Some(2),
            ..call(Address::ZERO)
        };
        assert!(matches!(
            convert(conflicting),
            Err(EthTxEnvError::CallFees(CallFeesError::ConflictingFeeFieldsInRequest))
        ));

        let fee_cap_too_low = TransactionRequest {
            max_fee_per_gas: Some(BASE_FEE as u128 - 1),
            ..call(Address::ZERO)
        };
        assert!(matches!(
            convert(fee_cap_too_low),
            Err(EthTxEnvError::CallFees(CallFeesError::FeeCapTooLow))
        ));

        let tip_above_fee_cap = TransactionRequest {
            max_fee_per_gas: Some(30),
            max_priority_fee_per_gas: Some(31),
            ..call(Address::ZERO)
        };
        assert!(matches!(
            convert(tip_above_fee_cap),
            Err(EthTxEnvError::CallFees(CallFeesError::TipAboveFeeCap))
        ));
    }

    #[test]
    fn conflicting_input_and_data_is_rejected() {
        let request = TransactionRequest {
            input: TransactionInput {
                input: Some(Bytes::from_static(&[1])),
                data: Some(Bytes::from_static(&[2])),
            },
            ..call(Address::ZERO)
        };
        assert!(matches!(convert(request), Err(EthTxEnvError::Input(_))));

        let data = Bytes::from_static(&[1]);
        let request = TransactionRequest {
            input: TransactionInput { input: None, data: Some(data.clone()) },
            ..call(Address::ZERO)
        };
        let tx = convert(request).unwrap();
        assert_eq!(*tx.inner().input(), data);
    }

    #[test]
    fn blob_request_is_eip4844() {
        let hash = B256::with_last_byte(1);
        let request = TransactionRequest {
            max_fee_per_gas: Some(25),
            max_priority_fee_per_gas: Some(5),
            blob_versioned_hashes: Some(vec![hash]),
            max_fee_per_blob_gas: Some(100),
            ..call(Address::ZERO)
        };
        let tx = convert(request).unwrap();

        let TxEnvelope::Eip4844(TxEip4844Variant::TxEip4844(tx)) = tx.inner() else {
            panic!("expected 4844: {tx:?}")
        };
        assert_eq!(tx.max_fee_per_gas, 20);
        assert_eq!(tx.max_priority_fee_per_gas, 5);
        assert_eq!(tx.max_fee_per_blob_gas, 100);
        assert_eq!(tx.blob_versioned_hashes, [hash]);

        // The block blob base fee is the default maximum blob fee.
        let request =
            TransactionRequest { blob_versioned_hashes: Some(vec![hash]), ..call(Address::ZERO) };
        let tx = convert(request).unwrap();
        let TxEnvelope::Eip4844(TxEip4844Variant::TxEip4844(tx)) = tx.inner() else {
            panic!("expected 4844: {tx:?}")
        };
        assert_eq!(tx.max_fee_per_blob_gas, BLOB_BASE_FEE as u128);
    }

    #[test]
    fn blob_request_errors() {
        let hash = B256::with_last_byte(1);

        let create =
            TransactionRequest { blob_versioned_hashes: Some(vec![hash]), ..Default::default() };
        assert!(matches!(convert(create), Err(EthTxEnvError::BlobTransactionIsCreate)));

        let no_hashes =
            TransactionRequest { blob_versioned_hashes: Some(vec![]), ..call(Address::ZERO) };
        assert!(matches!(
            convert(no_hashes),
            Err(EthTxEnvError::CallFees(CallFeesError::BlobTransactionMissingBlobHashes))
        ));

        let blob_fee_without_hashes =
            TransactionRequest { max_fee_per_blob_gas: Some(1), ..call(Address::ZERO) };
        assert!(matches!(
            convert(blob_fee_without_hashes),
            Err(EthTxEnvError::CallFees(CallFeesError::BlobTransactionMissingBlobHashes))
        ));
    }

    #[test]
    fn authorization_request_is_eip7702() {
        let to = Address::with_last_byte(7);
        let request = TransactionRequest {
            max_fee_per_gas: Some(25),
            max_priority_fee_per_gas: Some(5),
            authorization_list: Some(vec![authorization()]),
            ..call(to)
        };
        let tx = convert(request).unwrap();

        let TxEnvelope::Eip7702(tx) = tx.inner() else { panic!("expected 7702: {tx:?}") };
        assert_eq!(tx.max_fee_per_gas(), 20);
        assert_eq!(tx.max_priority_fee_per_gas(), Some(5));
        assert_eq!(tx.to(), Some(to));
        assert_eq!(tx.authorization_list().unwrap().len(), 1);
    }

    #[test]
    fn authorization_create_is_rejected() {
        let request = TransactionRequest {
            authorization_list: Some(vec![authorization()]),
            ..Default::default()
        };
        assert!(matches!(convert(request), Err(EthTxEnvError::AuthorizationListInvalidFields)));
    }

    #[test]
    fn normalize_fills_defaults_and_returns_fees() {
        let (version, block_env) = env();

        let mut request = TransactionRequest { max_fee_per_gas: Some(25), ..call(Address::ZERO) };
        let fees = normalize_transaction_request(&mut request, &version, &block_env).unwrap();

        assert_eq!(fees.gas_price, U256::from(BASE_FEE));
        assert_eq!(request.from, Some(Address::ZERO));
        assert_eq!(request.nonce, Some(0));
        assert_eq!(request.gas, Some(BLOCK_GAS_LIMIT));
        assert_eq!(request.chain_id, Some(CHAIN_ID));
        assert_eq!(request.value, Some(U256::ZERO));
        assert_eq!(request.max_fee_per_gas, Some(25));
        assert_eq!(request.max_priority_fee_per_gas, Some(0));
        assert_eq!(request.gas_price, None);

        let mut request = TransactionRequest::default();
        normalize_transaction_request(&mut request, &version, &block_env).unwrap();
        assert_eq!(request.gas_price, Some(0));
        assert_eq!(request.to, None);
    }
}
