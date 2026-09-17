//! Repeated transaction storage access on one State, with fixture construction excluded.
//!
//! Copy this benchmark and its Cargo target/dependency to the parent revision to compare
//! allocation reuse against the original cleanup path. Both revisions run the same public API;
//! neither recreates State nor clears the accepted cache between transactions.

#![allow(missing_docs)] // Criterion generates public harness entry points.

use alloy_primitives::{Address, U256};
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use evm2::evm::{CacheDB, State};
use std::{hint::black_box, time::Duration};

#[derive(Clone, Copy)]
enum Resolution {
    Commit,
    Discard,
}

impl Resolution {
    const fn name(self) -> &'static str {
        match self {
            Self::Commit => "commit",
            Self::Discard => "discard",
        }
    }
}

fn fixture(owners: usize, slots: usize) -> (State<'static>, Vec<Address>, Vec<U256>) {
    let owners: Vec<_> = (0..owners).map(|i| Address::with_last_byte(i as u8)).collect();
    let slots: Vec<_> = (0..slots).map(U256::from).collect();
    let mut database = CacheDB::default();
    for owner in &owners {
        for key in &slots {
            database.insert_account_storage(owner, key, &U256::from(3));
        }
    }
    (State::new(database), owners, slots)
}

fn transaction(
    state: &mut State<'_>,
    owners: &[Address],
    slots: &[U256],
    value: U256,
    resolution: Resolution,
) {
    for owner in owners {
        for &key in slots {
            let mut slot = state.storage_slot(owner, key, false).unwrap();
            black_box(slot.warm());
            black_box(slot.write(value));
        }
    }
    if matches!(resolution, Resolution::Commit) {
        state.commit_transaction();
    }
    state.clear_transaction_state();
}

fn storage_reuse(c: &mut Criterion) {
    let mut group = c.benchmark_group("transaction_storage_reuse");
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(3));
    group.sample_size(30);

    // Several small native-style owners, larger overlays, and a single oversized map which
    // must be dropped rather than retained. Every row is touched again in the next transaction.
    for (owners, slots) in [(4, 8), (16, 32), (1, 8_192)] {
        group.throughput(Throughput::Elements((owners * slots) as u64));
        for resolution in [Resolution::Commit, Resolution::Discard] {
            let (mut state, addresses, keys) = fixture(owners, slots);
            let mut round = 0u64;
            // Populate the accepted read cache and transaction scratch before measurement.
            for _ in 0..8 {
                round += 1;
                transaction(&mut state, &addresses, &keys, U256::from(1 + round % 2), resolution);
            }
            group.bench_function(
                BenchmarkId::new(resolution.name(), format!("{owners}x{slots}")),
                |b| {
                    b.iter(|| {
                        round = round.wrapping_add(1);
                        transaction(
                            &mut state,
                            black_box(&addresses),
                            black_box(&keys),
                            U256::from(1 + round % 2),
                            resolution,
                        );
                    });
                },
            );
            let expected = match resolution {
                Resolution::Commit => U256::from(1 + round % 2),
                Resolution::Discard => U256::from(3),
            };
            assert_eq!(state.read_committed_storage(&addresses[0], &keys[0]).unwrap(), expected);
            assert_eq!(state.get_storage(&addresses[0], &keys[0]), None);
        }
    }
    group.finish();
}

criterion_group!(benches, storage_reuse);
criterion_main!(benches);
