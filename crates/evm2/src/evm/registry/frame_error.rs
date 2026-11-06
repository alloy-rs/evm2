//! Typed frame transaction validation and execution failures.

use thiserror::Error;

/// A protocol validation or execution failure in an EIP-8141 frame transaction.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum FrameTransactionError {
    /// EIP-8141 derived gas limit overflow.
    #[error("EIP-8141 derived gas limit overflow")]
    GasLimitOverflow,
    /// Gas reservations or payment arithmetic overflowed.
    #[error("overflow payment in transaction")]
    PaymentOverflow,
    /// The execution or blob base-fee reservation overflowed.
    #[error("gas price is less than basefee")]
    BaseFeeOverflow,
    /// Frame nonce payload does not match EIP-8250 activation.
    #[error("frame nonce payload does not match EIP-8250 activation")]
    NoncePayloadMismatch,
    /// EIP-8141 frame count must be in 1..=64.
    #[error("EIP-8141 frame count must be in 1..=64")]
    InvalidFrameCount,
    /// EIP-8250 nonce sequence mismatch.
    #[error("EIP-8250 nonce sequence mismatch")]
    NonceSequenceMismatch,
    /// EIP-8141 validation prefix is outside frame bounds.
    #[error("EIP-8141 validation prefix is outside frame bounds")]
    PrefixOutOfBounds,
    /// EIP-8141 validation prefix boundary mismatch.
    #[error("EIP-8141 validation prefix boundary mismatch")]
    PrefixBoundaryMismatch,
    /// EIP-8141 explicit signature message must be a non-zero 32-byte digest.
    #[error("EIP-8141 explicit signature message must be a non-zero 32-byte digest")]
    InvalidSignatureMessage,
    /// EIP-8141 signature signer does not match.
    #[error("EIP-8141 signature signer does not match")]
    SignatureSignerMismatch,
    /// EIP-8141 signature validation failed.
    #[error("EIP-8141 signature validation failed")]
    SignatureValidationFailed,
    /// EIP-8141 reserved frame flag is set.
    #[error("EIP-8141 reserved frame flag is set")]
    ReservedFrameFlag,
    /// EIP-7906 ASSERT frames must form a trailing suffix.
    #[error("EIP-7906 ASSERT frames must form a trailing suffix")]
    InvalidAssertionSuffix,
    /// Only EIP-8141 SENDER frames may transfer value.
    #[error("only EIP-8141 SENDER frames may transfer value")]
    ValueTransferNotAllowed,
    /// EIP-7906 atomic flag is invalid on ASSERT frames.
    #[error("EIP-7906 atomic flag is invalid on ASSERT frames")]
    AtomicAssertion,
    /// An EIP-7906 atomic batch ends on an ASSERT frame.
    #[error("EIP-7906 atomic batch cannot end on a ASSERT frame")]
    AtomicBatchEndsOnAssertion,
    /// EIP-8141 execution approval target must be the sender.
    #[error("EIP-8141 execution approval target must be the sender")]
    InvalidExecutionApprovalTarget,
    /// EIP-8141 atomic flag is invalid on VERIFY frames.
    #[error("EIP-8141 atomic flag is invalid on VERIFY frames")]
    AtomicVerify,
    /// EIP-8141 atomic batch must be followed by a non-VERIFY frame.
    #[error("EIP-8141 atomic batch must be followed by a non-VERIFY frame")]
    InvalidAtomicBatchEnd,
    /// EIP-8141 atomic batch frames cannot carry approval scope.
    #[error("EIP-8141 atomic batch frames cannot carry approval scope")]
    AtomicBatchApproval,
    /// Malformed EIP-8141 expiry verifier frame.
    #[error("malformed EIP-8141 expiry verifier frame")]
    MalformedExpiryVerifier,
    /// Multiple EIP-8141 expiry verifier frames.
    #[error("multiple EIP-8141 expiry verifier frames")]
    MultipleExpiryVerifiers,
    /// EIP-8141 arbitrary signature signer must be empty.
    #[error("EIP-8141 arbitrary signature signer must be empty")]
    UnexpectedSignatureSigner,
    /// EIP-8141 SENDER frame executed before execution approval.
    #[error("EIP-8141 SENDER frame executed before execution approval")]
    SenderNotApproved,
    /// EIP-8141 VERIFY frame failed.
    #[error("EIP-8141 VERIFY frame failed")]
    VerifyFrameFailed,
    /// EIP-8141 validation prefix contains an unsuccessful frame.
    #[error("EIP-8141 validation prefix contains an unsuccessful frame")]
    UnsuccessfulPrefixFrame,
    /// EIP-8141 transaction did not approve a payer.
    #[error("EIP-8141 transaction did not approve a payer")]
    MissingPayer,
    /// Validation prefix requires an EIP-8141 transaction.
    #[error("validation prefix requires an EIP-8141 transaction")]
    PrefixRequiresFrameTransaction,
    /// EIP-8250 requires at most 16 nonce keys.
    #[error("EIP-8250 requires at most 16 nonce keys")]
    TooManyNonceKeys,
    /// EIP-8250 nonce keys must be strictly increasing.
    #[error("EIP-8250 nonce keys must be strictly increasing")]
    NonceKeysNotIncreasing,
}
