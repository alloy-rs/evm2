use super::*;
use crate::{
    ErrorCode, SpecId, Version,
    bytecode::Bytecode,
    evm::{AccountInfo, CacheDB, DbResult, DynDatabase, State, Tracked},
    interpreter::Word,
};
use alloy_primitives::{Address, B256};

const OWNER: Address = Address::with_last_byte(0x71);
const OTHER: Address = Address::with_last_byte(0x72);
const KEY: Word = Word::from_limbs([1, 0, 0, 0]);

fn database(value: u64) -> CacheDB {
    let mut database = CacheDB::default();
    for address in [OWNER, OTHER] {
        database.insert_account_info(&address, AccountInfo::default().with_balance(Word::from(1)));
        database.insert_account_storage(&address, &KEY, &Word::from(value));
    }
    database
}

fn assert_pool_bound(pool: &StoragePool) {
    assert!(pool.maps.len() <= MAX_MAPS);
    assert!(pool.capacity <= MAX_TOTAL_CAPACITY);
    assert_eq!(pool.capacity, pool.maps.iter().map(|map| map.capacity()).sum::<usize>());
    for map in &pool.maps {
        assert!(map.is_empty());
        assert!(map.capacity() > 0 && map.capacity() <= MAX_MAP_CAPACITY);
    }
}

#[test]
fn commit_reuses_capacity_but_resets_loaded_original_and_warmth() {
    let mut state = State::new(database(3));
    {
        let mut slot = state.storage_slot(&OWNER, KEY, false).unwrap();
        assert!(slot.warm());
        assert_eq!(slot.write(Word::from(7)), (Word::from(3), Word::from(3)));
    }
    let capacity = state.storage[&OWNER].slots.capacity();
    state.commit_transaction();
    state.clear_transaction_state();
    state.clear_transaction_state();
    assert!(state.storage.is_empty());
    assert_eq!(state.storage_pool.capacity, capacity);
    assert_pool_bound(&state.storage_pool);

    // Reserving an allocation must not make the old key loaded or warm this transaction.
    assert!(!state.storage(&OWNER).is_loaded(&KEY));
    assert_eq!(state.storage[&OWNER].slots.capacity(), capacity);
    assert_eq!(state.storage_pool.capacity, 0);
    assert_eq!(state.storage_slot(&OWNER, KEY, true).unwrap_err(), ErrorCode::COLD_LOAD_SKIPPED);
    assert_eq!(state.get_storage(&OWNER, &KEY), None);
    let mut slot = state.storage_slot(&OWNER, KEY, false).unwrap();
    assert_eq!((slot.original(), slot.current()), (Word::from(7), Word::from(7)));
    assert!(slot.warm());
    assert_eq!(slot.write(Word::from(9)), (Word::from(7), Word::from(7)));
    state.clear_transaction_state();
    assert_eq!(state.read_committed_storage(&OWNER, &KEY).unwrap(), Word::from(7));

    state.prewarm_storage_slot(&OWNER, KEY);
    assert!(state.storage_slot(&OWNER, KEY, true).unwrap().is_warm());
    state.clear_transaction_state();
    assert_eq!(state.storage_slot(&OWNER, KEY, true).unwrap_err(), ErrorCode::COLD_LOAD_SKIPPED);
}

#[test]
fn wiped_allocation_reused_by_another_owner_has_no_values_or_wipe_flag() {
    let mut state = State::new(database(3));
    state.storage_slot(&OWNER, KEY, false).unwrap().write(Word::from(7));
    let capacity = state.storage[&OWNER].slots.capacity();
    state.storage(&OWNER).wipe();
    state.commit_transaction();
    state.clear_transaction_state();
    assert_eq!(state.read_committed_storage(&OWNER, &KEY).unwrap(), Word::ZERO);

    assert!(!state.storage(&OTHER).is_wiped());
    assert!(!state.storage(&OTHER).is_loaded(&KEY));
    assert_eq!(state.storage[&OTHER].slots.capacity(), capacity);
    assert_eq!(state.storage_slot(&OTHER, KEY, false).unwrap().current(), Word::from(3));
    state.clear_transaction_state();

    // Recreating storage after the committed wipe starts from accepted zero, not the old row.
    let mut slot = state.storage_slot(&OWNER, KEY, false).unwrap();
    assert_eq!(slot.write(Word::from(11)), (Word::ZERO, Word::ZERO));
    state.commit_transaction();
    state.clear_transaction_state();
    assert_eq!(state.read_committed_storage(&OWNER, &KEY).unwrap(), Word::from(11));
    assert_pool_bound(&state.storage_pool);
}

#[test]
fn delete_then_recreate_does_not_inherit_old_storage() {
    let mut state = State::new(database(3));
    state.storage_slot(&OWNER, KEY, false).unwrap().write(Word::from(7));
    state.storage(&OWNER).wipe();
    state.account(&OWNER, false).unwrap().delete_for_finalization();
    state.commit_transaction();
    state.clear_transaction_state();
    assert_eq!(state.account_info_untracked(&OWNER).unwrap(), None);

    state.account(&OWNER, false).unwrap().set_balance(Word::from(1));
    let mut slot = state.storage_slot(&OWNER, KEY, false).unwrap();
    assert!(!slot.is_warm());
    assert_eq!(slot.write(Word::from(11)), (Word::ZERO, Word::ZERO));
    state.commit_transaction();
    state.clear_transaction_state();
    assert!(state.account_info_untracked(&OWNER).unwrap().is_some());
    assert_eq!(state.read_committed_storage(&OWNER, &KEY).unwrap(), Word::from(11));
    assert_pool_bound(&state.storage_pool);
}

#[test]
fn failed_cold_read_does_not_leave_a_loaded_or_warm_slot() {
    struct FailingDatabase(CacheDB);

    impl DynDatabase for FailingDatabase {
        fn get_account(&mut self, address: &Address) -> DbResult<Option<AccountInfo>> {
            self.0.get_account(address)
        }
        fn get_code_by_hash(&mut self, hash: &B256) -> DbResult<Bytecode> {
            self.0.get_code_by_hash(hash)
        }
        fn get_storage(&mut self, address: &Address, key: &Word) -> DbResult<Word> {
            if key.is_zero() {
                Err(ErrorCode::STORED_ERROR)
            } else {
                self.0.get_storage(address, key)
            }
        }
        fn get_block_hash(&mut self, number: &Word) -> DbResult<B256> {
            self.0.get_block_hash(number)
        }
    }

    let mut state = State::new(FailingDatabase(database(3)));
    state.storage_slot(&OWNER, KEY, false).unwrap().warm();
    state.clear_transaction_state();
    let capacity = state.storage_pool.capacity;
    assert_eq!(state.storage_slot(&OWNER, Word::ZERO, false).unwrap_err(), ErrorCode::STORED_ERROR);
    assert!(!state.storage(&OWNER).is_loaded(&Word::ZERO));
    assert!(!state.storage(&OWNER).is_warm(&Word::ZERO));
    state.clear_transaction_state();
    assert_eq!(state.storage_pool.capacity, capacity);
    let slot = state.storage_slot(&OWNER, KEY, false).unwrap();
    assert_eq!(slot.current(), Word::from(3));
    assert!(!slot.is_warm());
}

#[test]
fn rollback_keeps_live_slots_and_does_not_recycle() {
    let mut state = State::new(database(3));
    state.storage_slot(&OWNER, KEY, false).unwrap();
    state.clear_transaction_state();
    assert_eq!(state.storage_pool.maps.len(), 1);
    let checkpoint = state.checkpoint();
    {
        let mut slot = state.storage_slot(&OWNER, KEY, false).unwrap();
        assert!(slot.warm());
        slot.write(Word::from(7));
    }
    let capacity = state.storage[&OWNER].slots.capacity();
    state.rollback(checkpoint, Version::base(SpecId::OSAKA).features);
    assert_eq!(state.storage_pool.capacity, 0);
    assert_eq!(state.storage[&OWNER].slots.capacity(), capacity);
    let slot = state.storage_slot(&OWNER, KEY, false).unwrap();
    assert_eq!((slot.original(), slot.current()), (Word::from(3), Word::from(3)));
    assert!(!slot.is_warm());
    state.clear_transaction_state();
    assert_eq!(state.storage_pool.capacity, capacity);
}

#[test]
fn detached_maps_remain_owned_until_reattached_and_resolved() {
    let mut state = State::new(database(3));
    state.storage_slot(&OWNER, KEY, false).unwrap().write(Word::from(7));
    let pending = state.take_pending_state();
    let expected = pending.clone();
    state.clear_transaction_state();
    assert_eq!(state.storage_pool.capacity, 0);

    state.storage_slot(&OTHER, KEY, false).unwrap().write(Word::from(9));
    state.commit_transaction();
    state.clear_transaction_state();
    assert_eq!(pending, expected);
    assert_eq!(state.read_committed_storage(&OWNER, &KEY).unwrap(), Word::from(3));
    assert_eq!(state.read_committed_storage(&OTHER, &KEY).unwrap(), Word::from(9));

    state.set_pending_state(pending);
    state.commit_transaction();
    state.clear_transaction_state();
    assert_eq!(state.read_committed_storage(&OWNER, &KEY).unwrap(), Word::from(7));
    assert_eq!(state.read_committed_storage(&OTHER, &KEY).unwrap(), Word::from(9));
    assert_pool_bound(&state.storage_pool);
}

#[test]
fn replacing_database_keeps_only_empty_allocations() {
    let mut state = State::new(database(3));
    state.prewarm_storage_slot(&OWNER, KEY);
    state.storage_slot(&OWNER, KEY, false).unwrap().write(Word::from(7));
    state.storage(&OWNER).wipe();
    state.set_initial(database(19));
    assert!(state.storage.is_empty());
    assert!(state.journal().is_empty());
    assert_eq!(state.storage_pool.maps.len(), 1);
    assert!(!state.storage(&OWNER).is_wiped());
    let slot = state.storage_slot(&OWNER, KEY, false).unwrap();
    assert_eq!((slot.original(), slot.current()), (Word::from(19), Word::from(19)));
    assert!(!slot.is_warm());
}

#[test]
fn pooled_and_unpooled_transactions_build_identical_bal_and_cache() {
    let mut pooled = State::new(database(3));
    let mut unpooled = State::new(database(3));
    for state in [&mut pooled, &mut unpooled] {
        state.enable_bal_builder();
    }
    for (i, (address, value)) in [(OWNER, 7), (OTHER, 3), (OWNER, 9)].into_iter().enumerate() {
        for state in [&mut pooled, &mut unpooled] {
            state.bump_bal_index();
            state.account(&address, false).unwrap();
            let mut slot = state.storage_slot(&address, KEY, false).unwrap();
            assert!(!slot.is_warm());
            assert_eq!(slot.original(), Word::from(if i == 2 { 7 } else { 3 }));
            slot.warm();
            slot.write(Word::from(value));
            state.commit_transaction();
            state.clear_transaction_state();
        }
        // A reference that intentionally drops scratch allocations at every boundary.
        unpooled.storage_pool = StoragePool::default();
        assert_eq!(pooled.overlay_db().cache, unpooled.overlay_db().cache);
    }
    assert_eq!(pooled.take_bal_builder(), unpooled.take_bal_builder());
    assert_pool_bound(&pooled.storage_pool);
}

fn overlay_with_capacity(requested: usize) -> StorageOverlay {
    let mut slots = U256Map::default();
    slots.reserve(requested);
    slots.insert(
        KEY,
        StorageSlot { value: Tracked::new(Word::from(3)), is_warm: true, _non_exhaustive: () },
    );
    StorageOverlay { slots, wiped: true, _non_exhaustive: () }
}

#[test]
fn empty_and_oversized_maps_are_not_retained() {
    let mut pool = StoragePool::default();
    let mut storage = AddressMap::default();
    storage.insert(OWNER, StorageOverlay::default());
    storage.insert(OTHER, overlay_with_capacity(MAX_MAP_CAPACITY + 1));
    pool.clear(&mut storage);
    assert!(storage.is_empty());
    assert_eq!(pool.capacity, 0);
    assert_pool_bound(&pool);

    storage.insert(OWNER, overlay_with_capacity(1));
    pool.clear(&mut storage);
    assert_eq!(pool.maps.len(), 1);
    assert_pool_bound(&pool);
    assert!(pool.take().is_empty());
    assert_eq!(pool.capacity, 0);
    assert_eq!(pool.take().capacity(), 0);
}

#[test]
fn map_count_is_bounded_across_many_owners() {
    let mut pool = StoragePool::default();
    let mut storage = AddressMap::default();
    for i in 0..MAX_MAPS + 8 {
        storage.insert(Address::with_last_byte(i as u8), overlay_with_capacity(1));
    }
    let outer_capacity = storage.capacity();
    pool.clear(&mut storage);
    assert_eq!(pool.maps.len(), MAX_MAPS);
    assert_eq!(storage.capacity(), outer_capacity);
    assert_pool_bound(&pool);
    pool.clear(&mut storage);
    assert_eq!(pool.maps.len(), MAX_MAPS);
    for _ in 0..MAX_MAPS {
        assert!(pool.take().is_empty());
        assert_pool_bound(&pool);
    }
    assert_eq!(pool.capacity, 0);
}

#[test]
fn total_capacity_is_bounded_and_checkout_is_recharged() {
    let mut pool = StoragePool::default();
    let mut storage = AddressMap::default();
    let capacity = overlay_with_capacity(1_024).slots.capacity();
    assert!(capacity <= MAX_MAP_CAPACITY);
    assert!(capacity * MAX_MAPS > MAX_TOTAL_CAPACITY);
    for i in 0..MAX_MAPS {
        storage.insert(Address::with_last_byte(i as u8), overlay_with_capacity(1_024));
    }
    pool.clear(&mut storage);
    assert_pool_bound(&pool);
    assert_eq!(pool.maps.len(), MAX_TOTAL_CAPACITY / capacity);
    let retained = pool.capacity;
    let mut checked_out = pool.take();
    assert_eq!(pool.capacity, retained - capacity);
    // Growth while active must be charged at its new capacity, never its old checkout charge.
    checked_out.reserve(MAX_MAP_CAPACITY + 1);
    storage.insert(OWNER, StorageOverlay { slots: checked_out, ..StorageOverlay::default() });
    pool.clear(&mut storage);
    assert_eq!(pool.capacity, retained - capacity);
    assert_pool_bound(&pool);
}

#[test]
fn recycled_capacity_is_measured_after_deletion_markers_are_cleared() {
    let mut slots = U256Map::default();
    slots.reserve(1_024);
    let allocated_capacity = slots.capacity();
    for key in 0..allocated_capacity {
        slots.insert(Word::from(key), StorageSlot::default());
    }
    slots.retain(|key, _| *key == KEY);
    assert!(slots.capacity() <= allocated_capacity);

    let mut pool = StoragePool::default();
    let mut storage = AddressMap::default();
    storage.insert(OWNER, StorageOverlay { slots, ..StorageOverlay::default() });
    pool.clear(&mut storage);
    assert_eq!(pool.capacity, allocated_capacity);
    assert_pool_bound(&pool);
    assert_eq!(pool.take().capacity(), allocated_capacity);
    assert_eq!(pool.capacity, 0);
}
