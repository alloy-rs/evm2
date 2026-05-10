use alloy_consensus::{TypedTransaction, transaction::Recovered};
use alloy_eips::eip7702::SignedAuthorization;
use alloy_primitives::{Address, B256, Bytes, TxKind, U256};
use alloy_rpc_types_eth::{
    AccessList as RpcAccessList, AccessListItem as RpcAccessListItem, TransactionInput,
    TransactionRequest,
};
use evm2::ethereum::RecoveredTxEnvelope;
use k256::ecdsa::SigningKey;
use serde::{Deserialize, Deserializer};

/// Access list entry shared by EEST fixture formats.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AccessListItem {
    /// Accessed account.
    pub(crate) address: Address,
    /// Accessed storage keys.
    pub(crate) storage_keys: Vec<B256>,
}

/// EIP-7702 authorization entry shared by EEST fixture formats.
#[derive(Clone, Debug)]
pub(crate) struct TestAuthorization {
    /// Raw authorization JSON.
    pub(crate) value: serde_json::Value,
}

impl<'de> Deserialize<'de> for TestAuthorization {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let mut value = serde_json::Value::deserialize(deserializer)?;
        if let Some(object) = value.as_object_mut()
            && object.contains_key("v")
            && object.contains_key("yParity")
        {
            object.remove("v");
        }
        Ok(Self { value })
    }
}

/// Error while building a consensus transaction from fixture fields.
#[derive(Debug, thiserror::Error)]
pub(crate) enum TxBuildError {
    /// Numeric value overflowed the target type.
    #[error("value overflows {0}")]
    Overflow(&'static str),
    /// Transaction request could not be converted to a consensus transaction.
    #[error("could not build consensus transaction: {0}")]
    BuildTransaction(String),
    /// JSON decoding failed.
    #[error(transparent)]
    SerdeDeserialize(#[from] serde_json::Error),
}

/// Common transaction fields already selected from a fixture.
pub(crate) struct TxFields {
    /// Explicit transaction type.
    pub(crate) tx_type: Option<u8>,
    /// Recovered or explicit sender.
    pub(crate) caller: Address,
    /// Transaction recipient, or create.
    pub(crate) kind: TxKind,
    /// Transaction calldata.
    pub(crate) data: Bytes,
    /// Gas limit.
    pub(crate) gas_limit: u64,
    /// Transaction nonce.
    pub(crate) nonce: u64,
    /// Ether value.
    pub(crate) value: U256,
    /// Optional chain ID.
    pub(crate) chain_id: Option<U256>,
    /// Legacy gas price.
    pub(crate) gas_price: Option<U256>,
    /// EIP-1559 max fee.
    pub(crate) max_fee_per_gas: Option<U256>,
    /// EIP-1559 priority fee.
    pub(crate) max_priority_fee_per_gas: Option<U256>,
    /// EIP-2930 access list.
    pub(crate) access_list: Option<RpcAccessList>,
    /// EIP-7702 authorization list.
    pub(crate) authorization_list: Option<Vec<SignedAuthorization>>,
    /// EIP-4844 blob hashes.
    pub(crate) blob_versioned_hashes: Vec<B256>,
    /// EIP-4844 max fee per blob gas.
    pub(crate) max_fee_per_blob_gas: Option<U256>,
}

/// Builds an evm2 recovered transaction envelope from common fixture fields.
pub(crate) fn build_recovered_tx(fields: TxFields) -> Result<RecoveredTxEnvelope, TxBuildError> {
    let mut request = TransactionRequest::default()
        .from(fields.caller)
        .gas_limit(fields.gas_limit)
        .nonce(fields.nonce)
        .value(fields.value)
        .input(TransactionInput::from(fields.data));
    request.to = Some(fields.kind);
    request.transaction_type = fields.tx_type;
    request.chain_id = fields
        .chain_id
        .map(TryInto::try_into)
        .transpose()
        .map_err(|_| TxBuildError::Overflow("chainId"))?;
    if !matches!(fields.tx_type, Some(2..=4)) {
        request.gas_price = fields
            .gas_price
            .map(TryInto::try_into)
            .transpose()
            .map_err(|_| TxBuildError::Overflow("gasPrice"))?;
        if request.gas_price.is_none()
            && (matches!(fields.tx_type, Some(0 | 1))
                || (fields.max_fee_per_gas.is_none() && fields.max_priority_fee_per_gas.is_none()))
        {
            request.gas_price = Some(0);
        }
    }
    request.max_fee_per_gas = fields
        .max_fee_per_gas
        .map(TryInto::try_into)
        .transpose()
        .map_err(|_| TxBuildError::Overflow("maxFeePerGas"))?;
    request.max_priority_fee_per_gas =
        if fields.max_fee_per_gas.is_some() && fields.max_priority_fee_per_gas.is_none() {
            Some(0)
        } else {
            fields
                .max_priority_fee_per_gas
                .map(TryInto::try_into)
                .transpose()
                .map_err(|_| TxBuildError::Overflow("maxPriorityFeePerGas"))?
        };
    request.max_fee_per_blob_gas = fields
        .max_fee_per_blob_gas
        .map(TryInto::try_into)
        .transpose()
        .map_err(|_| TxBuildError::Overflow("maxFeePerBlobGas"))?;
    request.access_list = fields.access_list;
    request.authorization_list = fields.authorization_list;
    if fields.max_fee_per_blob_gas.is_some()
        || matches!(fields.tx_type, Some(3))
        || !fields.blob_versioned_hashes.is_empty()
    {
        request.blob_versioned_hashes = Some(fields.blob_versioned_hashes);
    }

    let tx =
        request.build_consensus_tx().map_err(|err| TxBuildError::BuildTransaction(err.error))?;
    Ok(recovered_envelope(tx, fields.caller))
}

/// Converts EEST access list items into RPC access list items.
pub(crate) fn rpc_access_list<'a>(
    items: impl IntoIterator<Item = &'a AccessListItem>,
) -> RpcAccessList {
    RpcAccessList(items.into_iter().map(rpc_access_list_item).collect())
}

fn rpc_access_list_item(item: &AccessListItem) -> RpcAccessListItem {
    RpcAccessListItem { address: item.address, storage_keys: item.storage_keys.clone() }
}

/// Deserializes signed authorizations from their raw fixture JSON form.
pub(crate) fn signed_authorizations(
    raw: Option<&[TestAuthorization]>,
) -> Result<Option<Vec<SignedAuthorization>>, TxBuildError> {
    let Some(authorizations) = raw else {
        return Ok(None);
    };
    let authorizations = authorizations
        .iter()
        .map(|authorization| serde_json::from_value(authorization.value.clone()))
        .collect::<Result<_, _>>()?;
    Ok(Some(authorizations))
}

/// Recovers an address from a private key.
pub(crate) fn recover_address(private_key: &[u8]) -> Option<Address> {
    let key = SigningKey::from_slice(private_key).ok()?;
    let public_key = key.verifying_key().to_encoded_point(false);
    Some(Address::from_raw_public_key(&public_key.as_bytes()[1..]))
}

fn recovered_envelope(tx: TypedTransaction, caller: Address) -> RecoveredTxEnvelope {
    match tx {
        TypedTransaction::Legacy(tx) => {
            RecoveredTxEnvelope::Legacy(Recovered::new_unchecked(tx, caller))
        }
        TypedTransaction::Eip2930(tx) => {
            RecoveredTxEnvelope::Eip2930(Recovered::new_unchecked(tx, caller))
        }
        TypedTransaction::Eip1559(tx) => {
            RecoveredTxEnvelope::Eip1559(Recovered::new_unchecked(tx, caller))
        }
        TypedTransaction::Eip4844(tx) => {
            RecoveredTxEnvelope::Eip4844(Recovered::new_unchecked(tx, caller))
        }
        TypedTransaction::Eip7702(tx) => {
            RecoveredTxEnvelope::Eip7702(Recovered::new_unchecked(tx, caller))
        }
    }
}
