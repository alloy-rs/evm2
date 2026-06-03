use alloy_primitives::{Address, B256};
use evm2::{
    bytecode::Bytecode,
    evm::{AccountInfo, DatabaseCommit, InMemoryDB, StateChanges},
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
    database.insert_account_info(&address, info);
    for (key, value) in storage {
        database.insert_account_storage(&address, &key, &value);
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
    database.commit(changes);
}

/// Returns whether the given system contract exists with non-empty code.
pub(crate) fn system_contract_has_code(database: &InMemoryDB, address: Address) -> bool {
    database
        .cache
        .accounts
        .get(&address)
        .and_then(Option::as_ref)
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
