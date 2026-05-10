use alloy_primitives::{Address, B256};
use evm2::{
    StorageKey,
    bytecode::Bytecode,
    evm::{AccountInfo, InMemoryDB, StateChanges},
};

/// Parses fixture bytecode into evm2 bytecode.
pub(crate) fn parse_bytecode(code: alloy_primitives::Bytes) -> Bytecode {
    Bytecode::new_raw_checked(code.clone()).unwrap_or_else(|_| Bytecode::new_legacy(code))
}

/// Inserts an account and its storage into an in-memory database.
pub(crate) fn insert_account_with_storage(
    database: &mut InMemoryDB,
    address: Address,
    info: AccountInfo,
    storage: impl IntoIterator<Item = (alloy_primitives::U256, alloy_primitives::U256)>,
) {
    database.insert_account_info(address, info);
    for (key, value) in storage {
        database.insert_account_storage(address, key, value);
    }
}

/// Applies state changes to a cloned database and returns the post-state.
pub(crate) fn apply_state_changes(pre: &InMemoryDB, changes: &StateChanges) -> InMemoryDB {
    let mut post = pre.clone();
    apply_state_changes_in_place(&mut post, changes);
    post
}

/// Applies state changes to an in-memory database.
pub(crate) fn apply_state_changes_in_place(database: &mut InMemoryDB, changes: &StateChanges) {
    for (&code_hash, code) in &changes.code {
        database.cache.contracts.insert(code_hash, code.clone());
    }
    for (&address, storage) in &changes.storage {
        if storage.wipe {
            database.cache.storage.retain(|key, _| key.address() != address);
        }
        for (&key, change) in &storage.slots {
            if change.current.is_zero() {
                database.cache.storage.remove(&StorageKey::new(address, key));
            } else {
                database.cache.storage.insert(StorageKey::new(address, key), change.current);
            }
        }
    }
    for (&address, change) in &changes.accounts {
        match &change.current {
            Some(info) => database.insert_account_info(address, info.clone()),
            None => {
                database.cache.accounts.remove(&address);
                database.cache.storage.retain(|key, _| key.address() != address);
            }
        }
    }
}

/// Returns whether the given system contract exists with non-empty code.
pub(crate) fn system_contract_has_code(database: &InMemoryDB, address: Address) -> bool {
    database
        .cache
        .accounts
        .get(&address)
        .and_then(|info| database.cache.contracts.get(&info.code_hash))
        .is_some_and(|code| !code.is_empty())
}

/// Returns persistent storage values for trie-root calculation.
pub(crate) fn storage_for_root(
    state: &InMemoryDB,
    address: Address,
) -> Vec<(B256, alloy_primitives::U256)> {
    state
        .cache
        .storage
        .iter()
        .filter_map(|(&key, &value)| {
            (key.address() == address && !value.is_zero()).then_some((B256::from(key.key()), value))
        })
        .collect()
}
