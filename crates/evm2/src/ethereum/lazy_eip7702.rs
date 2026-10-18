use crate::precompiles::secp256k1::ecrecover_bytes;
use alloc::vec::Vec;
use alloy_consensus::{Transaction, TxEip7702, TxType};
use alloy_eips::{
    Typed2718,
    eip2930::AccessList,
    eip7702::{
        Authorization, RecoveredAuthority, RecoveredAuthorization, SignedAuthorization,
        constants::SECP256K1N_HALF,
    },
};
use alloy_primitives::{Address, B256, Bytes, ChainId, TxKind, U256};

/// EIP-7702 authorization that may already have its authority recovered.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum LazyAuthorization {
    /// Signed authorization whose authority is recovered on demand.
    Signed(SignedAuthorization),
    /// Authorization with a cached recovery result.
    Recovered(RecoveredAuthorization),
}

impl LazyAuthorization {
    /// Returns the inner unsigned authorization.
    pub fn inner(&self) -> &Authorization {
        match self {
            Self::Signed(authorization) => authorization.inner(),
            Self::Recovered(authorization) => authorization,
        }
    }

    /// Returns the authorization chain ID.
    pub fn chain_id(&self) -> &U256 {
        self.inner().chain_id()
    }

    /// Returns the delegated address.
    pub fn address(&self) -> &Address {
        self.inner().address()
    }

    /// Returns the authorization nonce.
    pub fn nonce(&self) -> u64 {
        self.inner().nonce()
    }

    /// Returns the recovered authority address, if the authorization signature is valid.
    ///
    /// Signed authorizations perform recovery on demand. Recovered authorizations return their
    /// cached result without doing secp256k1 recovery.
    pub fn authority(&self) -> Option<Address> {
        match self {
            Self::Signed(authorization) => recover_authority(authorization),
            Self::Recovered(authorization) => authorization.authority(),
        }
    }

    /// Returns the signed authorization if this authorization has not been recovered yet.
    pub const fn as_signed(&self) -> Option<&SignedAuthorization> {
        match self {
            Self::Signed(authorization) => Some(authorization),
            Self::Recovered(_) => None,
        }
    }

    /// Returns the recovered authorization if this authorization has a cached recovery result.
    pub const fn as_recovered(&self) -> Option<&RecoveredAuthorization> {
        match self {
            Self::Recovered(authorization) => Some(authorization),
            Self::Signed(_) => None,
        }
    }
}

impl From<SignedAuthorization> for LazyAuthorization {
    fn from(value: SignedAuthorization) -> Self {
        Self::Signed(value)
    }
}

impl From<RecoveredAuthorization> for LazyAuthorization {
    fn from(value: RecoveredAuthorization) -> Self {
        Self::Recovered(value)
    }
}

/// EIP-7702 transaction used by the executable Ethereum transaction model.
///
/// This preserves the consensus [`TxEip7702`] fields but stores authorization list entries as
/// signed-or-recovered values so callers can supply cached EIP-7702 authority recovery results.
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct LazyTxEip7702 {
    /// EIP-155 replay protection chain ID.
    pub chain_id: ChainId,
    /// Sender nonce.
    pub nonce: u64,
    /// Maximum gas that may be used by the transaction.
    pub gas_limit: u64,
    /// Maximum total fee per gas.
    pub max_fee_per_gas: u128,
    /// Maximum priority fee per gas.
    pub max_priority_fee_per_gas: u128,
    /// Message-call recipient.
    pub to: Address,
    /// Wei value transferred to the recipient.
    pub value: U256,
    /// EIP-2930 access list.
    pub access_list: AccessList,
    /// EIP-7702 authorizations, either signed or with cached recovery results.
    pub authorization_list: Vec<LazyAuthorization>,
    signed_authorization_list: Vec<SignedAuthorization>,
    /// Transaction input calldata.
    pub input: Bytes,
}

impl LazyTxEip7702 {
    /// Converts a consensus transaction while keeping signed authorizations unresolved.
    pub fn from_signed_authorizations(tx: TxEip7702) -> Self {
        let authorization_list = tx.authorization_list.iter().cloned().map(Into::into).collect();
        Self::from_authorizations(tx, authorization_list)
    }

    /// Converts a consensus transaction and eagerly recovers all authorization authorities.
    ///
    /// Invalid authorization signatures are cached as invalid recovered authorizations so execution
    /// skips them in the same way as a failed on-demand recovery.
    pub fn from_recovered_authorizations(tx: TxEip7702) -> Self {
        let authorization_list = tx
            .authorization_list
            .iter()
            .map(|authorization| {
                let authority = recover_authority(authorization)
                    .map_or(RecoveredAuthority::Invalid, RecoveredAuthority::Valid);
                RecoveredAuthorization::new_unchecked(authorization.inner().clone(), authority)
                    .into()
            })
            .collect();
        Self::from_authorizations(tx, authorization_list)
    }

    /// Converts a consensus transaction using an externally supplied authorization list.
    ///
    /// The caller is responsible for ensuring that `authorization_list` corresponds to `tx`.
    pub fn from_authorizations(tx: TxEip7702, authorization_list: Vec<LazyAuthorization>) -> Self {
        let TxEip7702 {
            chain_id,
            nonce,
            gas_limit,
            max_fee_per_gas,
            max_priority_fee_per_gas,
            to,
            value,
            access_list,
            authorization_list: signed_authorization_list,
            input,
        } = tx;
        Self {
            chain_id,
            nonce,
            gas_limit,
            max_fee_per_gas,
            max_priority_fee_per_gas,
            to,
            value,
            access_list,
            authorization_list,
            signed_authorization_list,
            input,
        }
    }

    /// Converts a consensus transaction using externally supplied recovered authorizations.
    ///
    /// The caller is responsible for ensuring that `authorization_list` corresponds to `tx`.
    pub fn from_cached_recovered_authorizations(
        tx: TxEip7702,
        authorization_list: Vec<RecoveredAuthorization>,
    ) -> Self {
        Self::from_authorizations(tx, authorization_list.into_iter().map(Into::into).collect())
    }

    /// Calculates a heuristic for the in-memory size of the transaction.
    #[inline]
    pub fn size(&self) -> usize {
        size_of::<Self>()
            + self.access_list.size()
            + self.input.len()
            + self.authorization_list.capacity() * size_of::<LazyAuthorization>()
            + self.signed_authorization_list.capacity() * size_of::<SignedAuthorization>()
    }
}

impl From<TxEip7702> for LazyTxEip7702 {
    fn from(value: TxEip7702) -> Self {
        Self::from_signed_authorizations(value)
    }
}

impl Typed2718 for LazyTxEip7702 {
    fn ty(&self) -> u8 {
        TxType::Eip7702 as u8
    }
}

impl Transaction for LazyTxEip7702 {
    fn chain_id(&self) -> Option<ChainId> {
        Some(self.chain_id)
    }

    fn nonce(&self) -> u64 {
        self.nonce
    }

    fn gas_limit(&self) -> u64 {
        self.gas_limit
    }

    fn gas_price(&self) -> Option<u128> {
        None
    }

    fn max_fee_per_gas(&self) -> u128 {
        self.max_fee_per_gas
    }

    fn max_priority_fee_per_gas(&self) -> Option<u128> {
        Some(self.max_priority_fee_per_gas)
    }

    fn max_fee_per_blob_gas(&self) -> Option<u128> {
        None
    }

    fn priority_fee_or_price(&self) -> u128 {
        self.max_priority_fee_per_gas
    }

    fn effective_gas_price(&self, base_fee: Option<u64>) -> u128 {
        alloy_eips::eip1559::calc_effective_gas_price(
            self.max_fee_per_gas,
            self.max_priority_fee_per_gas,
            base_fee,
        )
    }

    fn is_dynamic_fee(&self) -> bool {
        true
    }

    fn kind(&self) -> TxKind {
        TxKind::Call(self.to)
    }

    fn is_create(&self) -> bool {
        false
    }

    fn value(&self) -> U256 {
        self.value
    }

    fn input(&self) -> &Bytes {
        &self.input
    }

    fn access_list(&self) -> Option<&AccessList> {
        Some(&self.access_list)
    }

    fn blob_versioned_hashes(&self) -> Option<&[B256]> {
        None
    }

    fn authorization_list(&self) -> Option<&[SignedAuthorization]> {
        Some(&self.signed_authorization_list)
    }
}

/// Recovers the authority of a signed authorization with the same secp256k1 backend as the
/// ecrecover precompile, instead of whichever backend alloy-primitives was built with.
///
/// Keeps the checks of [`SignedAuthorization::recover_authority`]: the y-parity must be 0 or 1,
/// `s` must not exceed secp256k1n/2, and the backend rejects a zero or out-of-range `r` or `s`.
fn recover_authority(authorization: &SignedAuthorization) -> Option<Address> {
    let signature = authorization.signature().ok()?;
    if signature.s() > SECP256K1N_HALF {
        return None;
    }
    let rsy = signature.as_rsy();
    let hash = ecrecover_bytes(rsy.first_chunk()?, rsy[64], &authorization.signature_hash())?;
    Some(Address::from_word(hash.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use k256::ecdsa::SigningKey;

    #[test]
    fn authority_matches_alloy_recovery() {
        let key = SigningKey::from_slice(&[0x11; 32]).unwrap();
        let expected =
            Address::from_raw_public_key(&key.verifying_key().to_sec1_point(false).as_bytes()[1..]);
        let inner = Authorization {
            chain_id: U256::from(1),
            address: Address::with_last_byte(0xbb),
            nonce: 7,
        };
        let (sig, recid) = key.sign_prehash_recoverable(inner.signature_hash().as_slice());
        let r = U256::from_be_slice(&sig.r().to_bytes());
        let s = U256::from_be_slice(&sig.s().to_bytes());
        let y = recid.to_byte();
        let n = SECP256K1N_HALF * U256::from(2) + U256::from(1);
        let signed = |y, r, s| SignedAuthorization::new_unchecked(inner.clone(), y, r, s);

        let valid = signed(y, r, s);
        assert_eq!(LazyAuthorization::from(valid.clone()).authority(), Some(expected));
        // Without the low-s check, the high-s signature would recover `expected` too.
        let authorization_list = vec![
            valid,
            signed(y ^ 1, r, n - s),
            signed(2, r, s),
            signed(y, U256::ZERO, s),
            signed(y, n, s),
            signed(y, r, U256::ZERO),
        ];
        let tx = TxEip7702 { authorization_list, ..Default::default() };
        let signed = LazyTxEip7702::from_signed_authorizations(tx.clone());
        let recovered = LazyTxEip7702::from_recovered_authorizations(tx.clone());
        for (i, authorization) in tx.authorization_list.iter().enumerate() {
            let want = authorization.recover_authority().ok();
            assert_eq!(want.is_some(), i == 0, "case {i}");
            assert_eq!(signed.authorization_list[i].authority(), want, "signed case {i}");
            assert_eq!(recovered.authorization_list[i].authority(), want, "recovered case {i}");
        }
    }
}
