//! `ecrecover` precompile.
//!
//! Depending on enabled features, it will use different implementations of `ecrecover`.
//! * [`k256`](https://crates.io/crates/k256) - uses maintained pure rust lib `k256`, it is perfect
//!   use for no_std environments.
//! * [`secp256k1`](https://crates.io/crates/secp256k1) - uses `bitcoin_secp256k1` lib, it is a C
//!   implementation of secp256k1 used in bitcoin core. It is faster than k256 and enabled by
//!   default and in std environment.
//!
//! Order of preference is `secp256k1` -> `k256`. Where if no features are enabled, it will use
//! `k256`.
//!
//! Input format:
//! [32 bytes for message][64 bytes for signature][1 byte for recovery id]
//!
//! Output format:
//! [32 bytes for recovered address]

#[cfg(feature = "secp256k1")]
pub(crate) mod bitcoin_secp256k1;
pub(crate) mod k256;

use crate::{
    Evm, EvmFeatures, EvmTypesHost,
    constants::{EIP7702_BYTECODE_LEN, EIP7702_MAGIC_BYTES, EIP7702_VERSION},
    interpreter::{GasTracker, Message},
    precompiles::{PrecompileOutput, PrecompileResult},
    utils::right_pad,
    version::GasId,
};
use alloy_primitives::{Address, Bytes};

/// `ecrecover` precompile function. Read more about input and output format in [this module
/// docs](self).
pub fn run<T: EvmTypesHost>(
    evm: &mut Evm<'_, T>,
    message: &Message<T>,
    gas: &mut GasTracker,
) -> PrecompileResult {
    const ECRECOVER_BASE: u64 = 3_000;

    gas.spend(ECRECOVER_BASE)?;

    let input = right_pad::<128>(message.input.as_ref());

    // `v` must be a 32-byte big-endian integer equal to 27 or 28.
    if !(input[32..63].iter().all(|&b| b == 0) && matches!(input[63], 27 | 28)) {
        return Ok(PrecompileOutput::new(Bytes::new()));
    }

    let msg = input.first_chunk().unwrap();
    let recid = input[63] - 27;
    let sig = input.last_chunk().unwrap();

    let Ok(recovered) = crate::precompiles::crypto().secp256k1_ecrecover(sig, recid, msg) else {
        return Ok(PrecompileOutput::new(Bytes::new()));
    };
    if evm.feature(EvmFeatures::EIP8151) {
        let address = Address::from_word(recovered.into());
        let warm_cost = evm.version().gas_params.get(GasId::WarmStorageReadCost);
        let access_cost = if evm.state().is_account_warm(&address) {
            warm_cost
        } else {
            evm.version().gas_params.cold_account_additional_cost() + warm_cost
        };
        gas.spend(access_cost + warm_cost)?;
        let mut account = evm.state_mut().account(&address)?;
        account.warm();
        let code = account.load_code()?;
        let raw = code.original_byte_slice();
        if !raw.is_empty()
            && !(raw.len() == EIP7702_BYTECODE_LEN
                && raw.starts_with(EIP7702_MAGIC_BYTES)
                && raw[2] == EIP7702_VERSION)
        {
            return Ok(PrecompileOutput::new(Bytes::new()));
        }
    }
    Ok(PrecompileOutput::new(recovered.to_vec().into()))
}

pub(crate) fn ecrecover_bytes(sig: &[u8; 64], recid: u8, msg: &[u8; 32]) -> Option<[u8; 32]> {
    match ecrecover(sig.into(), recid, msg.into()) {
        Ok(address) => Some(address.0),
        Err(_) => None,
    }
}

// Select the correct implementation based on the enabled features.
core::cfg_select! {
    feature = "secp256k1" => {
        pub(crate) use bitcoin_secp256k1::ecrecover;
    }
    _ => {
        pub(crate) use k256::ecrecover;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BaseEvmTypes, Precompiles, SpecId,
        bytecode::Bytecode,
        env::BlockEnvExt,
        evm::{AccountInfo, InMemoryDB},
        interpreter::{Gas, Host},
        precompiles::SECP256K1_ECRECOVER,
        registry::TxRegistry,
    };
    use ::k256::ecdsa::SigningKey;
    use alloc::vec;

    fn signed_input() -> (Address, [u8; 128]) {
        let key = SigningKey::from_slice(&[0x11; 32]).unwrap();
        let address =
            Address::from_raw_public_key(&key.verifying_key().to_sec1_point(false).as_bytes()[1..]);
        let hash = [0x42; 32];
        let (signature, recovery_id) = key.sign_prehash_recoverable(&hash);
        let mut input = [0; 128];
        input[..32].copy_from_slice(&hash);
        input[63] = 27 + recovery_id.to_byte();
        input[64..].copy_from_slice(&signature.to_bytes());
        (address, input)
    }

    fn recovery_evm(
        spec: SpecId,
        address: Address,
        code: Option<Bytecode>,
    ) -> Evm<'static, BaseEvmTypes> {
        let mut db = InMemoryDB::default();
        if let Some(code) = code {
            db.insert_account_info(&address, AccountInfo::default().with_code(code));
        }
        let mut evm =
            Evm::new(spec, BlockEnvExt::default(), TxRegistry::new(), db, Precompiles::base(spec));
        evm.state_mut().enable_bal_builder();
        evm
    }

    fn recover(
        evm: &mut Evm<'_, BaseEvmTypes>,
        input: &[u8; 128],
        gas: &mut GasTracker,
    ) -> PrecompileResult {
        let message =
            Message::<BaseEvmTypes> { input: Bytes::copy_from_slice(input), ..Default::default() };
        SECP256K1_ECRECOVER().run()(evm, &message, gas)
    }

    #[test]
    fn recovery_checks_raw_code_only_after_activation() {
        let (address, input) = signed_input();
        let delegate = Address::with_last_byte(0x42);
        let delegation = Bytecode::new_eip7702(delegate);
        let mut wrong_version = delegation.original_byte_slice().to_vec();
        wrong_version[2] = EIP7702_VERSION + 1;
        let mut long = delegation.original_byte_slice().to_vec();
        long.push(0);
        let cases = [
            (None, true),
            (Some(Bytecode::default()), true),
            (Some(Bytecode::new_legacy(Bytes::from_static(&[0]))), false),
            (Some(delegation.clone()), true),
            (Some(Bytecode::new_legacy(delegation.original_byte_slice().to_vec().into())), true),
            (Some(Bytecode::new_legacy(wrong_version.into())), false),
            (Some(Bytecode::new_legacy(long.into())), false),
            (Some(Bytecode::new_legacy(vec![0xef, 0x01, 0x00].into())), false),
        ];
        for spec in [SpecId::AMSTERDAM, SpecId::BOGOTA] {
            for (code, permitted) in &cases {
                let mut evm = recovery_evm(spec, address, code.clone());
                let mut gas = GasTracker::new(10_000);
                let output = recover(&mut evm, &input, &mut gas).unwrap();
                let activated = spec == SpecId::BOGOTA;
                let expected = if !activated || *permitted {
                    address.into_word().as_slice().to_vec()
                } else {
                    vec![]
                };
                assert_eq!(output.bytes(), expected, "{spec:?}, {code:?}");
                assert_eq!(gas.spent(), if activated { 6_100 } else { 3_000 });
                assert_eq!(evm.state().is_account_warm(&address), activated);
                evm.state_mut().commit_transaction();
                let accounts = &evm.state().bal_builder().unwrap().accounts;
                assert_eq!(accounts.contains_key(&address), activated);
                assert!(!accounts.contains_key(&delegate));
            }
        }
    }

    #[test]
    fn invalid_signatures_do_not_access_state() {
        let (address, valid_input) = signed_input();
        let mut invalid_v = valid_input;
        invalid_v[32] = 1;
        let mut invalid_signature = [0; 128];
        invalid_signature[63] = 27;
        for spec in [SpecId::AMSTERDAM, SpecId::BOGOTA] {
            for input in [invalid_v, invalid_signature] {
                let mut evm = recovery_evm(spec, address, None);
                let mut gas = GasTracker::new(3_000);
                assert!(recover(&mut evm, &input, &mut gas).unwrap().bytes().is_empty());
                assert_eq!(gas.spent(), 3_000);
                assert!(evm.state().bal_builder().unwrap().accounts.is_empty());
                assert!(!evm.state().is_account_warm(&address));
            }
        }
    }

    #[test]
    fn recovery_gas_is_paid_before_accessing_state() {
        let (address, input) = signed_input();
        for warm in [false, true] {
            let required = if warm { 3_200 } else { 6_100 };
            for limit in [2_999, 3_000, 3_199, 3_200, 6_099, 6_100] {
                let mut evm = recovery_evm(SpecId::BOGOTA, address, None);
                if warm {
                    evm.state_mut().prewarm(&address);
                }
                let mut gas = GasTracker::new(limit);
                let result = recover(&mut evm, &input, &mut gas);
                if limit < required {
                    assert!(result.unwrap_err().is_oog());
                    assert!(evm.state().bal_builder().unwrap().accounts.is_empty());
                    assert_eq!(evm.state().is_account_warm(&address), warm);
                } else {
                    assert_eq!(result.unwrap().bytes(), address.into_word().as_slice());
                    assert_eq!(gas.spent(), required);
                    assert!(evm.state().is_account_warm(&address));
                }
            }
        }
    }

    #[test]
    fn rejected_code_keeps_reads_and_reverts_only_runtime_warmth() {
        let (address, input) = signed_input();
        for code in [Bytecode::default(), Bytecode::new_legacy(Bytes::from_static(&[0]))] {
            let mut evm = recovery_evm(SpecId::BOGOTA, address, Some(code.clone()));
            let checkpoint = evm.state().checkpoint();
            for expected_gas in [6_100, 3_200] {
                let mut gas = GasTracker::new(expected_gas);
                let output = recover(&mut evm, &input, &mut gas).unwrap();
                assert_eq!(output.bytes().is_empty(), !code.is_empty());
                assert_eq!(gas.spent(), expected_gas);
            }
            let features = evm.version().features;
            evm.state_mut().rollback(checkpoint, features);
            assert!(!evm.state().is_account_warm(&address));
            evm.state_mut().commit_transaction();
            let account = &evm.state().bal_builder().unwrap().accounts[&address];
            assert!(account.account_info.nonce.is_empty());
            assert!(account.account_info.balance.is_empty());
            assert!(account.account_info.code.is_empty());
            assert!(account.storage.storage.is_empty());
        }
    }

    #[test]
    fn recovery_observes_adopted_code_and_its_rollback() {
        let (address, input) = signed_input();
        let source = Address::with_last_byte(0x33);
        let mut evm = recovery_evm(SpecId::BOGOTA, address, None);
        evm.overlay_db_mut().insert_account_info(
            &source,
            AccountInfo::default().with_code(Bytecode::new_legacy(Bytes::from_static(&[0]))),
        );
        let mut gas = GasTracker::new(6_100);
        assert_eq!(
            recover(&mut evm, &input, &mut gas).unwrap().bytes(),
            address.into_word().as_slice()
        );
        let checkpoint = evm.state().checkpoint();
        assert!(evm.set_code_from(&address, &source, &mut Gas::new(100_000)).unwrap());
        let mut gas = GasTracker::new(3_200);
        assert!(recover(&mut evm, &input, &mut gas).unwrap().bytes().is_empty());
        let features = evm.version().features;
        evm.state_mut().rollback(checkpoint, features);
        let mut gas = GasTracker::new(3_200);
        assert_eq!(
            recover(&mut evm, &input, &mut gas).unwrap().bytes(),
            address.into_word().as_slice()
        );
    }
}
