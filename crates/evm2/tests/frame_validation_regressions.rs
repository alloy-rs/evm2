//! Static frame validation, nonce domains, and first-error ordering before database access.

mod common;

use alloy_consensus::TxEip8141;
use alloy_eips::eip8141::{
    EXPIRY_DATA_LENGTH, EXPIRY_VERIFIER, Frame, FrameLimits, FrameMode, FrameSignature, SECP256K1N,
    SECP256R1N, SignatureMessage, SignatureScheme,
};
use alloy_primitives::{Address, B256, U256};
use common::{BODY, FailingDatabase, SENDER, recovered};
use evm2::{
    BaseEvmTypes, Evm, EvmFeatures, ExecutionConfig, Precompiles, SpecId, Version,
    env::BlockEnvExt,
    ethereum::{eip8141::FrameGas, ethereum_tx_registry},
    evm::Db,
    registry::{FrameTransactionError, HandlerError},
};
use k256::ecdsa::SigningKey;
use p256::ecdsa::{
    Signature as P256Signature, SigningKey as P256SigningKey, signature::hazmat::PrehashSigner,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

fn frame(mode: FrameMode, target: Address, flags: u8) -> Frame {
    Frame { flags, ..common::frame(mode, target) }
}

fn payload() -> TxEip8141 {
    common::payload(vec![frame(FrameMode::Verify, SENDER, 3), frame(FrameMode::Sender, BODY, 0)])
}

fn evm(version: Version, block: BlockEnvExt) -> (Evm<'static, BaseEvmTypes>, Arc<AtomicUsize>) {
    let reads = Arc::new(AtomicUsize::new(0));
    let evm = Evm::new_with_execution_config(
        ExecutionConfig::for_spec_and_version(SpecId::BOGOTA, version),
        SpecId::BOGOTA,
        block,
        ethereum_tx_registry(SpecId::BOGOTA),
        Db::new(FailingDatabase(reads.clone(), "validation performed a database read")),
        Precompiles::base(SpecId::BOGOTA),
    );
    (evm, reads)
}

const fn invalid(reason: FrameTransactionError) -> HandlerError {
    HandlerError::InvalidFrameTransaction(reason)
}

fn blob_hash() -> B256 {
    let mut hash = B256::ZERO;
    hash[0] = 1;
    hash
}

fn assert_static_error(name: &str, tx: TxEip8141, expected: HandlerError) {
    assert_static_error_with_env(
        name,
        tx,
        Version::new(SpecId::BOGOTA),
        BlockEnvExt::default(),
        expected,
    );
}

fn assert_static_error_with_env(
    name: &str,
    tx: TxEip8141,
    version: Version,
    block: BlockEnvExt,
    expected: HandlerError,
) {
    for entry in ["validate", "execute", "prefix"] {
        if entry == "prefix" && tx.frames.is_empty() {
            continue;
        }
        let (mut evm, reads) = evm(version, block.clone());
        let tx = recovered(tx.clone());
        let error = match entry {
            "validate" => evm.validate_tx(&tx).unwrap_err(),
            "execute" => evm.transact(&tx).unwrap_err(),
            _ => evm.validate_frame_prefix(&tx, 1).unwrap_err(),
        };
        assert_eq!(error, expected, "{name}, entry={entry}");
        assert_eq!(reads.load(Ordering::Relaxed), 0, "{name}, entry={entry}");
        assert!(evm.logs().is_empty());
    }
}

#[test]
fn malformed_frame_structure_and_nonce_schemas_are_rejected_without_reads() {
    type Case = (&'static str, fn(&mut TxEip8141), FrameTransactionError);
    let cases: &[Case] = &[
        ("no frames", |tx| tx.frames.clear(), FrameTransactionError::InvalidFrameCount),
        (
            "65 frames",
            |tx| tx.frames.resize(65, frame(FrameMode::Default, BODY, 0)),
            FrameTransactionError::InvalidFrameCount,
        ),
        (
            "assert suffix",
            |tx| tx.frames[0].mode = FrameMode::Assert,
            FrameTransactionError::InvalidAssertionSuffix,
        ),
        (
            "reserved flags",
            |tx| tx.frames[1].flags = 0x80,
            FrameTransactionError::ReservedFrameFlag,
        ),
        (
            "verify value",
            |tx| tx.frames[0].value = U256::from(1),
            FrameTransactionError::ValueTransferNotAllowed,
        ),
        (
            "default value",
            |tx| {
                tx.frames[1].mode = FrameMode::Default;
                tx.frames[1].value = U256::from(1);
            },
            FrameTransactionError::ValueTransferNotAllowed,
        ),
        (
            "approval target",
            |tx| tx.frames[1].flags = 2,
            FrameTransactionError::InvalidExecutionApprovalTarget,
        ),
        ("verify atomic", |tx| tx.frames[0].flags |= 4, FrameTransactionError::AtomicVerify),
        ("atomic end", |tx| tx.frames[1].flags = 4, FrameTransactionError::InvalidAtomicBatchEnd),
        (
            "atomic before verify",
            |tx| {
                tx.frames[1].flags = 4;
                tx.frames.push(frame(FrameMode::Verify, SENDER, 0));
            },
            FrameTransactionError::InvalidAtomicBatchEnd,
        ),
        (
            "atomic approval",
            |tx| {
                tx.frames[1].flags = 5;
                tx.frames.push(frame(FrameMode::Sender, BODY, 0));
            },
            FrameTransactionError::AtomicBatchApproval,
        ),
        (
            "atomic successor approval",
            |tx| {
                tx.frames[1].flags = 4;
                tx.frames.push(frame(FrameMode::Default, BODY, 1));
            },
            FrameTransactionError::AtomicBatchApproval,
        ),
        (
            "atomic assert successor",
            |tx| {
                tx.frames[1].flags = 4;
                tx.frames.push(frame(FrameMode::Assert, BODY, 0));
            },
            FrameTransactionError::AtomicBatchEndsOnAssertion,
        ),
        (
            "assert atomic",
            |tx| {
                tx.frames[1].mode = FrameMode::Assert;
                tx.frames[1].flags = 4;
            },
            FrameTransactionError::AtomicAssertion,
        ),
        (
            "expiry fields",
            |tx| tx.frames.push(frame(FrameMode::Verify, EXPIRY_VERIFIER, 0)),
            FrameTransactionError::MalformedExpiryVerifier,
        ),
        (
            "expiry repeated",
            |tx| {
                let expiry = valid_expiry();
                tx.frames.extend([expiry.clone(), expiry]);
            },
            FrameTransactionError::MultipleExpiryVerifiers,
        ),
        (
            "expiry approval flags",
            |tx| {
                let mut expiry = valid_expiry();
                expiry.flags = 1;
                tx.frames.push(expiry);
            },
            FrameTransactionError::MalformedExpiryVerifier,
        ),
        (
            "expiry short data",
            |tx| {
                let mut expiry = valid_expiry();
                expiry.data = vec![0; EXPIRY_DATA_LENGTH - 1].into();
                tx.frames.push(expiry);
            },
            FrameTransactionError::MalformedExpiryVerifier,
        ),
        (
            "expiry long data",
            |tx| {
                let mut expiry = valid_expiry();
                expiry.data = vec![0; EXPIRY_DATA_LENGTH + 1].into();
                tx.frames.push(expiry);
            },
            FrameTransactionError::MalformedExpiryVerifier,
        ),
        (
            "expiry state budget",
            |tx| {
                let mut expiry = valid_expiry();
                expiry.limits.state = 1;
                tx.frames.push(expiry);
            },
            FrameTransactionError::MalformedExpiryVerifier,
        ),
        (
            "assert value",
            |tx| {
                tx.frames[1].mode = FrameMode::Assert;
                tx.frames[1].value = U256::from(1);
            },
            FrameTransactionError::ValueTransferNotAllowed,
        ),
        (
            "keyed schema absent",
            |tx| tx.nonce_keys = None,
            FrameTransactionError::NoncePayloadMismatch,
        ),
        (
            "17 nonce keys",
            |tx| tx.nonce_keys = Some((1..=17).map(U256::from).collect()),
            FrameTransactionError::TooManyNonceKeys,
        ),
        (
            "duplicate keys",
            |tx| tx.nonce_keys = Some(vec![U256::from(1), U256::from(1)]),
            FrameTransactionError::NonceKeysNotIncreasing,
        ),
        (
            "descending keys",
            |tx| tx.nonce_keys = Some(vec![U256::from(2), U256::from(1)]),
            FrameTransactionError::NonceKeysNotIncreasing,
        ),
    ];
    for &(name, mutate, reason) in cases {
        let mut tx = payload();
        mutate(&mut tx);
        assert_static_error(name, tx, invalid(reason));
    }
    let mut tx = payload();
    tx.nonce = u64::MAX;
    assert_static_error("sequence overflow", tx, HandlerError::NonceOverflow);
    let mut tx = payload();
    tx.frames[1].limits.execution = u64::MAX;
    assert_static_error(
        "execution sum overflow",
        tx,
        invalid(FrameTransactionError::PaymentOverflow),
    );
    let mut tx = payload();
    tx.frames[1].limits.state = u64::MAX;
    assert_static_error("state sum overflow", tx, invalid(FrameTransactionError::PaymentOverflow));
    let mut tx = payload();
    tx.frames[0].limits.state = 0;
    tx.frames[1].limits.state = u64::MAX - 100_000;
    assert_static_error(
        "combined reservation overflow",
        tx,
        invalid(FrameTransactionError::PaymentOverflow),
    );
}

#[test]
fn every_signature_is_validated_before_account_or_nonce_reads() {
    let mut signatures = vec![
        (
            "secp length",
            FrameSignature {
                scheme: SignatureScheme::Secp256k1,
                signature: vec![0; 64].into(),
                ..Default::default()
            },
        ),
        (
            "p256 length",
            FrameSignature {
                scheme: SignatureScheme::P256,
                signature: vec![0; 127].into(),
                ..Default::default()
            },
        ),
        ("arbitrary signer", FrameSignature { signer: SENDER.into(), ..Default::default() }),
        (
            "zero explicit message",
            FrameSignature { msg: SignatureMessage::Explicit(B256::ZERO), ..Default::default() },
        ),
    ];
    let mut secp = vec![0; 65];
    secp[0] = 2;
    secp[32] = 1;
    secp[64] = 1;
    signatures.push((
        "secp recovery bit",
        FrameSignature {
            scheme: SignatureScheme::Secp256k1,
            signature: secp.into(),
            ..Default::default()
        },
    ));
    let mut high_s = vec![0; 65];
    high_s[32] = 1;
    high_s[33..].copy_from_slice(&((SECP256K1N >> 1usize) + U256::from(1)).to_be_bytes::<32>());
    signatures.push((
        "secp high s",
        FrameSignature {
            scheme: SignatureScheme::Secp256k1,
            signature: high_s.into(),
            ..Default::default()
        },
    ));
    for (scheme, order, length, offset) in [
        (SignatureScheme::Secp256k1, SECP256K1N, 65, 1),
        (SignatureScheme::P256, SECP256R1N, 128, 0),
    ] {
        for (name, r, s) in [
            ("zero r", U256::ZERO, U256::from(1)),
            ("out of range r", order, U256::from(1)),
            ("zero s", U256::from(1), U256::ZERO),
            ("high s", U256::from(1), (order >> 1usize) + U256::from(1)),
        ] {
            let mut bytes = vec![0; length];
            bytes[offset..offset + 32].copy_from_slice(&r.to_be_bytes::<32>());
            bytes[offset + 32..offset + 64].copy_from_slice(&s.to_be_bytes::<32>());
            signatures.push((
                name,
                FrameSignature { scheme, signature: bytes.into(), ..Default::default() },
            ));
        }
    }
    let mut p256 = vec![0; 128];
    p256[31] = 1;
    p256[63] = 1;
    signatures.push((
        "p256 public key signer mismatch",
        FrameSignature {
            scheme: SignatureScheme::P256,
            signature: p256.into(),
            ..Default::default()
        },
    ));
    for (name, signature) in signatures {
        let mut tx = payload();
        tx.signatures = vec![FrameSignature::default(), signature];
        let reason = match name {
            "zero explicit message" => FrameTransactionError::InvalidSignatureMessage,
            "arbitrary signer" => FrameTransactionError::UnexpectedSignatureSigner,
            "p256 public key signer mismatch" => FrameTransactionError::SignatureSignerMismatch,
            _ => FrameTransactionError::SignatureValidationFailed,
        };
        assert_static_error(name, tx, invalid(reason));
    }
    // Structurally canonical, cryptographically valid signature for a different signer.
    let mut tx = payload();
    let key = SigningKey::from_slice(&[0x11; 32]).unwrap();
    let message = B256::with_last_byte(1);
    let (signature, recovery) = key.sign_prehash_recoverable(message.as_slice());
    let mut bytes = vec![recovery.to_byte()];
    bytes.extend_from_slice(&signature.to_bytes());
    tx.signatures.push(FrameSignature {
        scheme: SignatureScheme::Secp256k1,
        msg: SignatureMessage::Explicit(message),
        signature: bytes.into(),
        ..Default::default()
    });
    assert_static_error(
        "wrong recovered signer",
        tx,
        invalid(FrameTransactionError::SignatureSignerMismatch),
    );
}

#[test]
fn resource_fee_and_blob_validation_precedes_database_reads() {
    type Case = (&'static str, fn(&mut TxEip8141, &mut Version, &mut BlockEnvExt), HandlerError);
    let version = Version::new(SpecId::BOGOTA);
    let gas = FrameGas::calculate(&payload(), &version).unwrap();
    let cases: &[Case] = &[
        (
            "active execution cap",
            |tx, version, _| {
                version.tx_gas_limit_cap = FrameGas::calculate(tx, version).unwrap().execution - 1;
            },
            HandlerError::TxGasLimitGreaterThanCap {
                gas_limit: gas.execution,
                cap: gas.execution - 1,
            },
        ),
        (
            "independent block dimensions",
            |_, _, block| block.gas_limit = U256::from(1),
            HandlerError::GasLimitMoreThanBlock {
                gas_limit: gas.execution.max(gas.state),
                block_gas_limit: U256::from(1),
            },
        ),
        ("chain", |tx, _, _| tx.chain_id = 2, HandlerError::InvalidChainId { expected: 1, got: 2 }),
        (
            "base-fee budget",
            |_, _, block| block.basefee = U256::from(10),
            HandlerError::FrameFeeBudgetTooLow {
                required: U256::from(gas.maximum) * U256::from(10),
                got: payload().fees.max_fee,
            },
        ),
        (
            "blob count",
            |tx, version, _| {
                tx.blob_versioned_hashes = vec![blob_hash(); version.max_blobs_per_tx + 1];
                tx.fees.max_fee = U256::MAX;
            },
            HandlerError::TooManyBlobs {
                have: version.max_blobs_per_tx + 1,
                max: version.max_blobs_per_tx,
            },
        ),
        (
            "blob version",
            |tx, _, _| {
                tx.blob_versioned_hashes.push(B256::ZERO);
                tx.fees.max_fee = U256::MAX;
            },
            HandlerError::BlobVersionNotSupported,
        ),
        (
            "blob base-fee budget",
            |tx, _, block| {
                tx.blob_versioned_hashes.push(blob_hash());
                tx.fees.max_fee = U256::from(2 * 131_072 - 1);
                block.blob_basefee = U256::from(2);
            },
            HandlerError::FrameFeeBudgetTooLow {
                required: U256::from(2 * 131_072),
                got: U256::from(2 * 131_072 - 1),
            },
        ),
        (
            "base-fee charge overflow",
            |tx, _, block| {
                tx.fees.max_fee = U256::MAX;
                block.basefee = U256::MAX;
            },
            invalid(FrameTransactionError::BaseFeeOverflow),
        ),
        (
            "blob charge overflow",
            |tx, _, block| {
                tx.blob_versioned_hashes.push(blob_hash());
                tx.fees.max_fee = U256::MAX;
                block.blob_basefee = U256::MAX;
            },
            invalid(FrameTransactionError::BaseFeeOverflow),
        ),
    ];
    for (name, prepare, expected) in cases {
        let mut tx = payload();
        let mut version = version;
        let mut block = BlockEnvExt::default();
        prepare(&mut tx, &mut version, &mut block);
        assert_static_error_with_env(name, tx, version, block, expected.clone());
    }
}

#[test]
fn competing_errors_follow_static_preparation_order_and_valid_payload_reaches_db() {
    let mut tx = payload();
    tx.frames.clear();
    tx.nonce_keys = Some((1..=17).map(U256::from).collect());
    assert_static_error(
        "nonce keys before frame count",
        tx,
        invalid(FrameTransactionError::TooManyNonceKeys),
    );
    let mut tx = payload();
    tx.frames[1].flags = 0x80;
    tx.nonce_keys = Some((1..=17).map(U256::from).collect());
    assert_static_error(
        "nonce keys before structure",
        tx,
        invalid(FrameTransactionError::TooManyNonceKeys),
    );
    let mut tx = payload();
    tx.nonce_keys = Some((1..=17).map(U256::from).collect());
    tx.nonce = u64::MAX;
    assert_static_error(
        "keys before sequence",
        tx,
        invalid(FrameTransactionError::TooManyNonceKeys),
    );
    let mut tx = payload();
    tx.nonce = u64::MAX;
    tx.frames[1].limits.execution = u64::MAX;
    assert_static_error(
        "gas arithmetic before sequence",
        tx,
        invalid(FrameTransactionError::PaymentOverflow),
    );
    let mut tx = payload();
    tx.chain_id = 2;
    tx.fees.max_priority_fee_per_gas = U256::from(1);
    assert_static_error(
        "chain before priority",
        tx,
        HandlerError::InvalidChainId { expected: 1, got: 2 },
    );
    let mut tx = payload();
    tx.chain_id = 2;
    tx.signatures.push(FrameSignature { scheme: SignatureScheme::P256, ..Default::default() });
    assert_static_error(
        "chain before signature shape",
        tx,
        HandlerError::InvalidChainId { expected: 1, got: 2 },
    );
    let (mut evm, reads) = evm(Version::new(SpecId::BOGOTA), BlockEnvExt::default());
    assert!(
        matches!(evm.validate_tx(&recovered(payload())), Err(HandlerError::Database(error)) if error.is_fatal())
    );
    assert_eq!(reads.load(Ordering::Relaxed), 1);
}

#[test]
fn activation_and_signature_placeholder_controls_do_not_relax_static_shape() {
    let reads = Arc::new(AtomicUsize::new(0));
    let mut inactive = Evm::<BaseEvmTypes>::new(
        SpecId::AMSTERDAM,
        BlockEnvExt::default(),
        ethereum_tx_registry(SpecId::BOGOTA),
        Db::new(FailingDatabase(reads.clone(), "validation performed a database read")),
        Precompiles::base(SpecId::AMSTERDAM),
    );
    assert_eq!(
        inactive.validate_tx(&recovered(payload())).unwrap_err(),
        HandlerError::UnsupportedTransactionType(6)
    );
    assert_eq!(reads.load(Ordering::Relaxed), 0);
    for (feature, mode, error) in [
        (EvmFeatures::EIP8141, FrameMode::Sender, HandlerError::UnsupportedTransactionType(6)),
        (
            EvmFeatures::EIP8250,
            FrameMode::Sender,
            invalid(FrameTransactionError::NoncePayloadMismatch),
        ),
        (
            EvmFeatures::EIP7906,
            FrameMode::Assert,
            invalid(FrameTransactionError::AssertionsNotActive),
        ),
    ] {
        let mut version = Version::new(SpecId::BOGOTA);
        version.features.remove(feature);
        let (mut evm, reads) = evm(version, BlockEnvExt::default());
        let mut tx = payload();
        tx.frames[1] = frame(mode, BODY, 0);
        assert_eq!(evm.validate_tx(&recovered(tx)).unwrap_err(), error);
        assert_eq!(reads.load(Ordering::Relaxed), 0);
    }
    let mut version = Version::new(SpecId::BOGOTA);
    version.features.insert(EvmFeatures::FRAME_SIGNATURE_PLACEHOLDERS);
    let (mut evm, reads) = evm(version, BlockEnvExt::default());
    let mut tx = payload();
    tx.signatures.push(FrameSignature { scheme: SignatureScheme::Secp256k1, ..Default::default() });
    assert!(matches!(evm.validate_tx(&recovered(tx.clone())), Err(HandlerError::Database(_))));
    assert_eq!(reads.load(Ordering::Relaxed), 1);
    tx.signatures[0].msg = SignatureMessage::Explicit(B256::ZERO);
    assert_eq!(
        evm.validate_tx(&recovered(tx)).unwrap_err(),
        invalid(FrameTransactionError::InvalidSignatureMessage)
    );
    assert_eq!(reads.load(Ordering::Relaxed), 1);
}

#[test]
fn valid_structure_boundaries_reach_database_validation() {
    type Case = (&'static str, fn(&mut TxEip8141, &mut Version));
    let cases: &[Case] = &[
        ("baseline", |_, _| {}),
        ("one frame", |tx, _| tx.frames.truncate(1)),
        ("64 frames", |tx, _| tx.frames.resize(64, frame(FrameMode::Sender, BODY, 0))),
        ("16 keys", |tx, _| tx.nonce_keys = Some((1..=16).map(U256::from).collect())),
        ("zero keyed domain", |tx, _| tx.nonce_keys = Some(vec![U256::ZERO])),
        ("zero mixed with another key", |tx, _| {
            tx.nonce_keys = Some(vec![U256::ZERO, U256::from(1)]);
        }),
        ("single-use zero keyed domain", |tx, _| {
            tx.nonce_keys = Some(vec![U256::ZERO]);
            tx.nonce = u64::MAX;
        }),
        ("implicit sender approval target", |tx, _| tx.frames[0].target = Default::default()),
        ("last incrementable sequence", |tx, _| tx.nonce = u64::MAX - 1),
        ("zero frame budgets", |tx, _| tx.frames[1].limits = FrameLimits::default()),
        ("constrained expiry", |tx, _| tx.frames.push(valid_expiry())),
        ("maximum blob count", |tx, version| {
            tx.blob_versioned_hashes = vec![blob_hash(); version.max_blobs_per_tx];
            tx.fees.max_fee = U256::MAX;
        }),
        ("maximum aggregate budget", |tx, _| tx.fees.max_fee = U256::MAX),
        ("maximum priority with zero budget", |tx, _| tx.fees.max_priority_fee_per_gas = U256::MAX),
    ];
    for (name, prepare) in cases {
        let mut tx = payload();
        let mut version = Version::new(SpecId::BOGOTA);
        prepare(&mut tx, &mut version);
        let (mut evm, reads) = evm(version, BlockEnvExt::default());
        assert!(
            matches!(evm.validate_tx(&recovered(tx)), Err(HandlerError::Database(error)) if error.is_fatal()),
            "{name}"
        );
        assert_eq!(reads.load(Ordering::Relaxed), 1, "{name}");
    }
}

#[test]
fn native_p256_verification_accepts_a_valid_signature_and_rejects_invalid_crypto_before_reads() {
    let key = P256SigningKey::from_slice(&[0x22; 32]).unwrap();
    let message = B256::with_last_byte(1);
    let signature: P256Signature = key.sign_prehash(message.as_slice()).unwrap();
    let signature = signature.normalize_s();
    let public = key.verifying_key().to_sec1_point(false);
    let mut bytes = signature.to_bytes().to_vec();
    bytes.extend_from_slice(&public.as_bytes()[1..]);
    let mut tx = payload();
    tx.signatures = vec![
        FrameSignature::default(),
        FrameSignature {
            scheme: SignatureScheme::P256,
            signer: Address::from_raw_public_key(&public.as_bytes()[1..]).into(),
            msg: SignatureMessage::Explicit(message),
            signature: bytes.into(),
        },
    ];
    let (mut evm, reads) = evm(Version::new(SpecId::BOGOTA), BlockEnvExt::default());
    assert!(matches!(evm.validate_tx(&recovered(tx.clone())), Err(HandlerError::Database(_))));
    assert_eq!(reads.load(Ordering::Relaxed), 1);
    // Preserve canonical scalars and the signer/public-key match while invalidating the signature.
    let mut invalid_bytes = tx.signatures[1].signature.to_vec();
    invalid_bytes[..32].fill(0);
    invalid_bytes[31] = 1;
    tx.signatures[1].signature = invalid_bytes.into();
    assert_static_error(
        "p256 invalid crypto",
        tx,
        invalid(FrameTransactionError::SignatureValidationFailed),
    );
}

#[test]
fn prefix_bounds_are_checked_before_payload_preparation_or_database_access() {
    for (tx, boundary) in [
        (payload(), 0),
        (payload(), 3),
        (
            {
                let mut tx = payload();
                tx.frames.clear();
                tx
            },
            1,
        ),
    ] {
        let (mut evm, reads) = evm(Version::new(SpecId::BOGOTA), BlockEnvExt::default());
        assert_eq!(
            evm.validate_frame_prefix(&recovered(tx), boundary).unwrap_err(),
            invalid(FrameTransactionError::PrefixOutOfBounds)
        );
        assert_eq!(reads.load(Ordering::Relaxed), 0);
    }
}

fn valid_expiry() -> Frame {
    let mut expiry = frame(FrameMode::Verify, EXPIRY_VERIFIER, 0);
    expiry.limits.state = 0;
    expiry.data = vec![0; EXPIRY_DATA_LENGTH].into();
    expiry
}

#[test]
fn frame_environment_errors_follow_revm_precedence() {
    let version = Version::new(SpecId::BOGOTA);
    let mut tx = payload();
    tx.chain_id = 2;
    tx.frames[1].limits.execution = u64::MAX;
    assert_static_error(
        "chain before gas arithmetic",
        tx,
        HandlerError::InvalidChainId { expected: 1, got: 2 },
    );

    let mut tx = payload();
    tx.nonce = u64::MAX;
    tx.nonce_keys = None;
    assert_static_error("sequence before nonce schema", tx, HandlerError::NonceOverflow);

    let mut tx = payload();
    tx.frames[0].mode = FrameMode::Assert;
    tx.frames[1].flags = 0x80;
    assert_static_error(
        "reserved flags before assertion suffix",
        tx,
        invalid(FrameTransactionError::ReservedFrameFlag),
    );

    let mut tx = payload();
    tx.frames[1].mode = FrameMode::Assert;
    tx.frames[1].flags = 4;
    tx.frames[1].value = U256::from(1);
    assert_static_error(
        "value before assertion atomic flag",
        tx,
        invalid(FrameTransactionError::ValueTransferNotAllowed),
    );

    let mut tx = payload();
    tx.frames[1].flags = 0x80;
    let gas = FrameGas::calculate(&tx, &version).unwrap();
    let mut capped = version;
    capped.tx_gas_limit_cap = gas.execution - 1;
    assert_static_error_with_env(
        "cap before frame flags",
        tx,
        capped,
        BlockEnvExt::default(),
        HandlerError::TxGasLimitGreaterThanCap { gas_limit: gas.execution, cap: gas.execution - 1 },
    );

    let mut tx = payload();
    tx.blob_versioned_hashes = vec![B256::ZERO; version.max_blobs_per_tx + 1];
    tx.fees.max_fee = U256::MAX;
    tx.nonce = u64::MAX;
    tx.frames[1].flags = 0x80;
    assert_static_error(
        "blob version before count and nonce",
        tx.clone(),
        HandlerError::BlobVersionNotSupported,
    );
    tx.blob_versioned_hashes.fill(blob_hash());
    assert_static_error(
        "blob count before nonce and structure",
        tx,
        HandlerError::TooManyBlobs {
            have: version.max_blobs_per_tx + 1,
            max: version.max_blobs_per_tx,
        },
    );

    for blobs in [false, true] {
        let mut tx = payload();
        tx.nonce = u64::MAX;
        tx.frames[1].flags = 0x80;
        tx.signatures.push(FrameSignature { scheme: SignatureScheme::P256, ..Default::default() });
        if blobs {
            tx.blob_versioned_hashes = vec![B256::ZERO; version.max_blobs_per_tx + 1];
        }
        let block = BlockEnvExt {
            basefee: U256::from(1),
            blob_basefee: U256::from(2),
            ..Default::default()
        };
        let gas = FrameGas::calculate(&tx, &version).unwrap();
        let required =
            U256::from(gas.maximum) + U256::from(tx.blob_versioned_hashes.len() * 131_072 * 2);
        assert_static_error_with_env(
            "fee before blobs, nonce, cap and structure",
            tx,
            capped,
            block,
            HandlerError::FrameFeeBudgetTooLow { required, got: U256::ZERO },
        );
    }
}

#[test]
fn frame_fee_boundary_includes_blob_and_keyed_nonce_reservations() {
    let version = Version::new(SpecId::BOGOTA);
    for keyed in [false, true] {
        for blobs in [false, true] {
            let mut tx = payload();
            if keyed {
                tx.nonce_keys = Some(vec![U256::from(1)]);
            }
            if blobs {
                tx.blob_versioned_hashes.push(blob_hash());
            }
            let gas = FrameGas::calculate(&tx, &version).unwrap();
            let required =
                U256::from(gas.maximum) + U256::from(tx.blob_versioned_hashes.len() * 131_072 * 2);
            let block = BlockEnvExt {
                basefee: U256::from(1),
                blob_basefee: U256::from(2),
                ..Default::default()
            };
            tx.fees.max_fee = required - U256::from(1);
            assert_static_error_with_env(
                "one wei short",
                tx.clone(),
                version,
                block.clone(),
                HandlerError::FrameFeeBudgetTooLow { required, got: tx.fees.max_fee },
            );
            tx.fees.max_fee = required;
            for entry in ["validate", "execute", "prefix"] {
                let (mut evm, reads) = evm(version, block.clone());
                let tx = recovered(tx.clone());
                let error = match entry {
                    "validate" => evm.validate_tx(&tx).unwrap_err(),
                    "execute" => evm.transact(&tx).unwrap_err(),
                    _ => evm.validate_frame_prefix(&tx, 1).unwrap_err(),
                };
                assert!(
                    matches!(error, HandlerError::Database(_)),
                    "keyed={keyed}, blobs={blobs}, entry={entry}: {error}"
                );
                assert_eq!(reads.load(Ordering::Relaxed), 1);
            }
        }
    }
}

mod nonces {
    //! EIP-8250 empty-list legacy selection, zero keys, and single-use nonce exhaustion.

    use super::*;

    use alloy_consensus::{TxEip8141, transaction::Recovered};
    use alloy_eips::{
        eip8141::{Frame, FrameLimits, FrameMode, FrameStatus},
        eip8250::{NONCE_MANAGER, nonce_keys_hash, nonce_slot},
    };
    use alloy_primitives::{Address, Bytes, U256};
    use common::{APPROVE_BOTH, BODY, SENDER, stored};
    use evm2::{
        BaseEvmTypes, Evm, Inspector,
        bytecode::Bytecode,
        ethereum::TxEnvelope,
        evm::{AccountInfo, Bal, BlockAccessIndex},
        interpreter::InstrStop,
        registry::HandlerError,
        version::GasId,
    };
    use std::sync::Arc;

    const FAILURE: Address = Address::with_last_byte(0xc0);
    const WRITE_BODY: &[u8] = &[0x60, 42, 0x5f, 0x55, 0x00];

    struct Observe;

    impl Inspector<BaseEvmTypes> for Observe {}

    fn frame(mode: FrameMode, target: Address, flags: u8) -> Frame {
        Frame {
            flags,
            limits: FrameLimits { execution: 150_000, state: 300_000 },
            ..common::frame(mode, target)
        }
    }

    fn payload(keys: Vec<U256>, sequence: u64, frames: Vec<Frame>) -> Recovered<TxEnvelope> {
        common::recovered(TxEip8141 {
            nonce: sequence,
            nonce_keys: Some(keys),
            ..common::payload(frames)
        })
    }

    fn approval(keys: Vec<U256>, sequence: u64) -> Recovered<TxEnvelope> {
        payload(keys, sequence, vec![frame(FrameMode::Verify, SENDER, 3)])
    }

    fn evm(
        sender: &[u8],
        body: &[u8],
        legacy_nonce: u64,
        inspected: bool,
    ) -> Evm<'static, BaseEvmTypes> {
        let mut db = common::database([(BODY, body), (FAILURE, &[0x5f, 0x5f, 0xfd])]);
        db.insert_account_info(&SENDER, common::account(sender).with_nonce(legacy_nonce));
        db.insert_account_info(
            &NONCE_MANAGER,
            AccountInfo::default()
                .with_nonce(1)
                .with_code(Bytecode::new_legacy(Bytes::from_static(&[0x60, 0, 0x60, 0, 0xfd]))),
        );
        let mut evm = common::evm(db);
        if inspected {
            evm.set_inspector(Observe);
        }
        evm
    }

    fn keyed_nonce(evm: &mut Evm<'_, BaseEvmTypes>, key: U256) -> U256 {
        stored(evm, NONCE_MANAGER, nonce_slot(SENDER, key))
    }

    fn legacy_nonce(evm: &mut Evm<'_, BaseEvmTypes>) -> u64 {
        evm.state_mut().account(&SENDER).unwrap().nonce()
    }

    const fn first_use_cost(evm: &Evm<'_, BaseEvmTypes>) -> u64 {
        evm.version().gas_params.get(GasId::SstoreSetState)
    }

    fn assert_nonce_mismatch(evm: &mut Evm<'_, BaseEvmTypes>, tx: &Recovered<TxEnvelope>) {
        common::assert_validation_error(
            evm,
            tx,
            HandlerError::InvalidFrameTransaction(FrameTransactionError::NonceSequenceMismatch),
        );
    }

    #[test]
    fn empty_keys_select_account_nonce_while_zero_is_an_ordinary_storage_key() {
        for inspected in [false, true] {
            let mut legacy = evm(APPROVE_BOTH, &[], 5, inspected);
            legacy.overlay_db_mut().insert_account_storage(
                &NONCE_MANAGER,
                &nonce_slot(SENDER, U256::ZERO),
                &U256::from(7),
            );
            let result = legacy.transact(&approval(vec![], 5)).unwrap().commit();
            assert!(result.status);
            assert_eq!(result.state_gas_spent, 0);
            assert_eq!(legacy_nonce(&mut legacy), 6);
            assert_eq!(keyed_nonce(&mut legacy, U256::ZERO), U256::from(7));

            for keys in [vec![U256::ZERO], vec![U256::ZERO, U256::from(1)]] {
                let mut keyed = evm(APPROVE_BOTH, &[], 5, inspected);
                let cost = first_use_cost(&keyed) * keys.len() as u64;
                let result = keyed.transact(&approval(keys.clone(), 0)).unwrap().commit();
                assert!(result.status);
                assert_eq!(result.state_gas_spent, cost);
                assert_eq!(result.frame.unwrap().receipts[0].gas_used.state, cost);
                assert_eq!(legacy_nonce(&mut keyed), 5);
                for key in keys {
                    assert_eq!(keyed_nonce(&mut keyed, key), U256::from(1));
                }
            }
        }
    }

    #[test]
    fn single_use_consumes_every_fresh_key_to_max_and_rejects_all_replays() {
        for inspected in [false, true] {
            let mut evm = evm(APPROVE_BOTH, &[], 5, inspected);
            let keys = vec![U256::ZERO, U256::MAX];
            let cost = 2 * first_use_cost(&evm);
            let result = evm.transact(&approval(keys.clone(), u64::MAX)).unwrap().commit();
            assert!(result.status);
            assert_eq!(result.state_gas_spent, cost);
            assert_eq!(legacy_nonce(&mut evm), 5);
            for &key in &keys {
                assert_eq!(keyed_nonce(&mut evm, key), U256::from(u64::MAX));
                for sequence in [0, u64::MAX - 1, u64::MAX] {
                    assert_nonce_mismatch(&mut evm, &approval(vec![key], sequence));
                    assert_eq!(keyed_nonce(&mut evm, key), U256::from(u64::MAX));
                }
            }
            assert_nonce_mismatch(&mut evm, &approval(keys, u64::MAX));
        }
    }

    #[test]
    fn sequential_exhaustion_and_single_use_checks_preserve_full_storage_width() {
        for inspected in [false, true] {
            let key = U256::from(7);
            let mut evm = evm(APPROVE_BOTH, &[], 5, inspected);
            evm.overlay_db_mut().insert_account_storage(
                &NONCE_MANAGER,
                &nonce_slot(SENDER, key),
                &U256::from(u64::MAX - 1),
            );
            let result = evm.transact(&approval(vec![key], u64::MAX - 1)).unwrap().commit();
            assert!(result.status);
            assert_eq!(result.state_gas_spent, 0);
            assert_eq!(keyed_nonce(&mut evm, key), U256::from(u64::MAX));
            assert_nonce_mismatch(&mut evm, &approval(vec![key], u64::MAX));
            assert_eq!(legacy_nonce(&mut evm), 5);

            for (stored, sequence) in [
                (U256::from(1) << 64, u64::MAX),
                (U256::MAX, u64::MAX),
                ((U256::from(1) << 64) + U256::from(1), 1),
            ] {
                let mut evm = self::evm(APPROVE_BOTH, &[], 5, inspected);
                evm.overlay_db_mut().insert_account_storage(
                    &NONCE_MANAGER,
                    &nonce_slot(SENDER, key),
                    &stored,
                );
                assert_nonce_mismatch(&mut evm, &approval(vec![key], sequence));
                assert_eq!(keyed_nonce(&mut evm, key), stored);
            }
        }
    }

    #[test]
    fn one_used_key_rejects_single_use_without_consuming_any_fresh_key() {
        for inspected in [false, true] {
            for used in [U256::from(1), U256::from(u64::MAX)] {
                let mut evm = evm(APPROVE_BOTH, &[], 5, inspected);
                let fresh = U256::ZERO;
                let exhausted = U256::from(9);
                evm.overlay_db_mut().insert_account_storage(
                    &NONCE_MANAGER,
                    &nonce_slot(SENDER, exhausted),
                    &used,
                );
                assert_nonce_mismatch(&mut evm, &approval(vec![fresh, exhausted], u64::MAX));
                assert_eq!(keyed_nonce(&mut evm, fresh), U256::ZERO);
                assert_eq!(keyed_nonce(&mut evm, exhausted), used);
                assert_eq!(legacy_nonce(&mut evm), 5);
            }
        }
    }

    #[test]
    fn legacy_max_is_invalid_while_keyed_approval_ignores_an_exhausted_account_nonce() {
        for inspected in [false, true] {
            let mut evm = evm(APPROVE_BOTH, &[], u64::MAX, inspected);
            let legacy = approval(vec![], u64::MAX);
            assert!(matches!(evm.validate_tx(&legacy), Err(HandlerError::NonceOverflow)));
            assert!(matches!(evm.transact(&legacy), Err(HandlerError::NonceOverflow)));
            for (key, sequence) in [(U256::ZERO, 0), (U256::from(1), u64::MAX)] {
                let result = evm.transact(&approval(vec![key], sequence)).unwrap().commit();
                assert!(result.status);
                assert_eq!(keyed_nonce(&mut evm, key), U256::from(sequence.saturating_add(1)));
                assert_eq!(legacy_nonce(&mut evm), u64::MAX);
            }
        }
    }

    #[test]
    fn single_use_first_use_budget_is_checked_before_writing_any_key() {
        for inspected in [false, true] {
            let mut evm = evm(APPROVE_BOTH, &[], 5, inspected);
            let keys = vec![U256::ZERO, U256::from(7)];
            let cost = 2 * first_use_cost(&evm);
            let mut insufficient = frame(FrameMode::Verify, SENDER, 3);
            insufficient.limits.state = cost - 1;
            let tx = payload(keys.clone(), u64::MAX, vec![insufficient]);
            assert!(matches!(
                evm.transact(&tx),
                Err(HandlerError::InvalidFrameTransaction(
                    FrameTransactionError::VerifyFrameFailed
                ))
            ));
            for &key in &keys {
                assert_eq!(keyed_nonce(&mut evm, key), U256::ZERO);
            }
            let result = evm.transact(&approval(keys.clone(), u64::MAX)).unwrap().commit();
            assert!(result.status);
            assert_eq!(result.state_gas_spent, cost);
            for key in keys {
                assert_eq!(keyed_nonce(&mut evm, key), U256::from(u64::MAX));
            }
            assert_eq!(legacy_nonce(&mut evm), 5);
        }
    }

    // Nonempty input delegates APPROVE to BODY, then fails. Empty input approves directly.
    fn reverting_nested_approval(exit: &[u8]) -> Vec<u8> {
        common::delegating_sender(BODY, exit)
    }

    #[test]
    fn nested_parent_failure_restores_single_use_keys_and_allows_one_later_approval() {
        for inspected in [false, true] {
            for exit in [&[0x5f, 0x5f, 0xfd][..], &[0xfe][..]] {
                let mut evm = evm(&reverting_nested_approval(exit), APPROVE_BOTH, 5, inspected);
                let keys = vec![U256::ZERO, U256::from(7)];
                let cost = 2 * first_use_cost(&evm);
                let mut nested = frame(FrameMode::Default, SENDER, 3);
                nested.data = U256::from(1).to_be_bytes::<32>().to_vec().into();
                let tx = payload(
                    keys.clone(),
                    u64::MAX,
                    vec![nested, frame(FrameMode::Verify, SENDER, 3)],
                );
                let result = evm.transact(&tx).unwrap().commit();
                assert!(result.status);
                assert_eq!(result.state_gas_spent, cost);
                let outcome = result.frame.unwrap();
                assert_eq!(outcome.receipts[0].status, FrameStatus::Failure);
                assert_eq!(outcome.receipts[0].gas_used.state, 0);
                assert_eq!(outcome.receipts[1].status, FrameStatus::Success);
                assert_eq!(outcome.receipts[1].gas_used.state, cost);
                for key in keys {
                    assert_eq!(keyed_nonce(&mut evm, key), U256::from(u64::MAX));
                }
                assert_eq!(legacy_nonce(&mut evm), 5);
            }
        }
    }

    #[test]
    fn atomic_or_assertion_body_rollback_preserves_single_use_payment_consumption() {
        for inspected in [false, true] {
            for assertion in [false, true] {
                let mut evm = evm(APPROVE_BOTH, WRITE_BODY, 5, inspected);
                let keys = vec![U256::ZERO, U256::from(7)];
                let cost = 2 * first_use_cost(&evm);
                let tx = payload(
                    keys.clone(),
                    u64::MAX,
                    vec![
                        frame(FrameMode::Verify, SENDER, 3),
                        frame(FrameMode::Sender, BODY, if assertion { 0 } else { 4 }),
                        frame(
                            if assertion { FrameMode::Assert } else { FrameMode::Sender },
                            FAILURE,
                            0,
                        ),
                    ],
                );
                let result = evm.transact(&tx).unwrap().commit();
                assert_eq!(result.status, !assertion);
                assert_eq!(result.state_gas_spent, cost);
                let outcome = result.frame.unwrap();
                assert_eq!(outcome.receipts[0].gas_used.state, cost);
                assert_eq!(outcome.receipts[1].gas_used.state, 0);
                assert!(outcome.rolled_back[1]);
                assert_eq!(outcome.stops[2], InstrStop::Revert);
                assert_eq!(outcome.assertion_failure, assertion.then_some(2));
                assert_eq!(stored(&mut evm, BODY, U256::ZERO), U256::ZERO);
                for &key in &keys {
                    assert_eq!(keyed_nonce(&mut evm, key), U256::from(u64::MAX));
                }
                assert_eq!(legacy_nonce(&mut evm), 5);
                assert_nonce_mismatch(&mut evm, &approval(keys, u64::MAX));
            }
        }
    }

    #[test]
    fn repeated_prefix_and_discard_do_not_consume_single_use_keys_before_commit() {
        for inspected in [false, true] {
            let mut evm = evm(APPROVE_BOTH, &[], 5, inspected);
            let keys = vec![U256::ZERO, U256::MAX];
            let tx = approval(keys.clone(), u64::MAX);
            let cost = 2 * first_use_cost(&evm);
            for _ in 0..3 {
                let prefix = evm.validate_frame_prefix(&tx, 1).unwrap();
                assert!(prefix.sender_approved);
                assert_eq!(prefix.state_gas, cost);
                let result = evm.transact(&tx).unwrap().discard();
                assert!(result.status);
                assert_eq!(result.state_gas_spent, cost);
                for &key in &keys {
                    assert_eq!(keyed_nonce(&mut evm, key), U256::ZERO);
                }
                assert_eq!(legacy_nonce(&mut evm), 5);
            }
            assert!(evm.transact(&tx).unwrap().commit().status);
            for key in keys {
                assert_eq!(keyed_nonce(&mut evm, key), U256::from(u64::MAX));
            }
        }
    }

    #[test]
    fn zero_and_single_use_keys_reconstruct_the_same_bal_in_positioned_execution() {
        for inspected in [false, true] {
            let transactions = [
                approval(vec![], 5),
                approval(vec![U256::ZERO], u64::MAX),
                approval(vec![U256::from(1)], 0),
                approval(vec![U256::from(2)], u64::MAX),
                approval(vec![U256::from(1)], 1),
            ];
            let mut serial = evm(APPROVE_BOTH, &[], 5, inspected);
            let cost = first_use_cost(&serial);
            serial.state_mut().enable_bal_builder();
            serial.state_mut().reset_bal_index();
            let mut expected = Vec::new();
            for tx in &transactions {
                serial.state_mut().bump_bal_index();
                expected.push(serial.transact(tx).unwrap().commit());
            }
            assert_eq!(
                expected.iter().map(|result| result.state_gas_spent).collect::<Vec<_>>(),
                [0, cost, cost, cost, 0]
            );
            let bal = Arc::new(serial.state_mut().take_bal_builder().unwrap());
            let mut rebuilt = Bal::new();
            for (index, tx) in transactions.iter().enumerate() {
                let bal_index = BlockAccessIndex::new(index as u64 + 1);
                let mut positioned = evm(APPROVE_BOTH, &[], 5, inspected);
                positioned.state_mut().set_bal(bal.clone());
                positioned.state_mut().set_bal_index(bal_index);
                let detached = positioned.transact(tx).unwrap().detach();
                assert_eq!(detached.result, expected[index]);
                rebuilt.commit(bal_index, detached.pending_state);
            }
            assert_eq!(rebuilt, *bal);
            assert_eq!(legacy_nonce(&mut serial), 6);
            assert_eq!(keyed_nonce(&mut serial, U256::ZERO), U256::from(u64::MAX));
            assert_eq!(keyed_nonce(&mut serial, U256::from(1)), U256::from(2));
            assert_eq!(keyed_nonce(&mut serial, U256::from(2)), U256::from(u64::MAX));
            assert_nonce_mismatch(
                &mut serial,
                &approval(vec![U256::ZERO, U256::from(3)], u64::MAX),
            );
            assert_eq!(keyed_nonce(&mut serial, U256::from(3)), U256::ZERO);
        }
    }

    #[test]
    fn nonce_introspection_returns_zero_first_key_for_the_empty_legacy_set() {
        let mut code = Vec::new();
        for (index, selector) in [1, 13, 14, 15, 16].into_iter().enumerate() {
            code.extend_from_slice(&[0x60, selector, 0xb0, 0x60, (index * 32) as u8, 0x52]);
        }
        code.extend_from_slice(&[0x60, 160, 0x5f, 0xf3]);
        for inspected in [false, true] {
            for keys in [vec![], vec![U256::ZERO], vec![U256::from(7)]] {
                let mut evm = evm(APPROVE_BOTH, &code, 5, inspected);
                let sequence = if keys.is_empty() { 5 } else { 0 };
                let result = evm
                    .transact(&payload(
                        keys.clone(),
                        sequence,
                        vec![
                            frame(FrameMode::Verify, SENDER, 3),
                            frame(FrameMode::Default, BODY, 0),
                        ],
                    ))
                    .unwrap()
                    .discard();
                assert!(result.status, "keys={keys:?}, inspected={inspected}");
                let output = &result.frame.unwrap().outputs[1];
                let actual = output
                    .as_chunks::<32>()
                    .0
                    .iter()
                    .copied()
                    .map(U256::from_be_bytes)
                    .collect::<Vec<_>>();
                assert_eq!(
                    actual,
                    [
                        U256::from(sequence),
                        U256::from(5),
                        U256::from(keys.len()),
                        U256::from_be_bytes(nonce_keys_hash(&keys).0),
                        keys.first().copied().unwrap_or_default(),
                    ]
                );
            }
        }
    }

    #[test]
    fn empty_key_nonce_errors_match_revm_without_consuming_the_nonce() {
        for inspected in [false, true] {
            for (sequence, message) in
                [(4, "nonce 4 too low, expected 5"), (6, "nonce 6 too high, expected 5")]
            {
                for entry in ["validate", "execute", "prefix"] {
                    let mut evm = evm(APPROVE_BOTH, &[], 5, inspected);
                    let tx = approval(vec![], sequence);
                    let error = match entry {
                        "validate" => evm.validate_tx(&tx).unwrap_err(),
                        "execute" => evm.transact(&tx).unwrap_err(),
                        _ => evm.validate_frame_prefix(&tx, 1).unwrap_err(),
                    };
                    assert_eq!(error, HandlerError::InvalidNonce { expected: 5, got: sequence });
                    assert_eq!(error.to_string(), message);
                    assert_eq!(legacy_nonce(&mut evm), 5);
                    assert!(evm.logs().is_empty());
                }
            }
        }
    }
}
