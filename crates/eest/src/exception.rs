//! Execution-specs exception names for the transaction and block errors the runners report.
//!
//! A fixture's `expectException` lists the names a client may report, separated by `|`. A runner
//! passes an invalid transaction or block only when its error maps to one of them.

use evm2::registry::HandlerError;
use std::fmt;

macro_rules! exceptions {
    ($($variant:ident => [$first:literal $(, $name:literal)*],)*) => {
        /// A rule execution-specs enforces on a transaction or block, with every exception name
        /// a fixture uses for it.
        ///
        /// The first name is the `TransactionException` or `BlockException` execution-specs
        /// raises. The others name the same failure in the shared `ethereum/tests` fixtures and
        /// in the checks execution-specs folds into one rule.
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        pub(crate) enum Exception {
            $(
                #[doc = $first]
                $variant,
            )*
        }

        impl Exception {
            /// Returns every name a fixture uses for this exception.
            const fn names(self) -> &'static [&'static str] {
                match self {
                    $(Self::$variant => &[$first $(, $name)*],)*
                }
            }
        }
    };
}

exceptions! {
    // `TR_GasLimitReached` is the `ethereum/tests` name of the same rule.
    GasAllowanceExceeded => ["TransactionException.GAS_ALLOWANCE_EXCEEDED", "TR_GasLimitReached"],
    GasLimitExceedsMaximum => ["TransactionException.GAS_LIMIT_EXCEEDS_MAXIMUM"],
    // `TR_InitCodeLimitExceeded` is the `ethereum/tests` name of the same rule.
    InitcodeSizeExceeded => [
        "TransactionException.INITCODE_SIZE_EXCEEDED",
        "TR_InitCodeLimitExceeded"
    ],
    // `TR_NoFunds` and `TR_NoFundsX` are the `ethereum/tests` names of the same rule. The fixtures
    // that name `TR_NoFundsOrGas` leave the order of the funds and gas limit checks open.
    InsufficientAccountFunds => [
        "TransactionException.INSUFFICIENT_ACCOUNT_FUNDS",
        "TR_NoFunds",
        "TR_NoFundsX",
        "TR_NoFundsOrGas"
    ],
    InsufficientMaxFeePerBlobGas => ["TransactionException.INSUFFICIENT_MAX_FEE_PER_BLOB_GAS"],
    // `TR_FeeCapLessThanBlocks` is the `ethereum/tests` name of the same rule.
    InsufficientMaxFeePerGas => [
        "TransactionException.INSUFFICIENT_MAX_FEE_PER_GAS",
        "TR_FeeCapLessThanBlocks"
    ],
    // `TR_IntrinsicGas` and `IntrinsicGas` are the `ethereum/tests` names of the same rule. The
    // fixtures that name `TR_NoFundsOrGas` leave the order of the funds and gas limit checks open.
    IntrinsicGasTooLow => [
        "TransactionException.INTRINSIC_GAS_TOO_LOW",
        "TR_IntrinsicGas",
        "IntrinsicGas",
        "TR_NoFundsOrGas"
    ],
    // Execution-specs checks `intrinsic > tx.gas` and `calldata_floor > tx.gas` as separate steps
    // that raise one `InsufficientTransactionGasError` (`validate_transaction`), and fixtures name
    // a limit that is below the floor but not the intrinsic gas either way.
    GasLimitBelowFloor => [
        "TransactionException.INTRINSIC_GAS_BELOW_FLOOR_GAS_COST",
        "TransactionException.INTRINSIC_GAS_TOO_LOW",
        "TR_IntrinsicGas",
        "IntrinsicGas",
        "TR_NoFundsOrGas"
    ],
    InvalidChainid => ["TransactionException.INVALID_CHAINID"],
    InvalidSignatureVrs => ["TransactionException.INVALID_SIGNATURE_VRS"],
    // `TR_NonceHasMaxValue` is the `ethereum/tests` name of the same rule.
    NonceIsMax => ["TransactionException.NONCE_IS_MAX", "TR_NonceHasMaxValue"],
    NonceMismatchTooHigh => ["TransactionException.NONCE_MISMATCH_TOO_HIGH"],
    NonceMismatchTooLow => ["TransactionException.NONCE_MISMATCH_TOO_LOW"],
    // `TR_TipGtFeeCap` is the `ethereum/tests` name of the same rule.
    PriorityGreaterThanMaxFeePerGas => [
        "TransactionException.PRIORITY_GREATER_THAN_MAX_FEE_PER_GAS",
        "TR_TipGtFeeCap"
    ],
    // `TR_RLP_WRONGVALUE` is the `ethereum/tests` name of the same rule.
    RlpInvalidValue => ["TransactionException.RLP_INVALID_VALUE", "TR_RLP_WRONGVALUE"],
    // `SenderNotEOA` is the `ethereum/tests` name of the same rule.
    SenderNotEoa => ["TransactionException.SENDER_NOT_EOA", "SenderNotEOA"],
    // `TR_TypeNotSupported` is the `ethereum/tests` name for any transaction type before its fork.
    Type1TxPreFork => ["TransactionException.TYPE_1_TX_PRE_FORK", "TR_TypeNotSupported"],
    Type2TxPreFork => ["TransactionException.TYPE_2_TX_PRE_FORK", "TR_TypeNotSupported"],
    // `TR_BLOBLIST_OVERSIZE` is the `ethereum/tests` name of the same rule.
    Type3TxBlobCountExceeded => [
        "TransactionException.TYPE_3_TX_BLOB_COUNT_EXCEEDED",
        "TR_BLOBLIST_OVERSIZE"
    ],
    // `TR_BLOBCREATE` is the `ethereum/tests` name of the same rule.
    Type3TxContractCreation => [
        "TransactionException.TYPE_3_TX_CONTRACT_CREATION",
        "TR_BLOBCREATE"
    ],
    // `TR_BLOBVERSION_INVALID` is the `ethereum/tests` name of the same rule.
    Type3TxInvalidBlobVersionedHash => [
        "TransactionException.TYPE_3_TX_INVALID_BLOB_VERSIONED_HASH",
        "TR_BLOBVERSION_INVALID"
    ],
    Type3TxMaxBlobGasAllowanceExceeded => [
        "TransactionException.TYPE_3_TX_MAX_BLOB_GAS_ALLOWANCE_EXCEEDED"
    ],
    Type3TxPreFork => ["TransactionException.TYPE_3_TX_PRE_FORK", "TR_TypeNotSupported"],
    // `TR_EMPTYBLOB` is the `ethereum/tests` name of the same rule.
    Type3TxZeroBlobs => ["TransactionException.TYPE_3_TX_ZERO_BLOBS", "TR_EMPTYBLOB"],
    Type4EmptyAuthorizationList => ["TransactionException.TYPE_4_EMPTY_AUTHORIZATION_LIST"],
    Type4TxContractCreation => ["TransactionException.TYPE_4_TX_CONTRACT_CREATION"],
    Type4TxPreFork => ["TransactionException.TYPE_4_TX_PRE_FORK", "TR_TypeNotSupported"],

    BlockAccessListGasLimitExceeded => ["BlockException.BLOCK_ACCESS_LIST_GAS_LIMIT_EXCEEDED"],
    IncorrectBlobGasUsed => ["BlockException.INCORRECT_BLOB_GAS_USED"],
    IncorrectBlockFormat => ["BlockException.INCORRECT_BLOCK_FORMAT"],
    IncorrectExcessBlobGas => ["BlockException.INCORRECT_EXCESS_BLOB_GAS"],
    InvalidBalHash => ["BlockException.INVALID_BAL_HASH"],
    InvalidBlockAccessList => ["BlockException.INVALID_BLOCK_ACCESS_LIST"],
    // EIP-7928 requires sorted, unique entries, but execution-specs only compares the list hash.
    // The EEST fixtures name the same violation `INCORRECT_BLOCK_FORMAT` (`bal_invalid_account_order`,
    // `bal_invalid_duplicate_account`) and `INVALID_BLOCK_ACCESS_LIST` (`bal_invalid_account`,
    // `bal_invalid_extraneous_entries`), so neither name alone fits.
    MalformedBlockAccessList => [
        "BlockException.INCORRECT_BLOCK_FORMAT",
        "BlockException.INVALID_BLOCK_ACCESS_LIST"
    ],
    InvalidGasUsed => ["BlockException.INVALID_GAS_USED"],
    InvalidReceiptsRoot => ["BlockException.INVALID_RECEIPTS_ROOT"],
    RlpBlockLimitExceeded => ["BlockException.RLP_BLOCK_LIMIT_EXCEEDED"],
    SystemContractCallFailed => ["BlockException.SYSTEM_CONTRACT_CALL_FAILED"],
    SystemContractEmpty => ["BlockException.SYSTEM_CONTRACT_EMPTY"],
    UnknownParent => ["BlockException.UNKNOWN_PARENT"],
    UnknownParentZero => ["BlockException.UNKNOWN_PARENT_ZERO"],
}

impl Exception {
    /// Maps a transaction validation error to the exception execution-specs raises for it.
    ///
    /// Returns `None` for errors that are not a transaction validity failure, such as a database
    /// error.
    pub(crate) const fn from_handler_error(error: &HandlerError) -> Option<Self> {
        Some(match error {
            HandlerError::Database(_) | HandlerError::Fatal(_) | HandlerError::External(_) => {
                return None;
            }
            HandlerError::UnsupportedTransactionType(1) => Self::Type1TxPreFork,
            HandlerError::UnsupportedTransactionType(2) => Self::Type2TxPreFork,
            HandlerError::UnsupportedTransactionType(3) => Self::Type3TxPreFork,
            HandlerError::UnsupportedTransactionType(4) => Self::Type4TxPreFork,
            HandlerError::UnsupportedTransactionType(_)
            | HandlerError::WrongTransactionType { .. }
            | HandlerError::MissingChainId => return None,
            HandlerError::InvalidNonce { expected, got } if *got > *expected => {
                Self::NonceMismatchTooHigh
            }
            HandlerError::InvalidNonce { .. } => Self::NonceMismatchTooLow,
            HandlerError::InvalidChainId { .. } => Self::InvalidChainid,
            HandlerError::IntrinsicGasTooLow { .. } => Self::IntrinsicGasTooLow,
            HandlerError::GasLimitBelowFloor { .. } => Self::GasLimitBelowFloor,
            HandlerError::InsufficientFunds { .. } => Self::InsufficientAccountFunds,
            HandlerError::RejectCallerWithCode => Self::SenderNotEoa,
            HandlerError::NonceOverflow => Self::NonceIsMax,
            HandlerError::GasLimitMoreThanBlock { .. } => Self::GasAllowanceExceeded,
            HandlerError::TxGasLimitGreaterThanCap { .. } => Self::GasLimitExceedsMaximum,
            // Execution-specs raises `InsufficientTransactionGasError` for it.
            HandlerError::IntrinsicGasGreaterThanCap { .. } => Self::IntrinsicGasTooLow,
            HandlerError::CreateInitCodeSizeLimit { .. } => Self::InitcodeSizeExceeded,
            HandlerError::FeeCapLessThanBaseFee { .. } => Self::InsufficientMaxFeePerGas,
            HandlerError::EmptyAuthorizationList => Self::Type4EmptyAuthorizationList,
            HandlerError::BlobFeeCapLessThanBlobBaseFee { .. } => {
                Self::InsufficientMaxFeePerBlobGas
            }
            HandlerError::EmptyBlobs => Self::Type3TxZeroBlobs,
            HandlerError::TooManyBlobs { .. } => Self::Type3TxBlobCountExceeded,
            HandlerError::BlobVersionNotSupported => Self::Type3TxInvalidBlobVersionedHash,
            HandlerError::PriorityFeeGreaterThanMaxFee => Self::PriorityGreaterThanMaxFeePerGas,
        })
    }

    /// Returns the exception execution-specs raises for a transaction of `tx_type` without a
    /// recipient.
    pub(crate) const fn contract_creation(tx_type: u8) -> Option<Self> {
        match tx_type {
            3 => Some(Self::Type3TxContractCreation),
            4 => Some(Self::Type4TxContractCreation),
            _ => None,
        }
    }

    /// Returns whether `expected`, a fixture's `|`-separated `expectException`, lists this
    /// exception.
    fn is_expected(self, expected: &str) -> bool {
        expected.split('|').any(|name| self.names().contains(&name))
    }
}

impl fmt::Display for Exception {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.names()[0])
    }
}

/// Checks that a rejection `error`, which execution-specs raises as `exception`, is one of the
/// `|`-separated names in the fixture's `expected`.
///
/// On a mismatch, returns the error described for a failure report, led by its exception name.
pub(crate) fn check(
    expected: &str,
    exception: Option<Exception>,
    error: &dyn fmt::Display,
) -> Result<(), String> {
    match exception {
        Some(exception) if exception.is_expected(expected) => Ok(()),
        Some(exception) => Err(format!("{exception} ({error})")),
        None => Err(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expected_exception_lists_alternatives() {
        let expected =
            "TransactionException.NONCE_IS_MAX|TransactionException.INTRINSIC_GAS_TOO_LOW";

        assert!(Exception::NonceIsMax.is_expected(expected));
        assert!(Exception::IntrinsicGasTooLow.is_expected(expected));
        assert!(!Exception::InsufficientAccountFunds.is_expected(expected));
        assert!(!Exception::NonceIsMax.is_expected("TransactionException.NONCE_IS_MAX_2"));
    }

    #[test]
    fn gas_cap_errors_name_the_rule_they_enforce() {
        assert_eq!(
            Exception::from_handler_error(&HandlerError::TxGasLimitGreaterThanCap {
                gas_limit: 2,
                cap: 1
            }),
            Some(Exception::GasLimitExceedsMaximum)
        );
        assert_eq!(
            Exception::from_handler_error(&HandlerError::IntrinsicGasGreaterThanCap {
                required: 2,
                cap: 1
            }),
            Some(Exception::IntrinsicGasTooLow)
        );
    }

    #[test]
    fn nonce_mismatch_names_the_side_it_is_on() {
        let error = |expected, got| HandlerError::InvalidNonce { expected, got };

        assert_eq!(
            Exception::from_handler_error(&error(1, 2)),
            Some(Exception::NonceMismatchTooHigh)
        );
        assert_eq!(
            Exception::from_handler_error(&error(2, 1)),
            Some(Exception::NonceMismatchTooLow)
        );
    }
}
