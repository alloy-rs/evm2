use crate::mock::{MockAccount, MockNode};
use alloy_eips::BlockId;
use alloy_primitives::{Address, U256, map::HashMap};
use evm2::evm::{AccountInfo, DatabaseRef, DynDatabase, StateChangeSink, StorageChange};
use evm2_fork::{
    BlockchainDb, BlockchainDbMeta, ForkedDatabase, RevertStateSnapshotAction, SharedBackend,
    StateSnapshots,
};
use std::time::Duration;

const ACCOUNT: Address = Address::repeat_byte(0xa1);
const OTHER: Address = Address::repeat_byte(0xb2);
const SLOT: U256 = U256::from_limbs([1, 0, 0, 0]);
const FIRST_BLOCK: u64 = 3;
const SECOND_BLOCK: u64 = 5;

fn account(balance: u64, slot: u64) -> MockAccount {
    MockAccount {
        balance: U256::from(balance),
        nonce: 1,
        storage: HashMap::from_iter([(SLOT, U256::from(slot))]),
        ..Default::default()
    }
}

/// A node where `ACCOUNT` changes at `SECOND_BLOCK`.
fn mock_node() -> MockNode {
    let node = MockNode::default();
    node.add_blocks(10);
    node.insert_account(ACCOUNT, account(100, 10));
    node.insert_account_at(SECOND_BLOCK, ACCOUNT, account(500, 50));
    node.insert_account(OTHER, account(7, 70));
    node
}

fn forked_db(node: &MockNode) -> ForkedDatabase {
    let db = BlockchainDb::new(BlockchainDbMeta::new(1, FIRST_BLOCK), None);
    let backend =
        SharedBackend::spawn_backend_thread(node.provider(), db, Some(FIRST_BLOCK.into()));
    ForkedDatabase::new(backend)
}

fn storage(db: &mut ForkedDatabase) -> U256 {
    DynDatabase::get_storage(db, &ACCOUNT, &SLOT).unwrap()
}

fn write_storage(db: &mut ForkedDatabase, original: u64, current: u64) {
    let change = StorageChange {
        address: ACCOUNT,
        key: SLOT,
        original: U256::from(original),
        current: U256::from(current),
    };
    let Ok(()) = db.storage(change);
}

#[test]
fn revert_restores_committed_storage() {
    let node = mock_node();
    let mut db = forked_db(&node);
    assert_eq!(storage(&mut db), U256::from(10));

    let id = db.insert_state_snapshot();
    write_storage(&mut db, 10, 99);
    assert_eq!(storage(&mut db), U256::from(99));

    assert!(db.revert_state_snapshot(id, RevertStateSnapshotAction::RevertRemove));
    assert_eq!(storage(&mut db), U256::from(10));
    assert!(db.state_snapshots().lock().unwrap().get(id).is_none());
    assert!(!db.revert_state_snapshot(id, RevertStateSnapshotAction::RevertRemove));
    // The original value came from the overlay both times.
    assert_eq!(node.calls("eth_getStorageAt"), 1);
}

#[test]
fn revert_keep_allows_reverting_again_and_drops_later_snapshots() {
    let node = mock_node();
    let mut db = forked_db(&node);
    assert_eq!(storage(&mut db), U256::from(10));

    let first = db.insert_state_snapshot();
    write_storage(&mut db, 10, 99);
    let second = db.insert_state_snapshot();
    write_storage(&mut db, 99, 77);

    assert!(db.revert_state_snapshot(first, RevertStateSnapshotAction::RevertKeep));
    assert_eq!(storage(&mut db), U256::from(10));
    {
        let snapshots = db.state_snapshots().lock().unwrap();
        assert!(snapshots.get(first).is_some());
        assert!(snapshots.get(second).is_none());
    }

    write_storage(&mut db, 10, 55);
    assert_eq!(storage(&mut db), U256::from(55));
    assert!(db.revert_state_snapshot(first, RevertStateSnapshotAction::RevertKeep));
    assert_eq!(storage(&mut db), U256::from(10));
    assert!(!db.revert_state_snapshot(second, RevertStateSnapshotAction::RevertKeep));
}

#[test]
fn revert_restores_the_remote_cache() {
    let node = mock_node();
    let mut db = forked_db(&node);
    assert!(DynDatabase::get_account(&mut db, &ACCOUNT).unwrap().is_some());

    let id = db.insert_state_snapshot();
    assert!(DynDatabase::get_account(&mut db, &OTHER).unwrap().is_some());
    assert!(db.inner().account(&OTHER).is_some());

    assert!(db.revert_state_snapshot(id, RevertStateSnapshotAction::RevertRemove));
    assert!(db.inner().account(&OTHER).is_none());
    assert!(db.inner().account(&ACCOUNT).is_some());

    // The reverted entry is fetched again.
    let calls = node.state_calls();
    assert_eq!(DynDatabase::get_account(&mut db, &OTHER).unwrap().unwrap().balance, U256::from(7));
    assert!(node.state_calls() > calls);
}

#[test]
fn reset_reads_the_new_block() {
    let node = mock_node();
    let mut db = forked_db(&node);
    let info = DynDatabase::get_account(&mut db, &ACCOUNT).unwrap().unwrap();
    assert_eq!(info.balance, U256::from(100));
    assert_eq!(storage(&mut db), U256::from(10));
    write_storage(&mut db, 10, 99);
    let before = node.state_blocks().len();
    assert!(node.state_blocks().iter().all(|&block| block == FIRST_BLOCK));

    db.reset(SECOND_BLOCK).unwrap();
    assert!(db.inner().is_empty());
    let info = DynDatabase::get_account(&mut db, &ACCOUNT).unwrap().unwrap();
    assert_eq!(info.balance, U256::from(500));
    assert_eq!(storage(&mut db), U256::from(50));

    let blocks = node.state_blocks();
    assert!(blocks.len() > before);
    assert!(blocks[before..].iter().all(|&block| block == SECOND_BLOCK));
}

#[test]
fn reset_to_a_tag_names_and_pins_the_resolved_block() {
    let node = mock_node();
    let mut db = forked_db(&node);
    db.reset(BlockId::latest()).unwrap();
    let meta = db.inner().meta();
    assert_eq!((meta.block_number, meta.block_hash), (10, Some(MockNode::block_hash_of(10))));

    db.reset(SECOND_BLOCK).unwrap();
    let meta = db.inner().meta();
    assert_eq!((meta.block_number, meta.block_hash), (SECOND_BLOCK, None));
}

#[test]
fn fetches_in_flight_across_a_reset_are_not_cached() {
    let node = mock_node();
    node.set_delay(Duration::from_millis(200));
    let mut db = forked_db(&node);
    let backend = db.backend().clone();
    let reader = std::thread::spawn(move || backend.get_account(&ACCOUNT).unwrap().unwrap());
    std::thread::sleep(Duration::from_millis(50));
    db.reset(SECOND_BLOCK).unwrap();
    // The reader that joined before the reset is served at the new pin.
    assert_eq!(reader.join().unwrap().balance, U256::from(500));
    assert_eq!(db.inner().account(&ACCOUNT).unwrap().unwrap().balance, U256::from(500));
    let info = DynDatabase::get_account(&mut db, &ACCOUNT).unwrap().unwrap();
    assert_eq!(info.balance, U256::from(500));
}

#[test]
fn reset_drops_snapshots_of_the_old_block() {
    let node = mock_node();
    let mut db = forked_db(&node);
    write_storage(&mut db, 10, 99);
    let id = db.insert_state_snapshot();
    db.reset(SECOND_BLOCK).unwrap();
    assert!(db.state_snapshots().lock().unwrap().is_empty());
    assert!(!db.revert_state_snapshot(id, RevertStateSnapshotAction::RevertRemove));
    assert_eq!(storage(&mut db), U256::from(50));
}

#[test]
fn snapshots_of_a_clone_from_before_a_reset_are_never_restored() {
    let node = mock_node();
    let mut db = forked_db(&node);
    write_storage(&mut db, 10, 99);
    let stale = db.clone();
    db.reset(SECOND_BLOCK).unwrap();
    // A clone that missed the reset still snapshots its old overlay; it must not come back.
    let id = stale.insert_state_snapshot();
    assert!(!db.revert_state_snapshot(id, RevertStateSnapshotAction::RevertKeep));
    assert_eq!(storage(&mut db), U256::from(50));

    // Snapshots taken after the reset still work, from any clone.
    let id = db.insert_state_snapshot();
    let mut other = db.clone();
    write_storage(&mut db, 50, 77);
    assert!(other.revert_state_snapshot(id, RevertStateSnapshotAction::RevertRemove));
    assert_eq!(storage(&mut other), U256::from(50));
}

#[test]
fn reads_go_through_the_overlay_first() {
    let node = mock_node();
    let mut db = forked_db(&node);
    db.database_mut()
        .insert_account_info(&ACCOUNT, AccountInfo::default().with_balance(U256::from(3)));
    db.database_mut().insert_account_storage(&ACCOUNT, &SLOT, &U256::from(4));

    assert_eq!(
        DynDatabase::get_account(&mut db, &ACCOUNT).unwrap().unwrap().balance,
        U256::from(3)
    );
    assert_eq!(storage(&mut db), U256::from(4));
    assert_eq!(DatabaseRef::get_account(&db, &ACCOUNT).unwrap().unwrap().balance, U256::from(3));
    assert_eq!(DatabaseRef::get_storage(&db, &ACCOUNT, &SLOT).unwrap(), U256::from(4));
    assert_eq!(node.state_calls(), 0);
}

#[test]
fn database_ref_reads_do_not_fill_the_overlay() {
    let node = mock_node();
    let db = forked_db(&node);
    assert_eq!(DatabaseRef::get_storage(&db, &OTHER, &SLOT).unwrap(), U256::from(70));
    assert!(db.database().cache.storage.is_empty());
    assert_eq!(db.inner().storage(&OTHER, &SLOT), Some(U256::from(70)));
}

#[test]
fn state_snapshot_reads_overlay_then_remote_snapshot_then_backend() {
    let node = mock_node();
    let mut db = forked_db(&node);
    assert_eq!(DatabaseRef::get_storage(&db, &OTHER, &SLOT).unwrap(), U256::from(70));
    write_storage(&mut db, 10, 99);

    let snapshot = db.create_state_snapshot();
    // Later remote cache changes are not part of the snapshot.
    db.inner().insert_storage(OTHER, SLOT, U256::from(1234));

    assert_eq!(snapshot.get_storage(&ACCOUNT, &SLOT).unwrap(), U256::from(99));
    assert_eq!(snapshot.get_storage(&OTHER, &SLOT).unwrap(), U256::from(70));
    assert_eq!(DatabaseRef::get_storage(&db, &OTHER, &SLOT).unwrap(), U256::from(1234));
    assert_eq!(node.calls("eth_getStorageAt"), 1);

    // Anything neither layer holds comes from the backend.
    assert_eq!(snapshot.get_account(&ACCOUNT).unwrap().unwrap().balance, U256::from(100));
}

#[test]
fn state_snapshots_remove_later_ids() {
    let mut snapshots = StateSnapshots::default();
    assert!(snapshots.is_empty());
    let ids = ["a", "b", "c"].map(|value| snapshots.insert(value));
    assert_eq!(ids, [U256::ZERO, U256::from(1), U256::from(2)]);

    snapshots.insert_at("z", U256::from(9));
    assert_eq!(snapshots.remove(ids[1]), Some("b"));
    assert_eq!(snapshots.get(ids[0]), Some(&"a"));
    assert_eq!(snapshots.get(ids[2]), None);
    assert_eq!(snapshots.get(U256::from(9)), Some(&"z"));

    assert_eq!(snapshots.insert("d"), U256::from(3));
    assert_eq!(snapshots.remove_at(ids[0]), Some("a"));
    assert_eq!(snapshots.get(U256::from(3)), Some(&"d"));

    snapshots.clear();
    assert!(snapshots.is_empty());
    assert_eq!(snapshots.insert("e"), U256::from(4));
}

#[test]
fn revert_action_defaults_to_keep() {
    assert!(RevertStateSnapshotAction::default().is_keep());
    assert!(!RevertStateSnapshotAction::RevertRemove.is_keep());
}
