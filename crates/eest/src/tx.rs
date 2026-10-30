use alloy_consensus::{
    EthereumTxEnvelope, SignableTransaction, TxEip4844, TypedTransaction,
    crypto::{RecoveryError, secp256k1},
    transaction::Recovered,
};
use alloy_eips::{
    eip2718::{Decodable2718, Eip2718Error},
    eip7702::SignedAuthorization,
};
use alloy_primitives::{Address, B256, Bytes, Signature, TxKind, U256};
use alloy_rpc_types_eth::{
    AccessList as RpcAccessList, AccessListItem as RpcAccessListItem, TransactionInput,
    TransactionRequest,
};
use evm2::{
    SpecId,
    ethereum::{RecoveredTxEnvelope, TxEnvelope},
};
use k256::ecdsa::SigningKey;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Access list entry shared by EEST fixture formats.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccessListItem {
    /// Accessed account.
    pub address: Address,
    /// Accessed storage keys.
    pub storage_keys: Vec<B256>,
}

/// EIP-7702 authorization entry shared by EEST fixture formats.
#[derive(Clone, Debug)]
pub struct TestAuthorization {
    /// Raw authorization JSON.
    pub value: serde_json::Value,
}

impl Serialize for TestAuthorization {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.value.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for TestAuthorization {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let mut value = serde_json::Value::deserialize(deserializer)?;
        normalize_authorization_value(&mut value);
        Ok(Self { value })
    }
}

fn normalize_authorization_value(value: &mut serde_json::Value) {
    if let Some(object) = value.as_object_mut()
        && object.contains_key("v")
        && object.contains_key("yParity")
    {
        object.remove("v");
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
    /// Signed transaction bytes could not be decoded.
    #[error("could not decode signed transaction: {0}")]
    Decode(#[from] Eip2718Error),
    /// Signature `v` value is not valid for the transaction type.
    #[error("invalid signature v value: {0}")]
    InvalidV(U256),
    /// An EIP-155 legacy transaction appears before Spurious Dragon.
    #[error("EIP-155 transaction before Spurious Dragon")]
    ProtectedLegacy,
    /// Sender could not be recovered from the signature.
    #[error("could not recover sender: {0}")]
    Recover(#[from] RecoveryError),
}

/// Where a transaction's sender comes from.
pub(crate) enum TxSigner {
    /// Sender taken from the fixture, for fixtures that carry no signature.
    Fixture(Address),
    /// Sender recovered from the signature, as the fork at `spec` validates it.
    Signature { signature: Signature, spec: SpecId },
}

/// Common transaction fields already selected from a fixture.
pub(crate) struct TxFields {
    /// Explicit transaction type.
    pub(crate) tx_type: Option<u8>,
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
pub(crate) fn build_recovered_tx(
    fields: TxFields,
    signer: TxSigner,
) -> Result<RecoveredTxEnvelope, TxBuildError> {
    let mut request = TransactionRequest::default()
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
    let caller = match signer {
        TxSigner::Fixture(caller) => caller,
        TxSigner::Signature { signature, spec } => {
            recover_sender(&signature, tx.signature_hash(), spec)?
        }
    };
    Ok(recovered_envelope(tx, caller))
}

/// Decodes an EIP-2718 signed transaction and recovers its sender.
pub(crate) fn decode_recovered_tx(
    bytes: &[u8],
    spec: SpecId,
) -> Result<RecoveredTxEnvelope, TxBuildError> {
    let tx = EthereumTxEnvelope::<TxEip4844>::decode_2718_exact(bytes)?;
    if let EthereumTxEnvelope::Legacy(signed) = &tx
        && signed.tx().chain_id.is_some()
        && !spec.enables(SpecId::SPURIOUS_DRAGON)
    {
        return Err(TxBuildError::ProtectedLegacy);
    }
    let caller = recover_sender(tx.signature(), tx.signature_hash(), spec)?;
    Ok(Recovered::new_unchecked(tx.into(), caller))
}

/// Splits a fixture `v` value into the signature parity and, for legacy transactions, the EIP-155
/// chain ID it encodes. A legacy `v` is 27 or 28, or from Spurious Dragon on `35 + 2 * chain_id`
/// plus the parity.
pub(crate) fn signature_from_vrs(
    r: U256,
    s: U256,
    v: U256,
    legacy: bool,
    spec: SpecId,
) -> Result<(Signature, Option<u64>), TxBuildError> {
    let invalid = || TxBuildError::InvalidV(v);
    let (y_parity, chain_id) = if !legacy && v <= U256::ONE {
        (v == U256::ONE, None)
    } else if legacy && (v == U256::from(27) || v == U256::from(28)) {
        (v == U256::from(28), None)
    } else if legacy && v >= U256::from(35) {
        if !spec.enables(SpecId::SPURIOUS_DRAGON) {
            return Err(TxBuildError::ProtectedLegacy);
        }
        let encoded = v - U256::from(35);
        (encoded.bit(0), Some(u64::try_from(encoded >> 1).map_err(|_| invalid())?))
    } else {
        return Err(invalid());
    };
    Ok((Signature::new(r, s, y_parity), chain_id))
}

fn recover_sender(
    signature: &Signature,
    hash: B256,
    spec: SpecId,
) -> Result<Address, RecoveryError> {
    // EIP-2 rejects high-`s` signatures from Homestead on.
    if spec.enables(SpecId::HOMESTEAD) {
        secp256k1::recover_signer(signature, hash)
    } else {
        secp256k1::recover_signer_unchecked(signature, hash)
    }
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
    let public_key = key.verifying_key().to_sec1_point(false);
    Some(Address::from_raw_public_key(&public_key.as_bytes()[1..]))
}

fn recovered_envelope(tx: TypedTransaction, caller: Address) -> RecoveredTxEnvelope {
    match tx {
        TypedTransaction::Legacy(tx) => Recovered::new_unchecked(TxEnvelope::Legacy(tx), caller),
        TypedTransaction::Eip2930(tx) => Recovered::new_unchecked(TxEnvelope::Eip2930(tx), caller),
        TypedTransaction::Eip1559(tx) => Recovered::new_unchecked(TxEnvelope::Eip1559(tx), caller),
        TypedTransaction::Eip4844(tx) => Recovered::new_unchecked(TxEnvelope::Eip4844(tx), caller),
        TypedTransaction::Eip7702(tx) => Recovered::new_unchecked(tx.into(), caller),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_consensus::{TxLegacy, crypto::secp256k1};
    use alloy_eips::eip2718::Encodable2718;
    use serde_json::json;

    #[test]
    fn test_authorization_serializes_as_raw_fixture_value() {
        let value = json!({
            "chainId": "0x1",
            "address": "0x0000000000000000000000000000000000000001",
            "nonce": "0x0",
            "yParity": "0x1",
            "r": "0x2",
            "s": "0x3",
        });
        let authorization = TestAuthorization { value: value.clone() };

        assert_eq!(serde_json::to_value(authorization).unwrap(), value);
    }

    #[test]
    fn signature_from_vrs_accepts_the_v_each_fork_allows() {
        let (r, s) = (U256::ONE, U256::ONE);
        let v = |v: u64| U256::from(v);
        let parse = |v: U256, legacy, spec| signature_from_vrs(r, s, v, legacy, spec);

        assert_eq!(parse(v(27), true, SpecId::FRONTIER).unwrap().1, None);
        assert!(!parse(v(27), true, SpecId::FRONTIER).unwrap().0.v());
        assert!(parse(v(28), true, SpecId::FRONTIER).unwrap().0.v());
        assert!(matches!(
            parse(v(37), true, SpecId::TANGERINE),
            Err(TxBuildError::ProtectedLegacy)
        ));
        let (signature, chain_id) = parse(v(38), true, SpecId::SPURIOUS_DRAGON).unwrap();
        assert!(signature.v());
        assert_eq!(chain_id, Some(1));
        assert!(matches!(parse(v(29), true, SpecId::CANCUN), Err(TxBuildError::InvalidV(_))));
        assert!(matches!(parse(v(27), false, SpecId::CANCUN), Err(TxBuildError::InvalidV(_))));
        assert!(parse(v(1), false, SpecId::CANCUN).unwrap().0.v());
    }

    #[test]
    fn signature_from_vrs_keeps_v_at_full_width() {
        let encoded = |chain_id: u128| U256::from(chain_id) * U256::from(2) + U256::from(35);
        let parse = |v| signature_from_vrs(U256::ONE, U256::ONE, v, true, SpecId::CANCUN);

        // The encoded `v` of the largest chain ID needs 65 bits.
        let chain_id = u128::from(u64::MAX);
        assert_eq!(parse(encoded(chain_id)).unwrap().1, Some(u64::MAX));
        assert!(matches!(parse(encoded(chain_id + 1)), Err(TxBuildError::InvalidV(_))));
    }

    #[test]
    fn decode_recovered_tx_rejects_eip155_before_spurious_dragon() {
        let secret = B256::with_last_byte(1);
        let tx = TxLegacy { chain_id: Some(1), gas_limit: 21_000, ..Default::default() };
        let signature = secp256k1::sign_message(secret, tx.signature_hash()).unwrap();
        let bytes = tx.into_signed(signature).encoded_2718();

        assert!(matches!(
            decode_recovered_tx(&bytes, SpecId::TANGERINE),
            Err(TxBuildError::ProtectedLegacy)
        ));
        let recovered = decode_recovered_tx(&bytes, SpecId::SPURIOUS_DRAGON).unwrap();
        assert_eq!(recovered.signer(), recover_address(secret.as_slice()).unwrap());
    }
}
