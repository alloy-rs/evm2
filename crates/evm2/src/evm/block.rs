//! Block-level EVM helpers.

use super::{
    BEACON_ROOTS_ADDRESS, CONSOLIDATION_REQUEST_ADDRESS, Evm, HISTORY_STORAGE_ADDRESS,
    StateChanges, WITHDRAWAL_REQUEST_ADDRESS,
};
use crate::{
    BaseEvmTypes, EvmTypes, SpecId, TxResult, env::BlockEnv, ethereum::RecoveredTxEnvelope,
    interpreter::InstrStop, registry::HandlerError,
};
use alloc::vec::Vec;
use alloy_eips::{
    eip4895::Withdrawal, eip7002::WITHDRAWAL_REQUEST_TYPE, eip7251::CONSOLIDATION_REQUEST_TYPE,
    eip7685::Requests,
};
use alloy_primitives::{Address, B256, Bytes, U256, map::AddressMap};
use derive_where::derive_where;

const ONE_ETHER: u128 = 1_000_000_000_000_000_000;

/// The result of executing block transactions.
#[derive_where(Debug, Clone, PartialEq, Eq; T::TxResultExt)]
pub struct BlockExecutionResult<T: EvmTypes = BaseEvmTypes> {
    /// Transaction execution results in block order.
    pub transaction_results: Vec<TxResult<T>>,
    /// Total gas used by transactions in the block.
    pub gas_used: u64,
    /// Cumulative transaction gas used after refunds.
    pub cumulative_tx_gas_used: u64,
    /// Regular gas used by transactions in this block.
    pub block_regular_gas_used: u64,
    /// State gas used by transactions in this block.
    pub block_state_gas_used: u64,
    /// Blob gas used by transactions in the block.
    pub blob_gas_used: u64,
}

impl<T: EvmTypes> Default for BlockExecutionResult<T> {
    fn default() -> Self {
        Self {
            transaction_results: Vec::new(),
            gas_used: 0,
            cumulative_tx_gas_used: 0,
            block_regular_gas_used: 0,
            block_state_gas_used: 0,
            blob_gas_used: 0,
        }
    }
}

/// State changes and requests produced by block system calls.
#[derive_where(Debug, Clone, PartialEq, Eq; T::TxResultExt)]
pub struct BlockSystemCallResult<T: EvmTypes = BaseEvmTypes> {
    /// System transaction results in execution order.
    pub system_results: Vec<TxResult<T>>,
    /// EIP-7685 requests emitted by post-block system calls.
    pub requests: Requests,
}

/// Ommer header data needed for post-block balance increments.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct BlockOmmer {
    /// Ommer beneficiary.
    pub beneficiary: Address,
    /// Ommer block number.
    pub number: u64,
}

impl<T: EvmTypes> Default for BlockSystemCallResult<T> {
    fn default() -> Self {
        Self { system_results: Vec::new(), requests: Requests::default() }
    }
}

/// Block execution error.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum BlockExecutionError {
    /// Transaction execution failed.
    #[error(transparent)]
    Transaction(#[from] HandlerError),
    /// Database operation failed.
    #[error("database error {0:?}")]
    Database(super::DbErrorCode),
    /// Transaction gas limit exceeds the gas still available in the block.
    #[error(
        "transaction gas limit {transaction_gas_limit} is more than block available gas {block_available_gas}"
    )]
    TransactionGasLimitMoreThanAvailableBlockGas {
        /// The transaction gas limit.
        transaction_gas_limit: u64,
        /// The gas still available in the block.
        block_available_gas: u64,
    },
    /// Cancun block is missing its parent beacon block root.
    #[error("EIP-4788 parent beacon block root missing for active Cancun block")]
    MissingParentBeaconBlockRoot,
    /// Cancun genesis block has a non-zero parent beacon block root.
    #[error(
        "the parent beacon block root is not zero for Cancun genesis block: {parent_beacon_block_root}"
    )]
    CancunGenesisParentBeaconBlockRootNotZero {
        /// The supplied parent beacon block root.
        parent_beacon_block_root: B256,
    },
    /// System call execution failed.
    #[error("failed to apply {label} system call at {address}: {stop:?}")]
    SystemCall {
        /// System call label.
        label: &'static str,
        /// System contract address.
        address: Address,
        /// EVM stop reason.
        stop: InstrStop,
    },
}

impl<T: EvmTypes> Evm<T> {
    /// Returns the active block environment.
    #[inline]
    pub const fn block(&self) -> &BlockEnv<T> {
        &self.block
    }

    /// Returns the active block environment mutably.
    #[inline]
    pub const fn block_mut(&mut self) -> &mut BlockEnv<T> {
        &mut self.block
    }

    /// Replaces the active block environment.
    #[inline]
    pub const fn set_block(&mut self, block: BlockEnv<T>) {
        self.block = block;
    }
}

impl<T: EvmTypes<Host = Self>> Evm<T> {
    /// Applies pre-block system calls for EIP-2935 and EIP-4788.
    pub fn apply_pre_block_system_calls(
        &mut self,
        parent_block_hash: B256,
        parent_beacon_block_root: Option<B256>,
    ) -> Result<BlockSystemCallResult<T>, BlockExecutionError> {
        let mut result = BlockSystemCallResult::default();
        if self.spec_id().enables(SpecId::PRAGUE) && !self.block.number.is_zero() {
            result.system_results.push(self.block_system_call(
                "eip2935",
                HISTORY_STORAGE_ADDRESS,
                Bytes::copy_from_slice(parent_block_hash.as_slice()),
            )?);
        }

        if !self.spec_id().enables(SpecId::CANCUN) {
            return Ok(result);
        }

        let parent_beacon_block_root =
            parent_beacon_block_root.ok_or(BlockExecutionError::MissingParentBeaconBlockRoot)?;
        if self.block.number.is_zero() {
            if !parent_beacon_block_root.is_zero() {
                return Err(BlockExecutionError::CancunGenesisParentBeaconBlockRootNotZero {
                    parent_beacon_block_root,
                });
            }
            return Ok(result);
        }

        result.system_results.push(self.block_system_call(
            "eip4788",
            BEACON_ROOTS_ADDRESS,
            Bytes::copy_from_slice(parent_beacon_block_root.as_slice()),
        )?);
        Ok(result)
    }

    /// Applies post-block system calls for EIP-7002 and EIP-7251.
    pub fn apply_post_block_system_calls(
        &mut self,
    ) -> Result<BlockSystemCallResult<T>, BlockExecutionError> {
        let mut result = BlockSystemCallResult::default();
        if !self.spec_id().enables(SpecId::PRAGUE) {
            return Ok(result);
        }

        let withdrawal =
            self.block_system_call("eip7002", WITHDRAWAL_REQUEST_ADDRESS, Bytes::new())?;
        if !withdrawal.output.is_empty() {
            result
                .requests
                .push_request_with_type(WITHDRAWAL_REQUEST_TYPE, withdrawal.output.clone());
        }
        result.system_results.push(withdrawal);

        let consolidation =
            self.block_system_call("eip7251", CONSOLIDATION_REQUEST_ADDRESS, Bytes::new())?;
        if !consolidation.output.is_empty() {
            result
                .requests
                .push_request_with_type(CONSOLIDATION_REQUEST_TYPE, consolidation.output.clone());
        }
        result.system_results.push(consolidation);

        Ok(result)
    }

    fn block_system_call(
        &mut self,
        label: &'static str,
        address: Address,
        data: Bytes,
    ) -> Result<TxResult<T>, BlockExecutionError> {
        let result = self.system_call(address, data);
        if !result.status {
            return Err(BlockExecutionError::SystemCall { label, address, stop: result.stop });
        }
        Ok(result)
    }

    /// Calculates post-block balance increments for rewards and withdrawals.
    pub fn post_block_balance_increments(
        &self,
        ommers: &[BlockOmmer],
        withdrawals: Option<&[Withdrawal]>,
    ) -> AddressMap<U256> {
        let mut balance_increments = AddressMap::with_capacity_and_hasher(
            withdrawals.map_or(ommers.len(), |withdrawals| withdrawals.len()),
            Default::default(),
        );

        if let Some(base_block_reward) = base_block_reward(self.spec_id()) {
            let block_number = self.block.number.saturating_to::<u64>();
            for ommer in ommers {
                *balance_increments.entry(ommer.beneficiary).or_default() +=
                    U256::from(ommer_reward(base_block_reward, block_number, ommer.number));
            }

            *balance_increments.entry(self.block.beneficiary).or_default() +=
                U256::from(block_reward(base_block_reward, ommers.len()));
        }

        if self.spec_id().enables(SpecId::SHANGHAI)
            && let Some(withdrawals) = withdrawals
        {
            for withdrawal in withdrawals {
                let amount = withdrawal.amount_wei();
                if !amount.is_zero() {
                    *balance_increments.entry(withdrawal.address).or_default() += amount;
                }
            }
        }

        balance_increments
    }

    /// Applies post-block balance increments and returns the produced state changes.
    pub fn apply_post_block_balance_increments(
        &mut self,
        ommers: &[BlockOmmer],
        withdrawals: Option<&[Withdrawal]>,
    ) -> Result<StateChanges, BlockExecutionError> {
        let increments = self.post_block_balance_increments(ommers, withdrawals);
        self.apply_balance_increments(increments)
    }

    /// Applies explicit balance increments and returns the produced state changes.
    pub fn apply_balance_increments(
        &mut self,
        increments: AddressMap<U256>,
    ) -> Result<StateChanges, BlockExecutionError> {
        let checkpoint = self.state.checkpoint();
        for (address, amount) in increments {
            if let Err(code) = self.state.add_balance(&address, &amount) {
                self.state.rollback(checkpoint, self.spec_id());
                self.state.clear_transaction_state();
                return Err(BlockExecutionError::Database(code));
            }
        }

        let changes = self.state.build_state_changes();
        self.state.commit_transaction_overlay();
        self.state.clear_transaction_state();
        Ok(changes)
    }
}

impl Evm<BaseEvmTypes> {
    /// Executes Ethereum transactions in block order and advances the in-memory state overlay.
    pub fn execute_block_transactions<'a>(
        &mut self,
        txs: impl IntoIterator<Item = &'a RecoveredTxEnvelope>,
    ) -> Result<BlockExecutionResult, BlockExecutionError> {
        let mut result = BlockExecutionResult::default();

        for tx in txs {
            let block_gas_used = if self.feature(crate::EvmFeatures::EIP8037) {
                result.block_regular_gas_used
            } else {
                result.cumulative_tx_gas_used
            };
            let block_available_gas =
                self.block.gas_limit.saturating_sub(U256::from(block_gas_used));
            let max_tx_gas_usage = tx.gas_limit().min(self.version().tx_gas_limit_cap);
            if U256::from(max_tx_gas_usage) > block_available_gas {
                return Err(BlockExecutionError::TransactionGasLimitMoreThanAvailableBlockGas {
                    transaction_gas_limit: tx.gas_limit(),
                    block_available_gas: u64::try_from(block_available_gas).unwrap_or(u64::MAX),
                });
            }

            let tx_result = self.transact(tx)?;
            result.cumulative_tx_gas_used =
                result.cumulative_tx_gas_used.saturating_add(tx_result.gas_used);
            result.block_regular_gas_used =
                result.block_regular_gas_used.saturating_add(tx_result.gas_used);
            result.block_state_gas_used =
                result.block_state_gas_used.saturating_add(tx_result.state_gas_used);
            result.gas_used = if self.feature(crate::EvmFeatures::EIP8037) {
                result.block_regular_gas_used.max(result.block_state_gas_used)
            } else {
                result.cumulative_tx_gas_used
            };
            if self.spec_id().enables(SpecId::CANCUN) {
                result.blob_gas_used = result.blob_gas_used.saturating_add(tx.blob_gas_used());
            }
            result.transaction_results.push(tx_result);
        }

        Ok(result)
    }
}

const fn base_block_reward(spec_id: SpecId) -> Option<u128> {
    if spec_id.enables(SpecId::MERGE) {
        None
    } else if spec_id.enables(SpecId::PETERSBURG) {
        Some(ONE_ETHER * 2)
    } else if spec_id.enables(SpecId::BYZANTIUM) {
        Some(ONE_ETHER * 3)
    } else {
        Some(ONE_ETHER * 5)
    }
}

const fn block_reward(base_block_reward: u128, ommers: usize) -> u128 {
    base_block_reward + (base_block_reward >> 5) * ommers as u128
}

const fn ommer_reward(base_block_reward: u128, block_number: u64, ommer_block_number: u64) -> u128 {
    ((8 + ommer_block_number - block_number) as u128 * base_block_reward) >> 3
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        SpecId,
        evm::{EmptyDB, precompile::NoPrecompiles},
        registry::{HandlerError, TxRegistry},
    };
    use alloy_consensus::{TxLegacy, transaction::Recovered};

    #[test]
    fn execute_block_transactions_rejects_tx_over_remaining_block_gas() {
        let block = BlockEnv { gas_limit: U256::from(10), ..BlockEnv::default() };
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            block,
            TxRegistry::new(),
            EmptyDB::default(),
            NoPrecompiles::default(),
        );
        let tx = RecoveredTxEnvelope::Legacy(Recovered::new_unchecked(
            TxLegacy { gas_limit: 11, ..TxLegacy::default() },
            Address::ZERO,
        ));

        let err = evm.execute_block_transactions([&tx]).unwrap_err();

        assert_eq!(
            err,
            BlockExecutionError::TransactionGasLimitMoreThanAvailableBlockGas {
                transaction_gas_limit: 11,
                block_available_gas: 10,
            }
        );
    }

    #[test]
    fn execute_block_transactions_checks_capped_tx_gas_against_remaining_block_gas() {
        let cap = crate::Version::base(SpecId::AMSTERDAM).tx_gas_limit_cap;
        let block = BlockEnv { gas_limit: U256::from(cap), ..BlockEnv::default() };
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::AMSTERDAM,
            block,
            TxRegistry::new(),
            EmptyDB::default(),
            NoPrecompiles::default(),
        );
        let tx = RecoveredTxEnvelope::Legacy(Recovered::new_unchecked(
            TxLegacy { gas_limit: cap + 1, ..TxLegacy::default() },
            Address::ZERO,
        ));

        let err = evm.execute_block_transactions([&tx]).unwrap_err();

        assert_eq!(
            err,
            BlockExecutionError::Transaction(HandlerError::UnsupportedTransactionType(0))
        );
    }

    #[test]
    fn post_block_balance_increments_include_rewards_and_ommers() {
        let beneficiary = Address::with_last_byte(0x01);
        let ommer_beneficiary = Address::with_last_byte(0x02);
        let block = BlockEnv { number: U256::from(10), beneficiary, ..BlockEnv::default() };
        let evm = Evm::<BaseEvmTypes>::new(
            SpecId::HOMESTEAD,
            block,
            TxRegistry::new(),
            EmptyDB::default(),
            NoPrecompiles::default(),
        );

        let increments = evm.post_block_balance_increments(
            &[BlockOmmer { beneficiary: ommer_beneficiary, number: 9 }],
            None,
        );

        assert_eq!(increments[&beneficiary], U256::from(ONE_ETHER * 5 + ((ONE_ETHER * 5) >> 5)));
        assert_eq!(increments[&ommer_beneficiary], U256::from(((8 + 9 - 10) * ONE_ETHER * 5) >> 3));
    }

    #[test]
    fn apply_post_block_balance_increments_applies_withdrawals() {
        let address = Address::with_last_byte(0x03);
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::SHANGHAI,
            BlockEnv::default(),
            TxRegistry::new(),
            EmptyDB::default(),
            NoPrecompiles::default(),
        );
        let withdrawals = [Withdrawal { index: 0, validator_index: 0, address, amount: 2 }];

        let changes = evm.apply_post_block_balance_increments(&[], Some(&withdrawals)).unwrap();

        assert_eq!(
            changes.accounts[&address].current.as_ref().unwrap().balance,
            U256::from(2_000_000_000_u64)
        );
    }
}
