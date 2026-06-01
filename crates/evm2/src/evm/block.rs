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
use alloy_primitives::{Address, B256, Bytes, U256, address, map::AddressMap};
use derive_where::derive_where;

const ONE_ETHER: u128 = 1_000_000_000_000_000_000;

/// Mainnet DAO fork block.
pub const MAINNET_DAO_HARDFORK_BLOCK: u64 = 1_920_000;

/// DAO hardfork beneficiary that received ether from DAO accounts and child accounts.
pub const DAO_HARDFORK_BENEFICIARY: Address =
    address!("0xbf4ed7b27f1d666546e30d74d50d173d20bca754");

/// DAO hardfork accounts whose balances were moved to [`DAO_HARDFORK_BENEFICIARY`].
pub const DAO_HARDFORK_ACCOUNTS: [Address; 116] = [
    address!("0xd4fe7bc31cedb7bfb8a345f31e668033056b2728"),
    address!("0xb3fb0e5aba0e20e5c49d252dfd30e102b171a425"),
    address!("0x2c19c7f9ae8b751e37aeb2d93a699722395ae18f"),
    address!("0xecd135fa4f61a655311e86238c92adcd779555d2"),
    address!("0x1975bd06d486162d5dc297798dfc41edd5d160a7"),
    address!("0xa3acf3a1e16b1d7c315e23510fdd7847b48234f6"),
    address!("0x319f70bab6845585f412ec7724b744fec6095c85"),
    address!("0x06706dd3f2c9abf0a21ddcc6941d9b86f0596936"),
    address!("0x5c8536898fbb74fc7445814902fd08422eac56d0"),
    address!("0x6966ab0d485353095148a2155858910e0965b6f9"),
    address!("0x779543a0491a837ca36ce8c635d6154e3c4911a6"),
    address!("0x2a5ed960395e2a49b1c758cef4aa15213cfd874c"),
    address!("0x5c6e67ccd5849c0d29219c4f95f1a7a93b3f5dc5"),
    address!("0x9c50426be05db97f5d64fc54bf89eff947f0a321"),
    address!("0x200450f06520bdd6c527622a273333384d870efb"),
    address!("0xbe8539bfe837b67d1282b2b1d61c3f723966f049"),
    address!("0x6b0c4d41ba9ab8d8cfb5d379c69a612f2ced8ecb"),
    address!("0xf1385fb24aad0cd7432824085e42aff90886fef5"),
    address!("0xd1ac8b1ef1b69ff51d1d401a476e7e612414f091"),
    address!("0x8163e7fb499e90f8544ea62bbf80d21cd26d9efd"),
    address!("0x51e0ddd9998364a2eb38588679f0d2c42653e4a6"),
    address!("0x627a0a960c079c21c34f7612d5d230e01b4ad4c7"),
    address!("0xf0b1aa0eb660754448a7937c022e30aa692fe0c5"),
    address!("0x24c4d950dfd4dd1902bbed3508144a54542bba94"),
    address!("0x9f27daea7aca0aa0446220b98d028715e3bc803d"),
    address!("0xa5dc5acd6a7968a4554d89d65e59b7fd3bff0f90"),
    address!("0xd9aef3a1e38a39c16b31d1ace71bca8ef58d315b"),
    address!("0x63ed5a272de2f6d968408b4acb9024f4cc208ebf"),
    address!("0x6f6704e5a10332af6672e50b3d9754dc460dfa4d"),
    address!("0x77ca7b50b6cd7e2f3fa008e24ab793fd56cb15f6"),
    address!("0x492ea3bb0f3315521c31f273e565b868fc090f17"),
    address!("0x0ff30d6de14a8224aa97b78aea5388d1c51c1f00"),
    address!("0x9ea779f907f0b315b364b0cfc39a0fde5b02a416"),
    address!("0xceaeb481747ca6c540a000c1f3641f8cef161fa7"),
    address!("0xcc34673c6c40e791051898567a1222daf90be287"),
    address!("0x579a80d909f346fbfb1189493f521d7f48d52238"),
    address!("0xe308bd1ac5fda103967359b2712dd89deffb7973"),
    address!("0x4cb31628079fb14e4bc3cd5e30c2f7489b00960c"),
    address!("0xac1ecab32727358dba8962a0f3b261731aad9723"),
    address!("0x4fd6ace747f06ece9c49699c7cabc62d02211f75"),
    address!("0x440c59b325d2997a134c2c7c60a8c61611212bad"),
    address!("0x4486a3d68fac6967006d7a517b889fd3f98c102b"),
    address!("0x9c15b54878ba618f494b38f0ae7443db6af648ba"),
    address!("0x27b137a85656544b1ccb5a0f2e561a5703c6a68f"),
    address!("0x21c7fdb9ed8d291d79ffd82eb2c4356ec0d81241"),
    address!("0x23b75c2f6791eef49c69684db4c6c1f93bf49a50"),
    address!("0x1ca6abd14d30affe533b24d7a21bff4c2d5e1f3b"),
    address!("0xb9637156d330c0d605a791f1c31ba5890582fe1c"),
    address!("0x6131c42fa982e56929107413a9d526fd99405560"),
    address!("0x1591fc0f688c81fbeb17f5426a162a7024d430c2"),
    address!("0x542a9515200d14b68e934e9830d91645a980dd7a"),
    address!("0xc4bbd073882dd2add2424cf47d35213405b01324"),
    address!("0x782495b7b3355efb2833d56ecb34dc22ad7dfcc4"),
    address!("0x58b95c9a9d5d26825e70a82b6adb139d3fd829eb"),
    address!("0x3ba4d81db016dc2890c81f3acec2454bff5aada5"),
    address!("0xb52042c8ca3f8aa246fa79c3feaa3d959347c0ab"),
    address!("0xe4ae1efdfc53b73893af49113d8694a057b9c0d1"),
    address!("0x3c02a7bc0391e86d91b7d144e61c2c01a25a79c5"),
    address!("0x0737a6b837f97f46ebade41b9bc3e1c509c85c53"),
    address!("0x97f43a37f595ab5dd318fb46e7a155eae057317a"),
    address!("0x52c5317c848ba20c7504cb2c8052abd1fde29d03"),
    address!("0x4863226780fe7c0356454236d3b1c8792785748d"),
    address!("0x5d2b2e6fcbe3b11d26b525e085ff818dae332479"),
    address!("0x5f9f3392e9f62f63b8eac0beb55541fc8627f42c"),
    address!("0x057b56736d32b86616a10f619859c6cd6f59092a"),
    address!("0x9aa008f65de0b923a2a4f02012ad034a5e2e2192"),
    address!("0x304a554a310c7e546dfe434669c62820b7d83490"),
    address!("0x914d1b8b43e92723e64fd0a06f5bdb8dd9b10c79"),
    address!("0x4deb0033bb26bc534b197e61d19e0733e5679784"),
    address!("0x07f5c1e1bc2c93e0402f23341973a0e043f7bf8a"),
    address!("0x35a051a0010aba705c9008d7a7eff6fb88f6ea7b"),
    address!("0x4fa802324e929786dbda3b8820dc7834e9134a2a"),
    address!("0x9da397b9e80755301a3b32173283a91c0ef6c87e"),
    address!("0x8d9edb3054ce5c5774a420ac37ebae0ac02343c6"),
    address!("0x0101f3be8ebb4bbd39a2e3b9a3639d4259832fd9"),
    address!("0x5dc28b15dffed94048d73806ce4b7a4612a1d48f"),
    address!("0xbcf899e6c7d9d5a215ab1e3444c86806fa854c76"),
    address!("0x12e626b0eebfe86a56d633b9864e389b45dcb260"),
    address!("0xa2f1ccba9395d7fcb155bba8bc92db9bafaeade7"),
    address!("0xec8e57756626fdc07c63ad2eafbd28d08e7b0ca5"),
    address!("0xd164b088bd9108b60d0ca3751da4bceb207b0782"),
    address!("0x6231b6d0d5e77fe001c2a460bd9584fee60d409b"),
    address!("0x1cba23d343a983e9b5cfd19496b9a9701ada385f"),
    address!("0xa82f360a8d3455c5c41366975bde739c37bfeb8a"),
    address!("0x9fcd2deaff372a39cc679d5c5e4de7bafb0b1339"),
    address!("0x005f5cee7a43331d5a3d3eec71305925a62f34b6"),
    address!("0x0e0da70933f4c7849fc0d203f5d1d43b9ae4532d"),
    address!("0xd131637d5275fd1a68a3200f4ad25c71a2a9522e"),
    address!("0xbc07118b9ac290e4622f5e77a0853539789effbe"),
    address!("0x47e7aa56d6bdf3f36be34619660de61275420af8"),
    address!("0xacd87e28b0c9d1254e868b81cba4cc20d9a32225"),
    address!("0xadf80daec7ba8dcf15392f1ac611fff65d94f880"),
    address!("0x5524c55fb03cf21f549444ccbecb664d0acad706"),
    address!("0x40b803a9abce16f50f36a77ba41180eb90023925"),
    address!("0xfe24cdd8648121a43a7c86d289be4dd2951ed49f"),
    address!("0x17802f43a0137c506ba92291391a8a8f207f487d"),
    address!("0x253488078a4edf4d6f42f113d1e62836a942cf1a"),
    address!("0x86af3e9626fce1957c82e88cbf04ddf3a2ed7915"),
    address!("0xb136707642a4ea12fb4bae820f03d2562ebff487"),
    address!("0xdbe9b615a3ae8709af8b93336ce9b477e4ac0940"),
    address!("0xf14c14075d6c4ed84b86798af0956deef67365b5"),
    address!("0xca544e5c4687d109611d0f8f928b53a25af72448"),
    address!("0xaeeb8ff27288bdabc0fa5ebb731b6f409507516c"),
    address!("0xcbb9d3703e651b0d496cdefb8b92c25aeb2171f7"),
    address!("0x6d87578288b6cb5549d5076a207456a1f6a63dc0"),
    address!("0xb2c6f0dfbb716ac562e2d85d6cb2f8d5ee87603e"),
    address!("0xaccc230e8a6e5be9160b8cdf2864dd2a001c28b6"),
    address!("0x2b3455ec7fedf16e646268bf88846bd7a2319bb2"),
    address!("0x4613f3bca5c44ea06337a9e439fbc6d42e501d0a"),
    address!("0xd343b217de44030afaa275f54d31a9317c7f441e"),
    address!("0x84ef4b2357079cd7a7c69fd7a37cd0609a679106"),
    address!("0xda2fef9e4a3230988ff17df2165440f37e8b1708"),
    address!("0xf4c64518ea10f995918a454158c6b61407ea345c"),
    address!("0x7602b46df5390e432ef1c307d4f2c9ff6d65cc97"),
    address!("0xbb9bc244d798123fde783fcc1c72d3bb8c189413"),
    address!("0x807640a13483f8ac783c557fcdf27be11ea4ac7a"),
];

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

    /// Drains account balances into `beneficiary` and returns the produced state changes.
    pub fn apply_balance_drain(
        &mut self,
        accounts: &[Address],
        beneficiary: Address,
    ) -> Result<StateChanges, BlockExecutionError> {
        let checkpoint = self.state.checkpoint();
        let mut drained = U256::ZERO;

        for address in accounts {
            let balance = match self.state.account_info(address) {
                Ok(info) => info.map_or(U256::ZERO, |info| info.balance),
                Err(code) => {
                    self.state.rollback(checkpoint, self.spec_id());
                    self.state.clear_transaction_state();
                    return Err(BlockExecutionError::Database(code));
                }
            };
            if balance.is_zero() {
                continue;
            }

            if let Err(code) = self.state.add_balance(address, &U256::ZERO.wrapping_sub(balance)) {
                self.state.rollback(checkpoint, self.spec_id());
                self.state.clear_transaction_state();
                return Err(BlockExecutionError::Database(code));
            }
            drained = drained.saturating_add(balance);
        }

        if !drained.is_zero()
            && let Err(code) = self.state.add_balance(&beneficiary, &drained)
        {
            self.state.rollback(checkpoint, self.spec_id());
            self.state.clear_transaction_state();
            return Err(BlockExecutionError::Database(code));
        }

        let changes = self.state.build_state_changes();
        self.state.commit_transaction_overlay();
        self.state.clear_transaction_state();
        Ok(changes)
    }

    /// Applies the mainnet DAO hardfork balance move if the active block is the fork block.
    pub fn apply_mainnet_dao_hardfork_balance_move(
        &mut self,
    ) -> Result<Option<StateChanges>, BlockExecutionError> {
        if self.block.number != U256::from(MAINNET_DAO_HARDFORK_BLOCK) {
            return Ok(None);
        }
        self.apply_balance_drain(&DAO_HARDFORK_ACCOUNTS, DAO_HARDFORK_BENEFICIARY).map(Some)
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
        evm::{AccountInfo, EmptyDB, InMemoryDB, precompile::NoPrecompiles},
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

    #[test]
    fn apply_balance_drain_moves_balances_to_beneficiary() {
        let drained_1 = Address::with_last_byte(0x04);
        let drained_2 = Address::with_last_byte(0x05);
        let beneficiary = Address::with_last_byte(0x06);
        let mut db = InMemoryDB::default();
        db.insert_account_info(&drained_1, AccountInfo::default().with_balance(U256::from(7)));
        db.insert_account_info(&drained_2, AccountInfo::default().with_balance(U256::from(11)));
        db.insert_account_info(&beneficiary, AccountInfo::default().with_balance(U256::from(13)));
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::HOMESTEAD,
            BlockEnv::default(),
            TxRegistry::new(),
            db,
            NoPrecompiles::default(),
        );

        let changes = evm.apply_balance_drain(&[drained_1, drained_2], beneficiary).unwrap();

        assert_eq!(changes.accounts[&drained_1].current.as_ref().unwrap().balance, U256::ZERO);
        assert_eq!(changes.accounts[&drained_2].current.as_ref().unwrap().balance, U256::ZERO);
        assert_eq!(
            changes.accounts[&beneficiary].current.as_ref().unwrap().balance,
            U256::from(31)
        );
    }

    #[test]
    fn mainnet_dao_hardfork_balance_move_only_runs_at_fork_block() {
        let drained = DAO_HARDFORK_ACCOUNTS[0];
        let mut db = InMemoryDB::default();
        db.insert_account_info(&drained, AccountInfo::default().with_balance(U256::from(5)));
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::HOMESTEAD,
            BlockEnv { number: U256::from(MAINNET_DAO_HARDFORK_BLOCK - 1), ..BlockEnv::default() },
            TxRegistry::new(),
            db,
            NoPrecompiles::default(),
        );

        assert!(evm.apply_mainnet_dao_hardfork_balance_move().unwrap().is_none());
        evm.block_mut().number = U256::from(MAINNET_DAO_HARDFORK_BLOCK);
        let changes = evm.apply_mainnet_dao_hardfork_balance_move().unwrap().unwrap();

        assert_eq!(changes.accounts[&drained].current.as_ref().unwrap().balance, U256::ZERO);
        assert_eq!(
            changes.accounts[&DAO_HARDFORK_BENEFICIARY].current.as_ref().unwrap().balance,
            U256::from(5)
        );
    }
}
