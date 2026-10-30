use alloy_primitives::{Address, B256};
use evm2::{
    bytecode::Bytecode,
    evm::{AccountInfo, InMemoryDB},
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

/// Returns persistent storage values for trie-root calculation.
pub(crate) fn storage_for_root(
    state: &InMemoryDB,
    address: Address,
) -> Vec<(B256, alloy_primitives::U256)> {
    state
        .cache
        .storage
        .get(&address)
        .into_iter()
        .flat_map(|storage| storage.slots.iter())
        .filter_map(|(&key, &value)| (!value.is_zero()).then_some((B256::from(key), value)))
        .collect()
}
