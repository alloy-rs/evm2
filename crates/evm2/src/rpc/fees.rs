use alloy_primitives::{B256, U256};
use core::cmp::min;
use thiserror::Error;

/// Helper type for representing the fees of a `TransactionRequest`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CallFees {
    /// EIP-1559 priority fee.
    pub max_priority_fee_per_gas: Option<U256>,
    /// Unified gas price setting.
    ///
    /// Will be the configured `basefee` if unset in the request.
    ///
    /// `gasPrice` for legacy,
    /// `maxFeePerGas` for EIP-1559.
    pub gas_price: U256,
    /// Max fee per blob gas for EIP-4844 transactions.
    pub max_fee_per_blob_gas: Option<U256>,
}

impl CallFees {
    /// Ensures the fields of a `TransactionRequest` are not conflicting.
    ///
    /// # EIP-4844 transactions
    ///
    /// Blob transactions have an additional fee parameter `maxFeePerBlobGas`.
    /// If the `maxFeePerBlobGas` or `blobVersionedHashes` are set we treat it as an EIP-4844
    /// transaction.
    ///
    /// Note: `block_blob_fee` is only used for requests with blob versioned hashes. A `None`
    /// leaves the resulting `max_fee_per_blob_gas` unset unless the request provides one.
    ///
    /// ## Notable design decisions
    ///
    /// For compatibility reasons, this contains several exceptions when fee values are validated:
    /// - If both `maxFeePerGas` and `maxPriorityFeePerGas` are set to `0` they are treated as
    ///   missing values, bypassing fee checks wrt. `baseFeePerGas`.
    ///
    /// This mirrors geth's behaviour when transaction requests are executed: <https://github.com/ethereum/go-ethereum/blob/380688c636a654becc8f114438c2a5d93d2db032/core/state_transition.go#L306-L306>
    pub fn ensure_fees(
        call_gas_price: Option<U256>,
        call_max_fee: Option<U256>,
        call_priority_fee: Option<U256>,
        block_base_fee: U256,
        blob_versioned_hashes: Option<&[B256]>,
        max_fee_per_blob_gas: Option<U256>,
        block_blob_fee: Option<U256>,
    ) -> Result<Self, CallFeesError> {
        /// Get the effective gas price of a transaction as specified in EIP-1559 with relevant
        /// checks.
        fn get_effective_gas_price(
            max_fee_per_gas: Option<U256>,
            max_priority_fee_per_gas: Option<U256>,
            block_base_fee: U256,
        ) -> Result<U256, CallFeesError> {
            match max_fee_per_gas {
                Some(max_fee) => {
                    let max_priority_fee_per_gas = max_priority_fee_per_gas.unwrap_or(U256::ZERO);

                    // Only enforce the fee cap if provided input is not zero.
                    if !(max_fee.is_zero() && max_priority_fee_per_gas.is_zero())
                        && max_fee < block_base_fee
                    {
                        // `base_fee_per_gas` is greater than the `max_fee_per_gas`.
                        return Err(CallFeesError::FeeCapTooLow);
                    }
                    if max_fee < max_priority_fee_per_gas {
                        return Err(
                            // `max_priority_fee_per_gas` is greater than the `max_fee_per_gas`.
                            CallFeesError::TipAboveFeeCap,
                        );
                    }
                    // ref <https://github.com/ethereum/go-ethereum/blob/0dd173a727dd2d2409b8e401b22e85d20c25b71f/internal/ethapi/transaction_args.go#L446-L446>
                    Ok(min(
                        max_fee,
                        block_base_fee
                            .checked_add(max_priority_fee_per_gas)
                            .ok_or(CallFeesError::TipVeryHigh)?,
                    ))
                }
                None => Ok(block_base_fee
                    .checked_add(max_priority_fee_per_gas.unwrap_or(U256::ZERO))
                    .ok_or(CallFeesError::TipVeryHigh)?),
            }
        }

        let has_blob_hashes =
            blob_versioned_hashes.as_ref().map(|blobs| !blobs.is_empty()).unwrap_or(false);

        match (call_gas_price, call_max_fee, call_priority_fee, max_fee_per_blob_gas) {
            (gas_price, None, None, None) => {
                // Either legacy transaction or no fee fields are specified.
                // When no fields are specified, set gas price to zero.
                let gas_price = gas_price.unwrap_or(U256::ZERO);
                Ok(Self {
                    gas_price,
                    max_priority_fee_per_gas: None,
                    max_fee_per_blob_gas: has_blob_hashes.then_some(block_blob_fee).flatten(),
                })
            }
            (None, max_fee_per_gas, max_priority_fee_per_gas, None) => {
                // Request for EIP-1559 transaction.
                let effective_gas_price = get_effective_gas_price(
                    max_fee_per_gas,
                    max_priority_fee_per_gas,
                    block_base_fee,
                )?;
                let max_fee_per_blob_gas = has_blob_hashes.then_some(block_blob_fee).flatten();

                Ok(Self {
                    gas_price: effective_gas_price,
                    max_priority_fee_per_gas,
                    max_fee_per_blob_gas,
                })
            }
            (None, max_fee_per_gas, max_priority_fee_per_gas, Some(max_fee_per_blob_gas)) => {
                // Request for EIP-4844 transaction.
                let effective_gas_price = get_effective_gas_price(
                    max_fee_per_gas,
                    max_priority_fee_per_gas,
                    block_base_fee,
                )?;
                // Ensure blob hashes are present.
                if !has_blob_hashes {
                    // Blob transaction but no blob hashes.
                    return Err(CallFeesError::BlobTransactionMissingBlobHashes);
                }

                Ok(Self {
                    gas_price: effective_gas_price,
                    max_priority_fee_per_gas,
                    max_fee_per_blob_gas: Some(max_fee_per_blob_gas),
                })
            }
            _ => {
                // This fallback covers incompatible combinations of fields.
                Err(CallFeesError::ConflictingFeeFieldsInRequest)
            }
        }
    }
}

/// Error coming from decoding and validating transaction request fees.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum CallFeesError {
    /// Thrown when a call or transaction request (`eth_call`, `eth_estimateGas`,
    /// `eth_sendTransaction`) contains conflicting fields (legacy, EIP-1559).
    #[error("both gasPrice and (maxFeePerGas or maxPriorityFeePerGas) specified")]
    ConflictingFeeFieldsInRequest,
    /// Thrown post London if the transaction's fee is less than the base fee of the block.
    #[error("max fee per gas less than block base fee")]
    FeeCapTooLow,
    /// Thrown to ensure no one is able to specify a transaction with a tip higher than the total
    /// fee cap.
    #[error("max priority fee per gas higher than max fee per gas")]
    TipAboveFeeCap,
    /// A sanity error to avoid huge numbers specified in the tip field.
    #[error("max priority fee per gas higher than 2^256-1")]
    TipVeryHigh,
    /// Blob transaction has no versioned hashes.
    #[error("blob transaction missing blob hashes")]
    BlobTransactionMissingBlobHashes,
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_consensus::constants::GWEI_TO_WEI;

    #[test]
    fn test_ensure_0_fallback() {
        let CallFees { gas_price, .. } =
            CallFees::ensure_fees(None, None, None, U256::from(99), None, None, Some(U256::ZERO))
                .unwrap();
        assert!(gas_price.is_zero());
    }

    #[test]
    fn test_ensure_max_fee_0_exception() {
        let CallFees { gas_price, .. } =
            CallFees::ensure_fees(None, Some(U256::ZERO), None, U256::from(99), None, None, None)
                .unwrap();
        assert!(gas_price.is_zero());
    }

    #[test]
    fn test_blob_fees() {
        let CallFees { gas_price, max_fee_per_blob_gas, .. } =
            CallFees::ensure_fees(None, None, None, U256::from(99), None, None, Some(U256::ZERO))
                .unwrap();
        assert!(gas_price.is_zero());
        assert_eq!(max_fee_per_blob_gas, None);

        let CallFees { gas_price, max_fee_per_blob_gas, .. } = CallFees::ensure_fees(
            None,
            None,
            None,
            U256::from(99),
            Some(&[B256::from(U256::ZERO)]),
            None,
            Some(U256::from(99)),
        )
        .unwrap();
        assert!(gas_price.is_zero());
        assert_eq!(max_fee_per_blob_gas, Some(U256::from(99)));
    }

    #[test]
    fn test_eip_1559_fees() {
        let CallFees { gas_price, .. } = CallFees::ensure_fees(
            None,
            Some(U256::from(25 * GWEI_TO_WEI)),
            Some(U256::from(15 * GWEI_TO_WEI)),
            U256::from(15 * GWEI_TO_WEI),
            None,
            None,
            Some(U256::ZERO),
        )
        .unwrap();
        assert_eq!(gas_price, U256::from(25 * GWEI_TO_WEI));

        let CallFees { gas_price, .. } = CallFees::ensure_fees(
            None,
            Some(U256::from(25 * GWEI_TO_WEI)),
            Some(U256::from(5 * GWEI_TO_WEI)),
            U256::from(15 * GWEI_TO_WEI),
            None,
            None,
            Some(U256::ZERO),
        )
        .unwrap();
        assert_eq!(gas_price, U256::from(20 * GWEI_TO_WEI));

        let CallFees { gas_price, .. } = CallFees::ensure_fees(
            None,
            Some(U256::from(30 * GWEI_TO_WEI)),
            Some(U256::from(30 * GWEI_TO_WEI)),
            U256::from(15 * GWEI_TO_WEI),
            None,
            None,
            Some(U256::ZERO),
        )
        .unwrap();
        assert_eq!(gas_price, U256::from(30 * GWEI_TO_WEI));

        let call_fees = CallFees::ensure_fees(
            None,
            Some(U256::from(30 * GWEI_TO_WEI)),
            Some(U256::from(31 * GWEI_TO_WEI)),
            U256::from(15 * GWEI_TO_WEI),
            None,
            None,
            Some(U256::ZERO),
        );
        assert!(call_fees.is_err());

        let call_fees = CallFees::ensure_fees(
            None,
            Some(U256::from(5 * GWEI_TO_WEI)),
            Some(U256::from(GWEI_TO_WEI)),
            U256::from(15 * GWEI_TO_WEI),
            None,
            None,
            Some(U256::ZERO),
        );
        assert!(call_fees.is_err());

        let call_fees = CallFees::ensure_fees(
            None,
            Some(U256::MAX),
            Some(U256::MAX),
            U256::from(5 * GWEI_TO_WEI),
            None,
            None,
            Some(U256::ZERO),
        );
        assert!(call_fees.is_err());
    }

    #[test]
    fn test_fee_errors() {
        let base = U256::from(15 * GWEI_TO_WEI);
        let fees = |call_gas_price, max_fee, priority_fee, hashes: Option<&[B256]>, blob_fee| {
            CallFees::ensure_fees(
                call_gas_price,
                max_fee,
                priority_fee,
                base,
                hashes,
                blob_fee,
                Some(U256::ZERO),
            )
        };

        // Legacy and EIP-1559 fee fields conflict.
        let one = Some(U256::ONE);
        assert_eq!(
            fees(one, one, None, None, None),
            Err(CallFeesError::ConflictingFeeFieldsInRequest)
        );
        assert_eq!(
            fees(one, None, one, None, None),
            Err(CallFeesError::ConflictingFeeFieldsInRequest)
        );
        assert_eq!(
            fees(one, None, None, None, one),
            Err(CallFeesError::ConflictingFeeFieldsInRequest)
        );

        // The fee cap must cover the base fee, and the tip must fit under the fee cap.
        let gwei = |n| Some(U256::from(n * GWEI_TO_WEI));
        assert_eq!(fees(None, gwei(5), gwei(1), None, None), Err(CallFeesError::FeeCapTooLow));
        assert_eq!(fees(None, gwei(30), gwei(31), None, None), Err(CallFeesError::TipAboveFeeCap));
        // A tip that overflows the base fee sum is rejected.
        let max = Some(U256::MAX);
        assert_eq!(fees(None, None, max, None, None), Err(CallFeesError::TipVeryHigh));
        assert_eq!(fees(None, max, max, None, None), Err(CallFeesError::TipVeryHigh));

        // A blob fee requires blob hashes.
        assert_eq!(
            fees(None, None, None, None, one),
            Err(CallFeesError::BlobTransactionMissingBlobHashes)
        );
        assert_eq!(
            fees(None, None, None, Some(&[]), one),
            Err(CallFeesError::BlobTransactionMissingBlobHashes)
        );
    }

    #[test]
    fn test_effective_gas_price() {
        let base = U256::from(15 * GWEI_TO_WEI);
        let gwei = |n| Some(U256::from(n * GWEI_TO_WEI));

        // Without a fee cap, the price is the base fee plus the tip.
        let fees = CallFees::ensure_fees(None, None, gwei(2), base, None, None, None).unwrap();
        assert_eq!(fees.gas_price, U256::from(17 * GWEI_TO_WEI));
        assert_eq!(fees.max_priority_fee_per_gas, gwei(2));

        // A legacy gas price is used as is, even below the base fee.
        let fees = CallFees::ensure_fees(gwei(1), None, None, base, None, None, None).unwrap();
        assert_eq!(fees.gas_price, U256::from(GWEI_TO_WEI));
        assert_eq!(fees.max_priority_fee_per_gas, None);

        // An explicit blob fee is kept, with blob hashes present.
        let hashes = [B256::ZERO];
        let fees =
            CallFees::ensure_fees(None, None, None, base, Some(&hashes), gwei(4), gwei(1)).unwrap();
        assert_eq!(fees.max_fee_per_blob_gas, gwei(4));
    }
}
