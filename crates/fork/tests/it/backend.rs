use crate::mock::{MockAccount, MockNode};
use alloy_consensus::{TxLegacy, transaction::Recovered};
use alloy_eips::{
    BlockId,
    eip7928::{
        AccountChanges, BalanceChange, BlockAccessIndex, CodeChange, NonceChange, SlotChanges,
        StorageChange as BalStorageChange,
    },
};
use alloy_primitives::{Address, B256, Bytes, TxKind, U256, keccak256, map::HashMap};
use alloy_provider::{Provider, network::TransactionResponse};
use evm2::{
    BaseEvmTypes, Evm, Precompiles, SpecId,
    bytecode::Bytecode,
    env::BlockEnvExt,
    ethereum::{TxEnvelope, ethereum_tx_registry},
    evm::{
        AccountChangeRef, AccountInfo, DatabaseRef, DbRef, DynDatabase, StateChangeSink,
        StorageChange,
    },
    interpreter::op,
};
use evm2_fork::{
    AccountFetchPolicy, BlockchainDb, BlockchainDbMeta, BlockingMode, ForkDB, ForkError,
    PrefetchStats, SharedBackend, bal, fork_env,
};
use serde_json::json;
use std::{thread, time::Duration};

const CONTRACT: Address = Address::repeat_byte(0xc0);
const RICH: Address = Address::repeat_byte(0xaa);
const CALLER: Address = Address::repeat_byte(0xca);
const BLOCK: u64 = 100;

/// Returns `sload(1) + balance(RICH)`, then stores `0x99` in slot 1.
fn contract_code() -> Bytes {
    let mut code = vec![op::PUSH1, 1, op::SLOAD, op::PUSH20];
    code.extend_from_slice(RICH.as_slice());
    code.extend([
        op::BALANCE,
        op::ADD,
        op::PUSH1,
        0,
        op::MSTORE,
        op::PUSH1,
        0x99,
        op::PUSH1,
        1,
        op::SSTORE,
        op::PUSH1,
        32,
        op::PUSH1,
        0,
        op::RETURN,
    ]);
    code.into()
}

fn mock_node(delay: Duration) -> MockNode {
    let node = MockNode::with_delay(delay);
    node.add_blocks(BLOCK + 10);
    node.insert_account(
        CONTRACT,
        MockAccount {
            nonce: 1,
            code: contract_code(),
            storage: HashMap::from_iter([(U256::from(1), U256::from(40))]),
            ..Default::default()
        },
    );
    node.insert_account(RICH, MockAccount { balance: U256::from(2), ..Default::default() });
    node
}

fn spawn(node: &MockNode, db: BlockchainDb) -> SharedBackend {
    SharedBackend::spawn_backend_thread(node.provider(), db, Some(BLOCK.into()))
}

fn meta() -> BlockchainDbMeta {
    BlockchainDbMeta::new(1, BLOCK)
}

fn temp_path(name: &str) -> std::path::PathBuf {
    let path =
        std::env::temp_dir().join(format!("evm2-fork-{}-{name}/cache.json", std::process::id()));
    let _ = std::fs::remove_file(&path);
    path
}

/// Waits for the backend thread to drop its cache handle, which writes the cache file.
fn wait_for_file(path: &std::path::Path) {
    for _ in 0..200 {
        if path.exists() {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("{} was not written", path.display());
}

#[test]
fn reads_remote_state_at_the_pinned_block_and_caches_it() {
    let node = mock_node(Duration::ZERO);
    let backend = spawn(&node, BlockchainDb::new(meta(), None));
    let clone = backend.clone();

    let contract = backend.get_account(&CONTRACT).unwrap().expect("contract exists");
    assert_eq!(contract.nonce, 1);
    assert_eq!(contract.code_hash, keccak256(contract_code()));
    assert_eq!(
        backend.get_code_by_hash(&contract.code_hash).unwrap().original_bytes(),
        contract_code()
    );
    assert_eq!(backend.get_storage(&CONTRACT, &U256::from(1)).unwrap(), U256::from(40));
    assert_eq!(backend.get_storage(&CONTRACT, &U256::from(2)).unwrap(), U256::ZERO);
    assert_eq!(
        backend.get_block_hash(&U256::from(BLOCK - 1)).unwrap(),
        MockNode::block_hash_of(BLOCK - 1)
    );
    // Empty accounts read as absent.
    assert_eq!(backend.get_account(&CALLER).unwrap(), None);

    let calls = node.total_calls();
    // The first account fetch races eth_getAccountInfo; later ones only use separate requests.
    assert_eq!(node.calls("eth_getAccountInfo"), 1);
    assert_eq!(node.calls("eth_getCode"), 2);
    assert!(node.state_blocks().iter().all(|&block| block == BLOCK), "{:?}", node.state_blocks());

    // Clones share the cache, so repeated reads make no RPC calls.
    assert_eq!(clone.get_account(&RICH).unwrap().unwrap().balance, U256::from(2));
    assert_eq!(clone.get_account(&CALLER).unwrap(), None);
    assert_eq!(clone.get_storage(&CONTRACT, &U256::from(1)).unwrap(), U256::from(40));
    assert_eq!(
        clone.get_block_hash(&U256::from(BLOCK - 1)).unwrap(),
        MockNode::block_hash_of(BLOCK - 1)
    );
    assert_eq!(node.total_calls(), calls + 3);
}

#[test]
fn uses_eth_get_account_info_when_supported() {
    let node = mock_node(Duration::ZERO);
    node.set_supports_account_info(true);
    let db =
        BlockchainDb::new(meta().with_account_fetch_policy(AccountFetchPolicy::AccountInfo), None);
    let backend = spawn(&node, db);

    assert_eq!(backend.get_account(&CONTRACT).unwrap().unwrap().nonce, 1);
    assert_eq!(backend.get_account(&RICH).unwrap().unwrap().balance, U256::from(2));
    assert_eq!(node.calls("eth_getAccountInfo"), 2);
    assert_eq!(node.calls("eth_getBalance"), 0);

    let separate = spawn(
        &node,
        BlockchainDb::new(
            meta().with_account_fetch_policy(AccountFetchPolicy::SeparateRequests),
            None,
        ),
    );
    separate.get_account(&CONTRACT).unwrap();
    assert_eq!(node.calls("eth_getAccountInfo"), 2);
    assert_eq!(node.calls("eth_getBalance"), 1);
}

#[test]
fn concurrent_reads_share_one_request() {
    let node = mock_node(Duration::from_millis(200));
    let db = BlockchainDb::new(
        meta().with_account_fetch_policy(AccountFetchPolicy::SeparateRequests),
        None,
    );
    let backend = spawn(&node, db);

    let values = thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let backend = backend.clone();
                scope.spawn(move || {
                    let account = backend.get_account(&CONTRACT).unwrap().unwrap();
                    (account.nonce, backend.get_storage(&CONTRACT, &U256::from(1)).unwrap())
                })
            })
            .collect();
        handles.into_iter().map(|handle| handle.join().unwrap()).collect::<Vec<_>>()
    });

    assert_eq!(values, vec![(1, U256::from(40)); 8]);
    assert_eq!(node.calls("eth_getCode"), 1);
    assert_eq!(node.calls("eth_getStorageAt"), 1);
}

#[test]
fn executes_against_the_fork_and_keeps_writes_local() {
    let node = mock_node(Duration::ZERO);
    let backend = spawn(&node, BlockchainDb::new(meta(), None));
    let mut evm = Evm::<'static, BaseEvmTypes>::new(
        SpecId::OSAKA,
        BlockEnvExt::default(),
        ethereum_tx_registry(SpecId::OSAKA),
        ForkDB::new(DbRef::new(backend.clone())),
        Precompiles::base(SpecId::OSAKA),
    );
    let tx = TxEnvelope::Legacy(TxLegacy {
        to: TxKind::Call(CONTRACT),
        gas_limit: 100_000,
        ..Default::default()
    });

    let result = evm.transact(&Recovered::new_unchecked(tx, CALLER)).unwrap().commit();

    assert!(result.status, "{result:?}");
    assert_eq!(result.output, Bytes::from(U256::from(42).to_be_bytes::<32>()));
    // The write landed in the local overlay; the shared cache still holds remote state.
    assert_eq!(
        evm.state_mut().storage_slot_untracked(&CONTRACT, &U256::from(1)).unwrap(),
        U256::from(0x99)
    );
    assert_eq!(backend.data().storage(&CONTRACT, &U256::from(1)), Some(U256::from(40)));
}

#[test]
fn json_cache_round_trips_for_the_same_block_only() {
    let path = temp_path("roundtrip");
    let node = mock_node(Duration::ZERO);
    {
        let backend = spawn(
            &node,
            BlockchainDb::new(meta().with_url("http://a.example"), Some(path.clone())),
        );
        backend.get_account(&CONTRACT).unwrap();
        backend.get_storage(&CONTRACT, &U256::from(1)).unwrap();
        backend.get_block_hash(&U256::from(BLOCK - 1)).unwrap();
        // Dropping the last handle flushes the cache.
    }
    wait_for_file(&path);

    let offline = MockNode::default();
    let db = BlockchainDb::new(meta().with_url("ws://b.example"), Some(path.clone()));
    // Hosts are merged, not compared.
    assert_eq!(db.meta().hosts.into_iter().collect::<Vec<_>>(), ["a.example", "b.example"]);
    // Serve the offline fork from a copy without a cache path, so its backend thread never
    // rewrites the file the checks below read.
    let backend = spawn(&offline, BlockchainDb::from_data(db.snapshot(), None));
    drop(db);
    let contract = backend.get_account(&CONTRACT).unwrap().unwrap();
    assert_eq!(
        backend.get_code_by_hash(&contract.code_hash).unwrap().original_bytes(),
        contract_code()
    );
    assert_eq!(backend.get_storage(&CONTRACT, &U256::from(1)).unwrap(), U256::from(40));
    assert_eq!(
        backend.get_block_hash(&U256::from(BLOCK - 1)).unwrap(),
        MockNode::block_hash_of(BLOCK - 1)
    );
    assert_eq!(offline.total_calls(), 0);
    drop(backend);

    let skip_check =
        BlockchainDb::new_skip_check(BlockchainDbMeta::new(1, BLOCK + 1), Some(path.clone()));
    assert!(skip_check.account(&CONTRACT).is_some());
    drop(skip_check);
    assert!(BlockchainDb::new(BlockchainDbMeta::new(1, BLOCK + 1), Some(path.clone())).is_empty());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn reports_errors_with_context() {
    let node = mock_node(Duration::ZERO);
    let backend = spawn(&node, BlockchainDb::new(meta(), None));

    assert!(matches!(
        backend.get_block_hash(&U256::from(BLOCK + 50)),
        Err(ForkError::BlockNotFound(id)) if id == BlockId::number(BLOCK + 50)
    ));
    assert!(matches!(
        backend.get_block_hash(&U256::MAX),
        Err(ForkError::InvalidBlockNumber(U256::MAX))
    ));
    let hash = B256::repeat_byte(1);
    assert!(matches!(backend.get_code_by_hash(&hash), Err(ForkError::MissingCode(h)) if h == hash));
    assert!(matches!(
        backend.get_transaction(hash),
        Err(ForkError::TransactionNotFound(h)) if h == hash
    ));

    let mut db = DbRef::new(backend.clone());
    let error = DynDatabase::get_block_hash(&mut db, &U256::from(BLOCK + 50)).unwrap_err();
    assert!(error.is_fatal());
    assert!(error.downcast_ref::<ForkError>().is_some());

    // A fork pinned past the chain head fails every state read with context.
    backend.set_pinned_block(BLOCK + 50).unwrap();
    let error = backend.get_storage(&RICH, &U256::from(3)).unwrap_err();
    assert!(
        matches!(error, ForkError::GetStorage(RICH, slot, _) if slot == U256::from(3)),
        "{error}"
    );
    assert!(!error.is_possibly_non_archive_node_error());
}

#[test]
fn fetches_full_blocks_transactions_and_any_request() {
    let node = mock_node(Duration::ZERO);
    let tx_hash = node.insert_transaction(CALLER, 3);
    let backend = spawn(&node, BlockchainDb::new(meta(), None));

    let block = backend.get_full_block(BLOCK).unwrap();
    assert_eq!(block.header.number, BLOCK);
    assert_eq!(block.header.hash, MockNode::block_hash_of(BLOCK));
    assert!(matches!(
        backend.get_full_block(BLOCK + 50),
        Err(ForkError::BlockNotFound(id)) if id == BlockId::number(BLOCK + 50)
    ));

    let tx = backend.get_transaction(tx_hash).unwrap();
    assert_eq!(tx.from(), CALLER);

    let provider = node.provider();
    let chain_id = backend.do_any_request(async move { provider.get_chain_id().await }).unwrap();
    assert_eq!(chain_id, 1);
    let error =
        backend.do_any_request(async { Err::<(), _>(std::io::Error::other("boom")) }).unwrap_err();
    assert_eq!(error.to_string(), "failed to process AnyRequest: boom");
}

#[test]
fn updates_and_commits_shadow_the_remote() {
    let node = mock_node(Duration::ZERO);
    let backend = spawn(&node, BlockchainDb::new(meta(), None));
    let code = Bytecode::new_legacy(Bytes::from_static(&[op::STOP]));
    let info = AccountInfo::default().with_balance(U256::from(7)).with_code(code.clone());

    backend.insert_or_update_address(HashMap::from_iter([(CALLER, Some(info))]));
    backend.insert_or_update_storage(HashMap::from_iter([(
        CONTRACT,
        HashMap::from_iter([(U256::from(1), U256::from(5))]),
    )]));
    backend
        .insert_or_update_block_hashes(HashMap::from_iter([(U256::from(1), B256::repeat_byte(9))]));
    // Updates go through the handler; a request behind them sees them applied.
    backend.get_full_block(BLOCK).unwrap();

    assert_eq!(backend.get_account(&CALLER).unwrap().unwrap().balance, U256::from(7));
    assert_eq!(backend.get_code_by_hash(&code.hash_slow()).unwrap(), code);
    assert_eq!(backend.get_storage(&CONTRACT, &U256::from(1)).unwrap(), U256::from(5));
    assert_eq!(backend.get_block_hash(&U256::from(1)).unwrap(), B256::repeat_byte(9));
    assert_eq!(node.state_calls(), 0);

    // Committing a self-destruct wipes the account: its missing slots read as zero offline.
    let mut db = backend.data().clone();
    let deleted = AccountChangeRef {
        address: RICH,
        original: None,
        current: None,
        created: false,
        selfdestructed: true,
    };
    StateChangeSink::account(&mut db, deleted).unwrap();
    StateChangeSink::storage(
        &mut db,
        StorageChange {
            address: CONTRACT,
            key: U256::from(8),
            original: U256::ZERO,
            current: U256::from(1),
        },
    )
    .unwrap();
    assert_eq!(backend.get_account(&RICH).unwrap(), None);
    assert_eq!(backend.get_storage(&RICH, &U256::from(4)).unwrap(), U256::ZERO);
    assert_eq!(backend.get_storage(&CONTRACT, &U256::from(8)).unwrap(), U256::from(1));
    assert_eq!(node.state_calls(), 0);
}

#[test]
fn prefetched_entries_nothing_read_are_not_flushed() {
    let path = temp_path("prefetch");
    let node = mock_node(Duration::ZERO);
    {
        let backend = spawn(&node, BlockchainDb::new(meta(), Some(path.clone())))
            .with_blocking_mode(BlockingMode::Block);
        backend.prefetch_basic(CONTRACT).unwrap();
        backend.prefetch_basic(RICH).unwrap();
        backend.prefetch_storage(CONTRACT, U256::from(1)).unwrap();
        backend.prefetch_storage(CONTRACT, U256::from(2)).unwrap();
        let calls = node.state_calls();
        // Reads are served from the cache and keep the entries they touch.
        assert_eq!(backend.get_account(&RICH).unwrap().unwrap().balance, U256::from(2));
        assert_eq!(backend.get_storage(&CONTRACT, &U256::from(1)).unwrap(), U256::from(40));
        assert_eq!(node.state_calls(), calls);
    }
    wait_for_file(&path);

    let cached = BlockchainDb::new(meta(), Some(path.clone()));
    assert!(cached.account(&RICH).is_some());
    assert!(cached.account(&CONTRACT).is_none());
    assert_eq!(cached.storage(&CONTRACT, &U256::from(1)), Some(U256::from(40)));
    assert_eq!(cached.storage(&CONTRACT, &U256::from(2)), None);
    drop(cached);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn batched_prefetch_serves_later_reads() {
    for account_info in [true, false] {
        let node = mock_node(Duration::from_millis(5));
        node.set_supports_account_info(account_info);
        let backend = spawn(&node, BlockchainDb::new(meta(), None));
        let slots = (0..250).map(|slot| (CONTRACT, U256::from(slot)));
        let stats = backend.prefetch([CONTRACT, RICH], slots).unwrap();
        assert_eq!(stats, PrefetchStats { accounts: 2, slots: 250, failed: 0 });
        // Already cached keys are skipped.
        assert_eq!(
            backend.prefetch([RICH], [(CONTRACT, U256::from(1))]).unwrap(),
            PrefetchStats::default()
        );

        let calls = node.state_calls();
        assert_eq!(backend.get_account(&RICH).unwrap().unwrap().balance, U256::from(2));
        assert_eq!(backend.get_account(&CONTRACT).unwrap().unwrap().nonce, 1);
        assert_eq!(backend.get_storage(&CONTRACT, &U256::from(1)).unwrap(), U256::from(40));
        assert_eq!(backend.get_storage(&CONTRACT, &U256::from(249)).unwrap(), U256::ZERO);
        assert_eq!(node.state_calls(), calls);
        assert_eq!(node.calls("eth_getStorageAt"), 250);
    }
}

#[test]
fn prefetch_block_uses_the_prestate_trace() {
    let node = mock_node(Duration::ZERO);
    // The first value of each key is the one before the block; later ones are ignored.
    node.set_prestate(json!([
        { "txHash": B256::ZERO, "result": {
            format!("{CONTRACT}"): { "balance": "0x5", "nonce": 3, "code": contract_code(), "storage": { format!("{}", B256::with_last_byte(1)): format!("{}", B256::with_last_byte(7)) } },
        } },
        { "txHash": B256::ZERO, "result": {
            format!("{CONTRACT}"): { "balance": "0x6", "nonce": 4, "storage": { format!("{}", B256::with_last_byte(1)): format!("{}", B256::with_last_byte(8)) } },
            format!("{CALLER}"): { "balance": "0x0" },
        } },
    ]));
    // A backend not pinned to the block's parent ignores the trace.
    let other = spawn(&node, BlockchainDb::new(meta(), None));
    other.prefetch_block(BLOCK + 2).unwrap();
    assert_eq!(other.get_account(&CONTRACT).unwrap().unwrap().nonce, 1);
    assert_eq!(node.calls("debug_traceBlockByNumber"), 0);

    let calls = node.state_calls();
    let backend = spawn(&node, BlockchainDb::new(meta(), None));
    let stats = backend.prefetch_block(BLOCK + 1).unwrap();
    assert_eq!(stats, PrefetchStats { accounts: 2, slots: 1, failed: 0 });
    let account = backend.get_account(&CONTRACT).unwrap().unwrap();
    assert_eq!((account.balance, account.nonce), (U256::from(5), 3));
    assert_eq!(backend.get_storage(&CONTRACT, &U256::from(1)).unwrap(), U256::from(7));
    assert!(backend.get_account(&CALLER).unwrap().is_none());
    assert_eq!(node.state_calls(), calls);
}

#[test]
fn prefetch_block_falls_back_to_access_lists() {
    let node = mock_node(Duration::ZERO);
    let tx = node.insert_transaction(CALLER, 0);
    node.add_block_transaction(tx);
    node.set_access_list(json!([
        { "address": CONTRACT, "storageKeys": [B256::with_last_byte(1)] },
        { "address": RICH, "storageKeys": [] },
    ]));
    let backend = spawn(&node, BlockchainDb::new(meta(), None));
    let stats = backend.prefetch_block(BLOCK + 1).unwrap();
    assert_eq!(node.calls("eth_createAccessList"), 1);
    // CONTRACT, RICH, the sender and the block's miner.
    assert_eq!(stats.accounts, 4);
    assert_eq!(stats.slots, 1);

    let calls = node.state_calls();
    assert_eq!(backend.get_account(&RICH).unwrap().unwrap().balance, U256::from(2));
    assert_eq!(backend.get_storage(&CONTRACT, &U256::from(1)).unwrap(), U256::from(40));
    assert!(backend.get_account(&CALLER).unwrap().is_none());
    assert_eq!(node.state_calls(), calls);

    // Neither prediction works for an unknown block.
    assert!(matches!(backend.prefetch_block(BLOCK + 1000), Err(ForkError::BlockNotFound(_))));
}

#[test]
fn bal_post_state_prefills_the_cache() {
    let target = Address::repeat_byte(0xbb);
    let code = Bytes::from_static(&[op::STOP]);
    let mut account = AccountChanges::new(target);
    account.balance_changes.push(BalanceChange::new(BlockAccessIndex(1), U256::from(11)));
    account.nonce_changes.push(NonceChange::new(BlockAccessIndex(1), 4));
    account.code_changes.push(CodeChange::new(BlockAccessIndex(1), code.clone()));
    account.storage_changes.push(SlotChanges::new(
        U256::from(3),
        vec![
            BalStorageChange::new(BlockAccessIndex(1), U256::from(5)),
            BalStorageChange::new(BlockAccessIndex(2), U256::from(6)),
        ],
    ));
    let bal = vec![account];
    bal::validate_bal(&bal, 2, None).unwrap();
    assert!(matches!(
        bal::validate_bal(&bal, 2, Some(B256::ZERO)),
        Err(bal::BalError::HashMismatch { .. })
    ));

    let db = BlockchainDb::in_memory();
    db.insert_storage(target, U256::from(3), U256::from(1));
    assert_eq!(bal::cache_bal(&db, &bal), (1, 0));
    let info = db.account(&target).unwrap().unwrap();
    assert_eq!((info.balance, info.nonce), (U256::from(11), 4));
    assert_eq!(db.code_by_hash(&keccak256(&code)).unwrap().original_bytes(), code);
    // Entries already cached win.
    assert_eq!(db.storage(&target, &U256::from(3)), Some(U256::from(1)));
}

#[tokio::test(flavor = "multi_thread")]
async fn reads_from_inside_a_multi_threaded_runtime() {
    let node = mock_node(Duration::ZERO);
    let backend = SharedBackend::spawn_backend(
        node.provider(),
        BlockchainDb::in_memory(),
        Some(BLOCK.into()),
    );

    assert_eq!(backend.get_storage(&CONTRACT, &U256::from(1)).unwrap(), U256::from(40));

    backend.set_pinned_block(BLOCK + 1).unwrap();
    backend.get_account(&RICH).unwrap();
    assert_eq!(node.state_blocks().last(), Some(&(BLOCK + 1)));

    let (env, block) = fork_env(&node.provider(), BLOCK.into()).await.unwrap();
    assert_eq!((env.chain_id, env.block_number, env.block_hash), (1, BLOCK, block.header.hash));
    assert_eq!(env.block.basefee, U256::from(7));
    assert_eq!(env.block.timestamp, U256::from(1_700_000_000 + BLOCK * 12));
    // The mock's timestamps fall in mainnet's Shanghai era.
    assert_eq!(env.spec_id, SpecId::SHANGHAI);
}
