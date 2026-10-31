//! Execution-specs exception names for the transaction and block errors the runners report.
//!
//! A fixture's `expectException` lists the names a client may report, separated by `|`. A runner
//! passes an invalid transaction or block only when its error maps to one of them.

use evm2::{Version, registry::HandlerError};
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
    GasAllowanceExceeded => [
        "TransactionException.GAS_ALLOWANCE_EXCEEDED",
        "TR_GasLimitReached",
        "TR_FeeCapLessThanBlocksORGasLimitReached"
    ],
    GasLimitExceedsMaximum => ["TransactionException.GAS_LIMIT_EXCEEDS_MAXIMUM"],
    // The runner cannot build a fee above `u128`, so it reports one as this rule: a fee that large
    // exceeds any balance or overflows the gas limit product.
    GasPriceExceedsU128 => [
        "TransactionException.INSUFFICIENT_ACCOUNT_FUNDS",
        "TransactionException.GASLIMIT_PRICE_PRODUCT_OVERFLOW",
        "TR_NoFundsX"
    ],
    InitcodeSizeExceeded => [
        "TransactionException.INITCODE_SIZE_EXCEEDED",
        "TR_InitCodeLimitExceeded"
    ],
    InsufficientAccountFunds => [
        "TransactionException.INSUFFICIENT_ACCOUNT_FUNDS",
        "TR_NoFunds",
        "TR_NoFundsOrGas",
        "TR_FeeCapLessThanBlocksORNoFunds"
    ],
    InsufficientMaxFeePerBlobGas => ["TransactionException.INSUFFICIENT_MAX_FEE_PER_BLOB_GAS"],
    InsufficientMaxFeePerGas => [
        "TransactionException.INSUFFICIENT_MAX_FEE_PER_GAS",
        "TR_FeeCapLessThanBlocks",
        "TR_FeeCapLessThanBlocksORNoFunds",
        "TR_FeeCapLessThanBlocksORGasLimitReached"
    ],
    IntrinsicGasTooLow => [
        "TransactionException.INTRINSIC_GAS_TOO_LOW",
        "TransactionException.INTRINSIC_GAS_BELOW_FLOOR_GAS_COST",
        "TR_IntrinsicGas",
        "IntrinsicGas",
        "TR_NoFundsOrGas"
    ],
    InvalidChainid => ["TransactionException.INVALID_CHAINID"],
    InvalidSignatureVrs => ["TransactionException.INVALID_SIGNATURE_VRS"],
    NonceIsMax => ["TransactionException.NONCE_IS_MAX", "TR_NonceHasMaxValue"],
    NonceMismatchTooHigh => ["TransactionException.NONCE_MISMATCH_TOO_HIGH", "TR_NonceTooHigh"],
    NonceMismatchTooLow => ["TransactionException.NONCE_MISMATCH_TOO_LOW", "TR_NonceTooLow"],
    PriorityGreaterThanMaxFeePerGas => [
        "TransactionException.PRIORITY_GREATER_THAN_MAX_FEE_PER_GAS",
        "TR_TipGtFeeCap"
    ],
    RlpInvalidValue => ["TransactionException.RLP_INVALID_VALUE", "TR_RLP_WRONGVALUE"],
    SenderNotEoa => ["TransactionException.SENDER_NOT_EOA", "SenderNotEOA"],
    Type1TxPreFork => ["TransactionException.TYPE_1_TX_PRE_FORK", "TR_TypeNotSupported"],
    Type2TxPreFork => ["TransactionException.TYPE_2_TX_PRE_FORK", "TR_TypeNotSupported"],
    Type3TxBlobCountExceeded => [
        "TransactionException.TYPE_3_TX_BLOB_COUNT_EXCEEDED",
        "TR_BLOBLIST_OVERSIZE"
    ],
    Type3TxContractCreation => [
        "TransactionException.TYPE_3_TX_CONTRACT_CREATION",
        "TR_BLOBCREATE"
    ],
    Type3TxInvalidBlobVersionedHash => [
        "TransactionException.TYPE_3_TX_INVALID_BLOB_VERSIONED_HASH",
        "TR_BLOBVERSION_INVALID"
    ],
    Type3TxMaxBlobGasAllowanceExceeded => [
        "TransactionException.TYPE_3_TX_MAX_BLOB_GAS_ALLOWANCE_EXCEEDED"
    ],
    Type3TxPreFork => ["TransactionException.TYPE_3_TX_PRE_FORK", "TR_TypeNotSupported"],
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
    pub(crate) const fn from_handler_error(
        error: &HandlerError,
        version: &Version,
    ) -> Option<Self> {
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
            HandlerError::InsufficientFunds { .. } => Self::InsufficientAccountFunds,
            HandlerError::RejectCallerWithCode => Self::SenderNotEoa,
            HandlerError::NonceOverflow => Self::NonceIsMax,
            HandlerError::GasLimitMoreThanBlock { .. } => Self::GasAllowanceExceeded,
            // Under EIP-8037 the handler reports both the cap on the total gas limit and the cap
            // on the intrinsic execution gas with this error. Execution-specs treats the second as
            // an intrinsic gas failure.
            HandlerError::TxGasLimitGreaterThanCap { cap, .. }
                if *cap == version.tx_gas_caps().0 =>
            {
                Self::GasLimitExceedsMaximum
            }
            HandlerError::TxGasLimitGreaterThanCap { .. } => Self::IntrinsicGasTooLow,
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
    use evm2::SpecId;

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
    fn gas_cap_error_names_the_rule_it_enforces() {
        let amsterdam = Version::base(SpecId::AMSTERDAM);
        let (total_cap, execution_cap) = amsterdam.tx_gas_caps();
        let error = |cap| HandlerError::TxGasLimitGreaterThanCap { gas_limit: cap + 1, cap };

        assert_eq!(
            Exception::from_handler_error(&error(total_cap), amsterdam),
            Some(Exception::GasLimitExceedsMaximum)
        );
        assert_eq!(
            Exception::from_handler_error(&error(execution_cap), amsterdam),
            Some(Exception::IntrinsicGasTooLow)
        );
    }

    #[test]
    fn nonce_mismatch_names_the_side_it_is_on() {
        let version = Version::base(SpecId::CANCUN);
        let error = |expected, got| HandlerError::InvalidNonce { expected, got };

        assert_eq!(
            Exception::from_handler_error(&error(1, 2), version),
            Some(Exception::NonceMismatchTooHigh)
        );
        assert_eq!(
            Exception::from_handler_error(&error(2, 1), version),
            Some(Exception::NonceMismatchTooLow)
        );
    }
}
