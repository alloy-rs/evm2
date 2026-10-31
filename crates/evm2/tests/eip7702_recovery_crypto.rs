//! Authority recovery must go through an installed crypto provider without initializing it.
//!
//! The provider is a process-wide `OnceLock`, so this lives in its own test binary.

use alloy_consensus::TxEip7702;
use alloy_eips::eip7702::{Authorization, SignedAuthorization};
use alloy_primitives::{Address, U256};
use evm2::{
    Crypto, PrecompileHalt,
    ethereum::{LazyAuthorization, LazyTxEip7702},
    install_crypto,
};
use k256::ecdsa::SigningKey;

#[derive(Debug)]
struct BogusCrypto;

impl Crypto for BogusCrypto {
    fn secp256k1_ecrecover(
        &self,
        _: &[u8; 64],
        _: u8,
        _: &[u8; 32],
    ) -> Result<[u8; 32], PrecompileHalt> {
        Ok([0xff; 32])
    }
}

#[test]
fn recovery_uses_installed_crypto_provider() {
    let key = SigningKey::from_slice(&[0x11; 32]).unwrap();
    let signer =
        Address::from_raw_public_key(&key.verifying_key().to_sec1_point(false).as_bytes()[1..]);
    let inner = Authorization { chain_id: U256::from(1), address: Address::ZERO, nonce: 7 };
    let (sig, recid) = key.sign_prehash_recoverable(inner.signature_hash().as_slice());
    let r = U256::from_be_slice(&sig.r().to_bytes());
    let s = U256::from_be_slice(&sig.s().to_bytes());
    let authorization = SignedAuthorization::new_unchecked(inner, recid.to_byte(), r, s);
    let tx = TxEip7702 { authorization_list: vec![authorization.clone()], ..Default::default() };

    assert_eq!(LazyAuthorization::Signed(&authorization).authority(), Some(signer));
    let recovered = LazyTxEip7702::from_recovered_authorizations(tx.clone());
    assert_eq!(recovered.authorizations().next().unwrap().authority(), Some(signer));

    assert!(install_crypto(BogusCrypto), "recovery initialized the crypto provider");

    let bogus = Address::repeat_byte(0xff);
    assert_eq!(LazyAuthorization::Signed(&authorization).authority(), Some(bogus));
    let recovered = LazyTxEip7702::from_recovered_authorizations(tx);
    assert_eq!(recovered.authorizations().next().unwrap().authority(), Some(bogus));
}
