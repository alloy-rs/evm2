use crate::mock::{MockAccount, MockNode};
use alloy_eips::eip7928::{
    AccountChanges, BalanceChange, BlockAccessIndex, CodeChange, NonceChange, SlotChanges,
    StorageChange,
};
use alloy_primitives::{Address, Bytes, U256, map::HashMap};
use alloy_provider::Provider;
use evm2::evm::DatabaseRef;
use evm2_fork::{
    BlockchainDb, BlockchainDbMeta, CreateFork, ForkError, ForkId, JsonBlockCacheData, MultiFork,
    SharedBackend,
};
use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Barrier},
    thread,
    time::{Duration, Instant},
};

const URL: &str = "http://mock";
const LATEST: u64 = 10;
const ALICE: Address = Address::repeat_byte(0xa1);
const BOB: Address = Address::repeat_byte(0xb0);
const TOKEN: Address = Address::repeat_byte(0x70);

/// Alice's balance is `block + 1` from blocks 0, 5 and 8 on; the token has slot 1 set to 7.
fn mock_node(delay: Duration) -> MockNode {
    let node = MockNode::with_delay(delay);
    node.add_blocks(LATEST);
    for block in [0, 5, 8] {
        let account = MockAccount { balance: U256::from(block + 1), ..Default::default() };
        node.insert_account_at(block, ALICE, account);
    }
    let storage = HashMap::from_iter([(U256::from(1), U256::from(7))]);
    node.insert_account(TOKEN, MockAccount { nonce: 1, storage, ..Default::default() });
    node
}

fn multi_fork(node: &MockNode) -> MultiFork {
    let node = node.clone();
    MultiFork::spawn_with_connector(move |_| Ok(node.provider().erased()))
}

fn balance(backend: &SharedBackend, address: Address) -> U256 {
    backend.get_account(&address).unwrap().map(|info| info.balance).unwrap_or_default()
}

/// A fresh, empty directory unique to `name`.
fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("evm2-fork-multi-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    dir
}

fn read_cache(path: PathBuf) -> JsonBlockCacheData {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

#[test]
fn fork_ids() {
    assert_eq!(ForkId::new(URL, Some(16)).as_str(), "http://mock@0x10");
    assert_eq!(ForkId::new(URL, None).to_string(), "http://mock@latest");
    assert_eq!(ForkId::from("x"), ForkId("x".to_string()));
}

#[test]
fn creates_forks_at_latest_and_at_a_block() {
    let node = mock_node(Duration::ZERO);
    let multi = multi_fork(&node);

    let latest = multi.create_fork(CreateFork::new(URL)).unwrap();
    assert_eq!(latest.id, ForkId::new(URL, Some(LATEST)));
    assert_eq!(latest.env.block_number, LATEST);
    assert_eq!(latest.env.block_hash, MockNode::block_hash_of(LATEST));
    assert_eq!(latest.env.chain_id, 1);
    assert_eq!(balance(&latest.backend, ALICE), U256::from(9));
    let latest_reads = node.state_blocks().len();

    let at_five = multi.create_fork(CreateFork::new(URL).with_block(Some(5))).unwrap();
    assert_eq!(at_five.id, ForkId::new(URL, Some(5)));
    assert_eq!(at_five.env.block_number, 5);
    assert_eq!(balance(&at_five.backend, ALICE), U256::from(6));

    let blocks = node.state_blocks();
    assert!(latest_reads > 0 && blocks.len() > latest_reads);
    assert!(blocks[..latest_reads].iter().all(|&block| block == LATEST), "{blocks:?}");
    assert!(blocks[latest_reads..].iter().all(|&block| block == 5), "{blocks:?}");
}

#[test]
fn same_block_shares_the_backend() {
    let node = mock_node(Duration::ZERO);
    let multi = multi_fork(&node);
    let opts = CreateFork::new(URL).with_block(Some(5));

    let first = multi.create_fork(opts.clone()).unwrap();
    let second = multi.create_fork(opts.clone()).unwrap();
    assert_eq!(second.id.as_str(), "http://mock@0x5-1");
    assert_eq!(multi.create_fork(opts).unwrap().id.as_str(), "http://mock@0x5-2");

    assert_eq!(balance(&first.backend, ALICE), U256::from(6));
    let calls = node.state_calls();
    assert_eq!(balance(&second.backend, ALICE), U256::from(6));
    let shared = multi.get_fork(&second.id).unwrap();
    assert_eq!(balance(&shared, ALICE), U256::from(6));
    assert_eq!(node.state_calls(), calls);
}

#[test]
fn concurrent_identical_creations_resolve_once() {
    let node = mock_node(Duration::from_millis(50));
    let multi = multi_fork(&node);
    let barrier = Arc::new(Barrier::new(2));
    let threads = (0..2)
        .map(|_| {
            let (multi, barrier) = (multi.clone(), Arc::clone(&barrier));
            thread::spawn(move || {
                barrier.wait();
                multi.create_fork(CreateFork::new(URL).with_block(Some(5))).unwrap().id
            })
        })
        .collect::<Vec<_>>();
    let mut ids = threads.into_iter().map(|thread| thread.join().unwrap().0).collect::<Vec<_>>();
    ids.sort();
    assert_eq!(ids, ["http://mock@0x5", "http://mock@0x5-1"]);
    assert_eq!(node.calls("eth_chainId"), 1);
}

#[test]
fn roll_fork_reads_the_new_block() {
    let node = mock_node(Duration::ZERO);
    let multi = multi_fork(&node);
    let fork = multi.create_fork(CreateFork::new(URL).with_block(Some(5))).unwrap();

    let rolled = multi.roll_fork(&fork.id, 8).unwrap();
    assert_eq!(rolled.id, ForkId::new(URL, Some(8)));
    assert_eq!(rolled.env.block_number, 8);
    assert_eq!(balance(&rolled.backend, ALICE), U256::from(9));
    assert!(node.state_blocks().iter().all(|&block| block == 8));
    assert_eq!(multi.get_fork_options(&rolled.id).unwrap().block, Some(8));

    let unknown = multi.roll_fork(&ForkId::from("unknown"), 8);
    assert!(matches!(unknown, Err(ForkError::UnknownFork(id)) if id == "unknown"));
}

#[test]
fn getters_return_the_fork_and_its_options() {
    let node = mock_node(Duration::ZERO);
    let multi = multi_fork(&node);
    let opts = CreateFork::new(URL).with_block(Some(5)).with_cache_root(None);
    let fork = multi.create_fork(opts.clone()).unwrap();

    assert_eq!(multi.get_env(&fork.id), Some(fork.env.clone()));
    assert_eq!(multi.get_fork_url(&fork.id).as_deref(), Some(URL));
    assert_eq!(multi.get_fork_options(&fork.id), Some(opts));
    assert!(multi.get_fork(&fork.id).is_some());

    let unknown = ForkId::from("unknown");
    assert!(multi.get_fork(&unknown).is_none());
    assert!(multi.get_env(&unknown).is_none());
    assert!(multi.get_fork_url(&unknown).is_none());
    assert!(multi.get_fork_options(&unknown).is_none());
}

#[test]
fn disk_cache_is_written_and_reused() {
    let root = temp_dir("disk-cache");
    let opts = CreateFork::new(URL)
        .with_block(Some(5))
        .with_caching(true)
        .with_cache_root(Some(root.clone()));

    let node = mock_node(Duration::ZERO);
    node.set_chain_id(7);
    let multi = multi_fork(&node);
    let fork = multi.create_fork(opts.clone()).unwrap();
    assert_eq!(balance(&fork.backend, ALICE), U256::from(6));
    assert_eq!(fork.backend.get_storage(&TOKEN, &U256::from(1)).unwrap(), U256::from(7));
    drop(fork);
    drop(multi);
    let path = root.join("7").join("5");
    assert!(read_cache(path).accounts.contains_key(&ALICE));

    let node = mock_node(Duration::ZERO);
    node.set_chain_id(7);
    let multi = multi_fork(&node);
    let fork = multi.create_fork(opts).unwrap();
    assert_eq!(balance(&fork.backend, ALICE), U256::from(6));
    assert_eq!(fork.backend.get_storage(&TOKEN, &U256::from(1)).unwrap(), U256::from(7));
    assert_eq!(node.state_calls(), 0);
    drop(fork);
    drop(multi);
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn state_by_number_never_uses_the_disk_cache() {
    let root = temp_dir("state-by-number");
    let opts = CreateFork::new(URL)
        .with_block(Some(5))
        .with_caching(true)
        .with_cache_root(Some(root.clone()))
        .with_state_by_number(true);

    let node = mock_node(Duration::ZERO);
    let multi = multi_fork(&node);
    let fork = multi.create_fork(opts).unwrap();
    assert_eq!(fork.id.as_str(), "http://mock@0x5#state-by-number");
    assert_eq!(balance(&fork.backend, ALICE), U256::from(6));
    assert!(node.state_blocks().iter().all(|&block| block == 5));
    drop(fork);
    drop(multi);
    assert!(!root.exists());
}

#[test]
fn latest_fork_prefetches_keys_of_the_previous_cache() {
    let root = temp_dir("prefetch");
    let slot = U256::from(1);
    let previous =
        BlockchainDb::new(BlockchainDbMeta::new(1, LATEST - 1), Some(root.join("1").join("9")));
    previous.insert_account(ALICE, None);
    previous.insert_account(TOKEN, None);
    previous.insert_storage(TOKEN, slot, U256::ZERO);
    previous.flush().unwrap();
    drop(previous);

    let node = mock_node(Duration::ZERO);
    let multi = multi_fork(&node);
    let opts = CreateFork::new(URL)
        .with_caching(true)
        .with_cache_root(Some(root.clone()))
        .with_prefetch(true);
    let fork = multi.create_fork(opts).unwrap();
    assert_eq!(fork.env.block_number, LATEST);

    // Wait until every key is fetched and marked as prefetched.
    let db = fork.backend.data();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let snapshot = db.snapshot();
        if db.account(&ALICE).is_some()
            && db.account(&TOKEN).is_some()
            && db.storage(&TOKEN, &slot).is_some()
            && snapshot.accounts.is_empty()
            && snapshot.storage.values().all(|slots| slots.is_empty())
        {
            break;
        }
        assert!(Instant::now() < deadline, "prefetch did not finish");
        thread::sleep(Duration::from_millis(5));
    }
    let blocks = node.state_blocks();
    assert!(!blocks.is_empty() && blocks.iter().all(|&block| block == LATEST), "{blocks:?}");
    assert_eq!(db.storage(&TOKEN, &slot), Some(U256::from(7)));

    // Reading a prefetched key costs no request and keeps it in the cache file.
    let calls = node.state_calls();
    assert_eq!(fork.backend.get_account(&TOKEN).unwrap().unwrap().nonce, 1);
    assert_eq!(node.state_calls(), calls);
    drop(fork);
    drop(multi);

    let cache = read_cache(root.join("1").join(LATEST.to_string()));
    assert!(cache.accounts.contains_key(&TOKEN));
    assert!(!cache.accounts.contains_key(&ALICE));
    assert!(cache.storage.get(&TOKEN).is_none_or(|slots| !slots.contains_key(&slot)));
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn prewarm_bal_fills_the_cache() {
    let node = mock_node(Duration::ZERO);
    let index = BlockAccessIndex::new(1);
    let bal = vec![
        AccountChanges::new(TOKEN).with_storage_change(SlotChanges::new(
            U256::from(2),
            vec![StorageChange::new(index, U256::from(99))],
        )),
        AccountChanges::new(BOB)
            .with_balance_change(BalanceChange::new(index, U256::from(42)))
            .with_nonce_change(NonceChange::new(index, 3))
            .with_code_change(CodeChange::new(index, Bytes::new())),
    ];
    node.insert_bal(LATEST, bal);
    let multi = multi_fork(&node);

    let fork = multi.create_fork(CreateFork::new(URL).with_prewarm_bal(true)).unwrap();
    assert_eq!(node.calls("eth_getBlockAccessList"), 1);
    let bob = fork.backend.get_account(&BOB).unwrap().unwrap();
    assert_eq!((bob.balance, bob.nonce), (U256::from(42), 3));
    assert_eq!(fork.backend.get_storage(&TOKEN, &U256::from(2)).unwrap(), U256::from(99));
    assert_eq!(node.state_calls(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn works_inside_a_multi_threaded_runtime() {
    let node = mock_node(Duration::ZERO);
    let multi = multi_fork(&node);
    let fork = multi.create_fork(CreateFork::new(URL).with_block(Some(5))).unwrap();
    assert_eq!(balance(&fork.backend, ALICE), U256::from(6));
    assert_eq!(multi.get_env(&fork.id), Some(fork.env));
    drop(multi);
    // The backend outlives the last handle.
    assert_eq!(balance(&fork.backend, TOKEN), U256::ZERO);
}
