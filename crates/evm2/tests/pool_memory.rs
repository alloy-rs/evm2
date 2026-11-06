//! Frames must not keep the memory of every transaction that used them.
//!
//! This test counts heap allocations through a global allocator, so it lives in its own test
//! binary.

use alloy_consensus::{TxLegacy, transaction::Recovered};
use alloy_primitives::{Address, TxKind, hex};
use evm2::{
    BaseEvmTypes, Evm, Precompiles, SpecId,
    bytecode::Bytecode,
    env::BlockEnvExt,
    ethereum::{TxEnvelope, ethereum_tx_registry},
    evm::{AccountInfo, InMemoryDB},
    interpreter::Word,
};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::atomic::{AtomicUsize, Ordering::Relaxed},
};

static LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);

struct CountingAlloc;

// SAFETY: Delegates to `System`, only counting the live bytes.
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        LIVE_BYTES.fetch_add(layout.size(), Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE_BYTES.fetch_sub(layout.size(), Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        LIVE_BYTES.fetch_add(layout.size(), Relaxed);
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        LIVE_BYTES.fetch_add(new_size, Relaxed);
        LIVE_BYTES.fetch_sub(layout.size(), Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

#[test]
fn transactions_at_different_depths_do_not_accumulate_memory() {
    const CALLER: Address = Address::repeat_byte(0xaa);
    const TARGET: Address = Address::repeat_byte(0x44);
    // Calls itself with `calldata - 1` until the calldata is zero, then touches memory at 2 MB,
    // which costs about 8M gas and about 2 MB of memory in the innermost frame.
    let code = hex!("6000358015601d576001900360005260006000602060006000305af1005b6001621e84805300");

    let mut db = InMemoryDB::default();
    db.insert_account_info(&CALLER, AccountInfo::default().with_balance(Word::from(1u64 << 60)));
    db.insert_account_info(
        &TARGET,
        AccountInfo::default().with_code(Bytecode::new_legacy(code.to_vec().into())),
    );
    let mut evm = Evm::<BaseEvmTypes>::new(
        SpecId::CANCUN,
        BlockEnvExt::default(),
        ethereum_tx_registry(SpecId::CANCUN),
        db,
        Precompiles::base(SpecId::CANCUN),
    );

    // Every transaction reaches a different depth, so without a bound each one leaves 2 MB behind
    // in a different frame.
    const TXS: u64 = 40;
    for depth in 0..TXS {
        let tx = Recovered::new_unchecked(
            TxEnvelope::Legacy(TxLegacy {
                to: TxKind::Call(TARGET),
                gas_limit: 1_000_000_000,
                nonce: depth,
                input: Word::from(depth).to_be_bytes::<32>().to_vec().into(),
                ..TxLegacy::default()
            }),
            CALLER,
        );
        assert!(evm.transact(&tx).unwrap().commit().is_success());
    }

    let live = LIVE_BYTES.load(Relaxed);
    assert!(live < 16 << 20, "the EVM holds {live} bytes after {TXS} transactions");
}
