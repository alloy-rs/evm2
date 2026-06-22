use alloy_primitives::{Address, B256};
use alloy_trie::{
    TrieAccount,
    root::{state_root_unhashed, storage_root_unhashed},
};
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
        .get(&address)
        .into_iter()
        .flat_map(|storage| storage.slots.iter())
        .filter_map(|(&key, &value)| (!value.is_zero()).then_some((B256::from(key), value)))
        .collect()
}

/// Computes the state root of all accounts currently cached in the database.
pub(crate) fn state_root_from_database(state: &InMemoryDB) -> B256 {
    let accounts = state.cache.accounts.iter().filter_map(|(&address, info)| {
        let info = info.as_ref()?;
        let storage = storage_for_root(state, address);
        Some((
            address,
            TrieAccount {
                nonce: info.nonce,
                balance: info.balance,
                storage_root: storage_root_unhashed(storage),
                code_hash: info.code_hash,
            },
        ))
    });

    state_root_unhashed(accounts)
}
