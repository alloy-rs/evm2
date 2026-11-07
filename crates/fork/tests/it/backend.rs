use crate::mock::{MockAccount, MockNode};
use alloy_consensus::{TxLegacy, transaction::Recovered};
use alloy_primitives::{Address, B256, Bytes, TxKind, U256, keccak256, map::HashMap};
use evm2::{
    BaseEvmTypes, Evm, Precompiles, SpecId,
    bytecode::Bytecode,
    env::BlockEnvExt,
    ethereum::{TxEnvelope, ethereum_tx_registry},
    evm::{DatabaseRef, DbRef, DynDatabase},
    interpreter::op,
};
use evm2_fork::{BlockchainDb, BlockchainDbMeta, ForkDB, ForkError, SharedBackend};
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
    node.insert_block_hash(BLOCK - 1, keccak256("parent"));
    node
}

fn spawn(node: &MockNode, db: BlockchainDb) -> SharedBackend {
    SharedBackend::spawn_backend_thread(node.provider(), db, Some(BLOCK.into()))
}

const fn meta() -> BlockchainDbMeta {
    BlockchainDbMeta::new(1, BLOCK)
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
    assert_eq!(backend.get_block_hash(&U256::from(BLOCK - 1)).unwrap(), keccak256("parent"));
    // Empty accounts read as absent.
    assert_eq!(backend.get_account(&CALLER).unwrap(), None);

    let calls = node.total_calls();
    assert_eq!(calls, 3 * 2 + 2 + 1);
    assert!(node.block_tags().iter().all(|tag| tag == "0x64"), "{:?}", node.block_tags());

    // Clones share the cache, so repeated reads make no RPC calls.
    assert_eq!(clone.get_account(&RICH).unwrap().unwrap().balance, U256::from(2));
    assert_eq!(clone.get_account(&CALLER).unwrap(), None);
    assert_eq!(clone.get_storage(&CONTRACT, &U256::from(1)).unwrap(), U256::from(40));
    assert_eq!(clone.get_block_hash(&U256::from(BLOCK - 1)).unwrap(), keccak256("parent"));
    assert_eq!(node.total_calls(), calls + 3);
}

#[test]
fn concurrent_reads_share_one_request() {
    let node = mock_node(Duration::from_millis(200));
    let backend = spawn(&node, BlockchainDb::new(meta(), None));

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
    let path = std::env::temp_dir().join(format!("evm2-fork-{}/cache.json", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let node = mock_node(Duration::ZERO);
    {
        let backend = spawn(&node, BlockchainDb::new(meta(), Some(path.clone())));
        backend.get_account(&CONTRACT).unwrap();
        backend.get_storage(&CONTRACT, &U256::from(1)).unwrap();
        backend.get_block_hash(&U256::from(BLOCK - 1)).unwrap();
        // Dropping the last handle flushes the cache.
    }
    // Give the backend thread a moment to drop its copy of the cache.
    for _ in 0..100 {
        if path.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }

    let offline = MockNode::default();
    let backend = spawn(&offline, BlockchainDb::new(meta(), Some(path.clone())));
    let contract = backend.get_account(&CONTRACT).unwrap().unwrap();
    assert_eq!(
        backend.get_code_by_hash(&contract.code_hash).unwrap().original_bytes(),
        contract_code()
    );
    assert_eq!(backend.get_storage(&CONTRACT, &U256::from(1)).unwrap(), U256::from(40));
    assert_eq!(backend.get_block_hash(&U256::from(BLOCK - 1)).unwrap(), keccak256("parent"));
    assert_eq!(offline.total_calls(), 0);

    let other_block = BlockchainDb::new(BlockchainDbMeta::new(1, BLOCK + 1), Some(path.clone()));
    assert_eq!(other_block.account(&CONTRACT), None);
    drop(other_block);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn reports_missing_state_as_errors() {
    let node = mock_node(Duration::ZERO);
    let backend = spawn(&node, BlockchainDb::new(meta(), None));

    assert!(matches!(
        backend.get_block_hash(&U256::from(BLOCK + 5)),
        Err(ForkError::BlockNotFound(105))
    ));
    assert!(matches!(
        backend.get_block_hash(&U256::MAX),
        Err(ForkError::InvalidBlockNumber(U256::MAX))
    ));
    let hash = B256::repeat_byte(1);
    assert!(matches!(backend.get_code_by_hash(&hash), Err(ForkError::MissingCode(h)) if h == hash));

    let mut db = DbRef::new(backend);
    let error = DynDatabase::get_block_hash(&mut db, &U256::from(BLOCK + 5)).unwrap_err();
    assert!(error.is_fatal());
    assert!(error.downcast_ref::<ForkError>().is_some());
}

#[test]
fn cached_entries_shadow_the_remote() {
    let node = mock_node(Duration::ZERO);
    let backend = spawn(&node, BlockchainDb::new(meta(), None));
    let code = Bytecode::new_legacy(Bytes::from_static(&[op::STOP]));
    backend.data().insert_account(
        CALLER,
        Some(evm2::evm::AccountInfo::default().with_balance(U256::from(7)).with_code(code.clone())),
    );
    backend.data().insert_storage(CONTRACT, U256::from(1), U256::from(5));

    assert_eq!(backend.get_account(&CALLER).unwrap().unwrap().balance, U256::from(7));
    assert_eq!(backend.get_code_by_hash(&code.hash_slow()).unwrap(), code);
    assert_eq!(backend.get_storage(&CONTRACT, &U256::from(1)).unwrap(), U256::from(5));
    assert_eq!(node.total_calls(), 0);
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
    assert_eq!(node.block_tags().last().map(String::as_str), Some("0x65"));
}
