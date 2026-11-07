//! Block access lists (EIP-7928): validate one, and prefill a fork's cache from its post-state.
//!
//! A block access list records every account and slot a block touched with its value after each
//! transaction. Its post-state is the state at the end of the block, so a fork pinned to that
//! block can be warmed from one request instead of an RPC round trip per cold read.

use crate::BlockchainDb;
use alloy_consensus::BlockHeader;
use alloy_eips::{
    BlockId,
    eip7928::{
        BlockAccessList, BlockAccessListValidationError, compute_block_access_list_hash,
        validate_block_access_list,
    },
};
use alloy_primitives::{Address, B256};
use alloy_provider::{
    Network, Provider,
    network::{BlockResponse, primitives::HeaderResponse},
};
use evm2::bytecode::{Bytecode, BytecodeDecodeError};
use std::time::Duration;

/// Time allowed for fetching and checking a block access list in [`fetch_bal`].
const FETCH_BAL_TIMEOUT: Duration = Duration::from_millis(500);

/// Why a block access list was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum BalError {
    /// Its structure is invalid.
    #[error("invalid BAL structure: {0}")]
    Invalid(#[from] BlockAccessListValidationError),
    /// Its hash does not match the block header's.
    #[error("BAL hash mismatch: header has {expected}, BAL hashes to {computed}")]
    HashMismatch {
        /// The hash in the block header.
        expected: B256,
        /// The hash of the block access list.
        computed: B256,
    },
    /// It sets invalid code for an account.
    #[error("invalid BAL code for {0}: {1}")]
    InvalidCode(Address, BytecodeDecodeError),
}

/// Checks a block access list against its block's transaction count and header hash.
pub fn validate_bal(
    bal: &BlockAccessList,
    transaction_count: usize,
    expected_hash: Option<B256>,
) -> Result<(), BalError> {
    validate_block_access_list(bal, transaction_count)?;
    if let Some(expected) = expected_hash {
        let computed = compute_block_access_list_hash(bal);
        if computed != expected {
            return Err(BalError::HashMismatch { expected, computed });
        }
    }
    for account in bal {
        // Reject invalid code even in earlier changes or incomplete accounts.
        for change in &account.code_changes {
            Bytecode::new_raw_checked(change.new_code.clone())
                .map_err(|err| BalError::InvalidCode(account.address, err))?;
        }
    }
    Ok(())
}

/// Prefills `db` with the post-state of a validated block access list, keeping entries already
/// cached. Returns the number of accounts and slots inserted.
///
/// An account is only inserted when the list holds its balance, nonce and code post-state, since
/// a partial account cannot be completed without another request.
pub fn cache_bal(db: &BlockchainDb, bal: &BlockAccessList) -> (usize, usize) {
    let mut inserted_accounts = 0;
    {
        let mut accounts = db.write_accounts();
        let mut contracts = db.write_contracts();
        for account in bal {
            if let (Some(balance), Some(nonce), Some(code)) = (
                account.balance_post_state(),
                account.nonce_post_state(),
                account.code_changes.last(),
            ) && !accounts.contains_key(&account.address)
            {
                let info =
                    (!balance.is_zero() || nonce != 0 || !code.new_code.is_empty()).then(|| {
                        evm2::evm::AccountInfo::default()
                            .with_balance(balance)
                            .with_nonce(nonce)
                            .with_code(Bytecode::from_stored(code.new_code.clone()))
                    });
                if let Some(info) = &info
                    && let Some(code) = &info.code
                {
                    contracts.insert(info.code_hash, code.clone());
                }
                accounts.insert(account.address, info);
                inserted_accounts += 1;
            }
        }
    }
    let mut inserted_slots = 0;
    let mut storage = db.write_storage();
    for account in bal.iter().filter(|account| !account.storage_changes.is_empty()) {
        let slots = storage.entry(account.address).or_default();
        for (slot, value) in account.storage_post_states() {
            if let alloy_primitives::map::Entry::Vacant(entry) = slots.entry(slot) {
                entry.insert(value);
                inserted_slots += 1;
            }
        }
    }
    tracing::debug!(target: "evm2::fork", inserted_accounts, inserted_slots, "prefilled fork cache from BAL");
    (inserted_accounts, inserted_slots)
}

/// Fetches and validates the block access list of `block`, giving up after a short timeout.
///
/// Returns `None` when the endpoint has no list for the block, the list is invalid, or the
/// endpoint is a local dev node (it answers `anvil_nodeInfo`), whose state can change without
/// its block hash changing.
pub async fn fetch_bal<N: Network, P: Provider<N>>(
    provider: &P,
    block: &N::BlockResponse,
) -> Option<BlockAccessList> {
    let header = block.header();
    let fetch = async {
        if !immutable_source(provider).await {
            return None;
        }
        let bal = provider.get_block_access_list(BlockId::hash(header.hash())).await.ok()??;
        let transactions = block.transactions().len();
        if let Err(err) = validate_bal(&bal, transactions, header.block_access_list_hash()) {
            tracing::debug!(target: "evm2::fork", block_hash = %header.hash(), %err, "ignoring invalid BAL");
            return None;
        }
        Some(bal)
    };
    tokio::time::timeout(FETCH_BAL_TIMEOUT, fetch).await.ok().flatten()
}

/// Whether the endpoint is not a local dev node, judged by `anvil_nodeInfo` being unknown.
async fn immutable_source<N: Network, P: Provider<N>>(provider: &P) -> bool {
    match provider.raw_request::<_, serde_json::Value>("anvil_nodeInfo".into(), ()).await {
        Ok(_) => false,
        Err(err) => err.as_error_resp().is_some_and(|resp| resp.code == -32601),
    }
}
