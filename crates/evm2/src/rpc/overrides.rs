use crate::{
    DatabaseError,
    bytecode::{Bytecode, BytecodeDecodeError},
    env::BlockEnvExt,
    evm::{AccountChangeRef, CacheDB, DynDatabase, StateChangeSink, StorageChange},
};
use alloc::vec::Vec;
use alloy_primitives::{Address, U256};
use alloy_rpc_types_eth::{BlockOverrides, state::StateOverride};
use core::convert::Infallible;

/// Errors that can occur while applying state overrides.
#[derive(Debug, thiserror::Error)]
pub enum StateOverrideError {
    /// Invalid bytecode provided in an override.
    #[error(transparent)]
    InvalidBytecode(#[from] BytecodeDecodeError),
    /// Both state and state diff were provided for an account.
    #[error("Both 'state' and 'stateDiff' fields are set for account {0}")]
    BothStateAndStateDiff(Address),
    /// Database error.
    #[error(transparent)]
    Database(DatabaseError),
}

/// Applies RPC block overrides to a block environment and the block hashes of `db`.
///
/// Out-of-range `baseFeePerGas` and `blobBaseFee` values saturate to `u64` and `u128`.
pub fn apply_block_overrides<DB, E>(
    overrides: BlockOverrides,
    db: &mut CacheDB<DB>,
    block_env: &mut BlockEnvExt<E>,
) {
    let BlockOverrides {
        number,
        difficulty,
        time,
        gas_limit,
        coinbase,
        random,
        base_fee,
        blob_base_fee,
        block_hash,
        ..
    } = overrides;

    if let Some(block_hashes) = block_hash {
        for (number, hash) in block_hashes {
            db.insert_block_hash(&U256::from(number), &hash);
        }
    }

    if let Some(number) = number {
        block_env.number = number;
    }
    if let Some(difficulty) = difficulty {
        block_env.difficulty = difficulty;
    }
    if let Some(time) = time {
        block_env.timestamp = U256::from(time);
    }
    if let Some(gas_limit) = gas_limit {
        block_env.gas_limit = U256::from(gas_limit);
    }
    if let Some(coinbase) = coinbase {
        block_env.beneficiary = coinbase;
    }
    if let Some(random) = random {
        block_env.prevrandao = U256::from_be_slice(random.as_slice());
    }
    if let Some(base_fee) = base_fee {
        block_env.basefee = U256::from(base_fee.saturating_to::<u64>());
    }
    if let Some(blob_base_fee) = blob_base_fee {
        block_env.blob_basefee = U256::from(blob_base_fee.saturating_to::<u128>());
    }
}

/// Applies RPC state overrides to a state cache and streams each change to `sink`.
///
/// An override of `state` replaces the account's storage, `stateDiff` updates individual slots.
/// Changes reach `sink` in the order of [`StateChangeSink`]: bytecode, storage wipe, changed slots
/// with their values before the override, then the account. `movePrecompileToAddress` is not
/// supported. Pass [`NoopChangeSink`](crate::evm::NoopChangeSink) to discard the changes.
///
/// An attached block access list takes precedence over the cache when reading, so detach it first.
pub fn apply_state_overrides<DB: DynDatabase>(
    overrides: StateOverride,
    db: &mut CacheDB<DB>,
    sink: &mut impl StateChangeSink<Error = Infallible>,
) -> Result<(), StateOverrideError> {
    // Edit the cache directly: committing through its sink would also feed the BAL builder.
    for (address, account_override) in overrides {
        let original = db.get_account(&address).map_err(StateOverrideError::Database)?;
        let mut account = original.clone().unwrap_or_default();

        if let Some(nonce) = account_override.nonce {
            account.nonce = nonce;
        }
        if let Some(code) = account_override.code {
            account.set_code(Bytecode::new_raw_checked(code)?);
        }
        if let Some(balance) = account_override.balance {
            account.balance = balance;
        }

        let replaces_storage = account_override.state.is_some();
        let storage = match (account_override.state, account_override.state_diff) {
            (Some(_), Some(_)) => return Err(StateOverrideError::BothStateAndStateDiff(address)),
            (state, state_diff) => state.or(state_diff).unwrap_or_default(),
        };
        // Read the values before the override, ahead of any storage wipe.
        let changes = storage
            .into_iter()
            .map(|(key, value)| {
                let key = U256::from_be_bytes(key.0);
                let current = U256::from_be_bytes(value.0);
                let original =
                    db.get_storage(&address, &key).map_err(StateOverrideError::Database)?;
                Ok(StorageChange { address, key, original, current })
            })
            .collect::<Result<Vec<_>, StateOverrideError>>()?;

        // Account existence affects authorization refunds and new-account gas charges. The
        // override touches the account, so under EIP-161 an empty result is removed regardless of
        // whether the account existed before, unless storage materializes it. A full storage
        // replacement always materializes the account, even with no slots.
        if !account.is_empty() || replaces_storage || !changes.is_empty() {
            if let Some(code) = &account.code
                && !code.is_empty()
            {
                let Ok(()) = sink.bytecode(account.code_hash, code);
            }
            if replaces_storage {
                let Ok(()) = sink.storage_wipe(address);
                db.cache.storage.entry(address).or_default().wipe();
            }
            for change in &changes {
                // A wipe zeroes every slot, and an unchanged slot is not a write.
                if (replaces_storage && !change.current.is_zero())
                    || (!replaces_storage && change.original != change.current)
                {
                    let Ok(()) = sink.storage(*change);
                }
                db.insert_account_storage(&address, &change.key, &change.current);
            }
            if original.as_ref() == Some(&account) {
                let Ok(()) = sink.account_read(address, original.as_ref());
            } else {
                let Ok(()) = sink.account(AccountChangeRef {
                    address,
                    original: original.as_ref(),
                    current: Some(&account),
                    created: original.is_none(),
                    selfdestructed: false,
                });
            }
            db.insert_account_info(&address, account);
        } else if original.is_some() {
            let Ok(()) = sink.storage_wipe(address);
            let Ok(()) = sink.account(AccountChangeRef {
                address,
                original: original.as_ref(),
                current: None,
                created: false,
                selfdestructed: false,
            });
            db.cache.accounts.insert(address, None);
            db.cache.storage.entry(address).or_default().wipe();
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evm::{AccountInfo, InMemoryDB, NoopChangeSink};
    use alloy_primitives::{B256, Bytes, address, bytes, keccak256, map::B256Map};
    use alloy_rpc_types_eth::state::AccountOverride;

    const CODE: Bytes = bytes!(
        "0x63d0e30db05f525f5f6004601c3473c02aaa39b223fe8d0a0e5c4f27ead9083c756cc25af15f5260205ff3"
    );

    #[derive(Debug, PartialEq, Eq)]
    enum Event {
        Bytecode(B256),
        StorageWipe(Address),
        Storage(StorageChange),
        AccountRead(Address),
        Account {
            address: Address,
            original: Option<(U256, u64)>,
            current: Option<(U256, u64, B256)>,
            created: bool,
            selfdestructed: bool,
        },
    }

    #[derive(Default)]
    struct Recorder(Vec<Event>);

    impl StateChangeSink for Recorder {
        type Error = Infallible;

        fn bytecode(&mut self, code_hash: B256, _code: &Bytecode) -> Result<(), Self::Error> {
            self.0.push(Event::Bytecode(code_hash));
            Ok(())
        }

        fn storage_wipe(&mut self, address: Address) -> Result<(), Self::Error> {
            self.0.push(Event::StorageWipe(address));
            Ok(())
        }

        fn storage(&mut self, change: StorageChange) -> Result<(), Self::Error> {
            self.0.push(Event::Storage(change));
            Ok(())
        }

        fn account(&mut self, change: AccountChangeRef<'_>) -> Result<(), Self::Error> {
            self.0.push(Event::Account {
                address: change.address,
                original: change.original.map(|info| (info.balance, info.nonce)),
                current: change.current.map(|info| (info.balance, info.nonce, info.code_hash)),
                created: change.created,
                selfdestructed: change.selfdestructed,
            });
            Ok(())
        }

        fn account_read(
            &mut self,
            address: Address,
            _info: Option<&AccountInfo>,
        ) -> Result<(), Self::Error> {
            self.0.push(Event::AccountRead(address));
            Ok(())
        }
    }

    fn slots(slots: impl IntoIterator<Item = (u64, u64)>) -> B256Map<B256> {
        slots
            .into_iter()
            .map(|(key, value)| (B256::from(U256::from(key)), B256::from(U256::from(value))))
            .collect()
    }

    fn apply(
        db: &mut InMemoryDB,
        address: Address,
        account_override: AccountOverride,
    ) -> Result<(), StateOverrideError> {
        apply_state_overrides(
            StateOverride::from_iter([(address, account_override)]),
            db,
            &mut NoopChangeSink::default(),
        )
    }

    #[test]
    fn block_overrides_apply_set_fields_only() {
        let mut db = InMemoryDB::default();
        let mut env = BlockEnvExt::<()>::default();
        let before = env;
        let coinbase = address!("0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599");
        let random = B256::repeat_byte(7);

        let overrides = BlockOverrides::default()
            .with_number(U256::from(10))
            .with_difficulty(U256::from(11))
            .with_time(12)
            .with_gas_limit(13)
            .with_coinbase(coinbase)
            .with_random(random)
            .with_base_fee(U256::from(14))
            .with_blob_base_fee(U256::from(15))
            .with_block_hash_overrides([(9, B256::repeat_byte(1)), (8, B256::repeat_byte(2))]);
        apply_block_overrides(overrides, &mut db, &mut env);

        assert_eq!(env.number, U256::from(10));
        assert_eq!(env.difficulty, U256::from(11));
        assert_eq!(env.timestamp, U256::from(12));
        assert_eq!(env.gas_limit, U256::from(13));
        assert_eq!(env.beneficiary, coinbase);
        assert_eq!(env.prevrandao, U256::from_be_bytes(random.0));
        assert_eq!(env.basefee, U256::from(14));
        assert_eq!(env.blob_basefee, U256::from(15));
        assert_eq!(env.slot_num, before.slot_num);
        assert_eq!(db.cache.block_hashes[&U256::from(9)], B256::repeat_byte(1));
        assert_eq!(db.cache.block_hashes[&U256::from(8)], B256::repeat_byte(2));

        let mut env = before;
        apply_block_overrides(BlockOverrides::default(), &mut db, &mut env);
        assert_eq!(env, before);
    }

    #[test]
    fn block_overrides_saturate_fees() {
        let mut db = InMemoryDB::default();
        let mut env = BlockEnvExt::<()>::default();
        let overrides =
            BlockOverrides::default().with_base_fee(U256::MAX).with_blob_base_fee(U256::MAX);
        apply_block_overrides(overrides, &mut db, &mut env);
        assert_eq!(env.basefee, U256::from(u64::MAX));
        assert_eq!(env.blob_basefee, U256::from(u128::MAX));
    }

    #[test]
    fn code_override_sets_code_and_hash() {
        let to = address!("0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599");
        let mut db = InMemoryDB::default();
        let mut sink = Recorder::default();

        let overrides =
            StateOverride::from_iter([(to, AccountOverride::default().with_code(CODE))]);
        apply_state_overrides(overrides, &mut db, &mut sink).unwrap();

        let hash = keccak256(&CODE);
        let account = db.get_account(&to).unwrap().unwrap();
        assert_eq!(account.code_hash, hash);
        assert_eq!(db.get_code_by_hash(&hash).unwrap().original_bytes(), CODE);
        assert_eq!(
            sink.0,
            [
                Event::Bytecode(hash),
                Event::Account {
                    address: to,
                    original: None,
                    current: Some((U256::ZERO, 0, hash)),
                    created: true,
                    selfdestructed: false,
                },
            ]
        );
    }

    #[test]
    fn balance_and_nonce_overrides_keep_existing_code_and_storage() {
        let address = Address::with_last_byte(1);
        let mut db = InMemoryDB::default();
        db.insert_account_info(
            &address,
            AccountInfo::default()
                .with_balance(U256::from(5))
                .with_code(Bytecode::new_legacy(CODE)),
        );
        db.insert_account_storage(&address, &U256::ONE, &U256::from(9));

        let account_override = AccountOverride::default().with_balance(U256::from(7)).with_nonce(3);
        let mut sink = Recorder::default();
        let overrides = StateOverride::from_iter([(address, account_override)]);
        apply_state_overrides(overrides, &mut db, &mut sink).unwrap();

        let account = db.get_account(&address).unwrap().unwrap();
        assert_eq!((account.balance, account.nonce), (U256::from(7), 3));
        assert_eq!(account.code_hash, keccak256(&CODE));
        assert_eq!(db.get_storage(&address, &U256::ONE).unwrap(), U256::from(9));
        assert_eq!(
            sink.0,
            [Event::Account {
                address,
                original: Some((U256::from(5), 0)),
                current: Some((U256::from(7), 3, keccak256(&CODE))),
                created: false,
                selfdestructed: false,
            }]
        );
    }

    #[test]
    fn state_diff_updates_slots_and_keeps_others() {
        let address = Address::with_last_byte(1);
        let mut db = InMemoryDB::default();
        db.insert_account_info(&address, AccountInfo::default().with_nonce(1));
        db.insert_account_storage(&address, &U256::ONE, &U256::from(9));
        db.insert_account_storage(&address, &U256::from(3), &U256::from(30));

        let account_override =
            AccountOverride::default().with_state_diff(slots([(1, 100), (2, 200)]));
        let mut sink = Recorder::default();
        let overrides = StateOverride::from_iter([(address, account_override)]);
        apply_state_overrides(overrides, &mut db, &mut sink).unwrap();

        assert_eq!(db.get_storage(&address, &U256::ONE).unwrap(), U256::from(100));
        assert_eq!(db.get_storage(&address, &U256::from(2)).unwrap(), U256::from(200));
        assert_eq!(db.get_storage(&address, &U256::from(3)).unwrap(), U256::from(30));
        assert!(!sink.0.iter().any(|event| matches!(event, Event::StorageWipe(_))));
        let changes = sink
            .0
            .iter()
            .filter_map(|event| match event {
                Event::Storage(change) => Some((change.key, change.original, change.current)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(changes.len(), 2);
        assert!(changes.contains(&(U256::ONE, U256::from(9), U256::from(100))));
        assert!(changes.contains(&(U256::from(2), U256::ZERO, U256::from(200))));
    }

    #[test]
    fn state_diff_on_missing_account_creates_it() {
        let address = Address::with_last_byte(1);
        let mut db = InMemoryDB::default();

        let account_override =
            AccountOverride::default().with_state_diff(slots([(1, 100), (2, 200)]));
        apply(&mut db, address, account_override).unwrap();

        let account = db.get_account(&address).unwrap().unwrap();
        assert!(account.is_empty());
        assert_eq!(db.get_storage(&address, &U256::ONE).unwrap(), U256::from(100));
        assert_eq!(db.get_storage(&address, &U256::from(2)).unwrap(), U256::from(200));
    }

    #[test]
    fn state_replaces_storage() {
        let address = Address::with_last_byte(1);
        let other = Address::with_last_byte(2);
        let mut db = InMemoryDB::default();
        for address in [address, other] {
            db.insert_account_info(&address, AccountInfo::default().with_nonce(1));
            db.insert_account_storage(&address, &U256::ONE, &U256::from(9));
            db.insert_account_storage(&address, &U256::from(3), &U256::from(30));
        }

        let account_override = AccountOverride::default().with_state(slots([(1, 100), (2, 200)]));
        let mut sink = Recorder::default();
        let overrides = StateOverride::from_iter([(address, account_override)]);
        apply_state_overrides(overrides, &mut db, &mut sink).unwrap();

        assert_eq!(db.get_storage(&address, &U256::ONE).unwrap(), U256::from(100));
        assert_eq!(db.get_storage(&address, &U256::from(2)).unwrap(), U256::from(200));
        assert_eq!(db.get_storage(&address, &U256::from(3)).unwrap(), U256::ZERO);
        assert_eq!(db.get_storage(&other, &U256::from(3)).unwrap(), U256::from(30));
        // The sink sees the wipe first, then only the nonzero slots with their prior values, and
        // the account last. Zero-valued slots are not writes after a wipe.
        assert_eq!(sink.0.len(), 4);
        assert_eq!(sink.0[0], Event::StorageWipe(address));
        let storage = |key, original, current| {
            Event::Storage(StorageChange {
                address,
                key: U256::from(key),
                original: U256::from(original),
                current: U256::from(current),
            })
        };
        assert!(sink.0[1..3].contains(&storage(1, 9, 100)));
        assert!(sink.0[1..3].contains(&storage(2, 0, 200)));
        assert_eq!(sink.0[3], Event::AccountRead(address));
    }

    #[test]
    fn sink_skips_unchanged_and_zeroed_writes() {
        let address = Address::with_last_byte(1);
        let mut db = InMemoryDB::default();
        db.insert_account_info(&address, AccountInfo::default().with_nonce(1));
        db.insert_account_storage(&address, &U256::ONE, &U256::from(9));

        // Rewriting a slot to its current value changes nothing.
        let account_override = AccountOverride::default().with_state_diff(slots([(1, 9)]));
        let mut sink = Recorder::default();
        let overrides = StateOverride::from_iter([(address, account_override)]);
        apply_state_overrides(overrides, &mut db, &mut sink).unwrap();
        assert_eq!(sink.0, [Event::AccountRead(address)]);

        // Zero-valued slots of a replacement are not writes.
        let account_override = AccountOverride::default().with_state(slots([(1, 0), (2, 0)]));
        let mut sink = Recorder::default();
        let overrides = StateOverride::from_iter([(address, account_override)]);
        apply_state_overrides(overrides, &mut db, &mut sink).unwrap();
        assert!(!sink.0.iter().any(|event| matches!(event, Event::Storage(_))));
        assert_eq!(db.get_storage(&address, &U256::ONE).unwrap(), U256::ZERO);
    }

    #[test]
    fn overrides_shadow_the_backing_database() {
        let kept = Address::with_last_byte(1);
        let replaced = Address::with_last_byte(2);
        let emptied = Address::with_last_byte(3);
        let mut backing = InMemoryDB::default();
        for address in [kept, replaced] {
            backing.insert_account_info(&address, AccountInfo::default().with_nonce(1));
            backing.insert_account_storage(&address, &U256::ONE, &U256::from(10));
            backing.insert_account_storage(&address, &U256::from(2), &U256::from(20));
        }
        backing.insert_account_info(&emptied, AccountInfo::default().with_balance(U256::ONE));
        backing.insert_account_storage(&emptied, &U256::ONE, &U256::from(10));
        let mut db = CacheDB::new(backing);

        let overrides = StateOverride::from_iter([
            (kept, AccountOverride::default().with_state_diff(slots([(1, 11)]))),
            (replaced, AccountOverride::default().with_state(slots([(1, 11)]))),
            (emptied, AccountOverride::default().with_balance(U256::ZERO)),
        ]);
        apply_state_overrides(overrides, &mut db, &mut NoopChangeSink::default()).unwrap();

        // A diff leaves other slots to the backing database, a replacement hides them.
        assert_eq!(db.get_storage(&kept, &U256::ONE).unwrap(), U256::from(11));
        assert_eq!(db.get_storage(&kept, &U256::from(2)).unwrap(), U256::from(20));
        assert_eq!(db.get_storage(&replaced, &U256::ONE).unwrap(), U256::from(11));
        assert_eq!(db.get_storage(&replaced, &U256::from(2)).unwrap(), U256::ZERO);
        // An emptied account that only the backing database holds is removed with its storage.
        assert_eq!(db.get_account(&emptied).unwrap(), None);
        assert_eq!(db.get_storage(&emptied, &U256::ONE).unwrap(), U256::ZERO);
    }

    #[test]
    fn state_and_state_diff_conflict() {
        let address = Address::with_last_byte(1);
        let mut db = InMemoryDB::default();

        let account_override =
            AccountOverride::default().with_state(slots([(1, 1)])).with_state_diff(slots([(1, 2)]));
        let err = apply(&mut db, address, account_override).unwrap_err();

        assert!(matches!(err, StateOverrideError::BothStateAndStateDiff(a) if a == address));
        assert_eq!(db.get_account(&address).unwrap(), None);
    }

    #[test]
    fn invalid_bytecode_is_rejected() {
        let address = Address::with_last_byte(1);
        let mut db = InMemoryDB::default();

        let code = bytes!("0xef0100");
        let err = apply(&mut db, address, AccountOverride::default().with_code(code)).unwrap_err();

        assert!(matches!(err, StateOverrideError::InvalidBytecode(_)));
    }

    #[test]
    fn state_overrides_remove_accounts_left_empty() {
        let emptied = Address::with_last_byte(1);
        let absent = Address::with_last_byte(2);
        let replaced = Address::with_last_byte(3);
        let slot = U256::ONE;

        let mut db = InMemoryDB::default();
        for address in [emptied, replaced] {
            db.insert_account_info(&address, AccountInfo::default().with_balance(U256::from(5)));
            db.insert_account_storage(&address, &slot, &U256::from(9));
        }

        let overrides = StateOverride::from_iter([
            (emptied, AccountOverride::default().with_balance(U256::ZERO)),
            (absent, AccountOverride::default()),
            (
                replaced,
                AccountOverride::default().with_balance(U256::ZERO).with_state(B256Map::default()),
            ),
        ]);
        let mut sink = Recorder::default();
        apply_state_overrides(overrides, &mut db, &mut sink).unwrap();

        // An existing account touched into emptiness is removed under EIP-161, with its storage.
        assert_eq!(db.get_account(&emptied).unwrap(), None);
        assert_eq!(db.get_storage(&emptied, &slot).unwrap(), U256::ZERO);
        // An absent account stays absent.
        assert_eq!(db.get_account(&absent).unwrap(), None);
        // A full storage replacement materializes the account even when it is left empty.
        assert!(db.get_account(&replaced).unwrap().is_some_and(|account| account.is_empty()));
        assert_eq!(db.get_storage(&replaced, &slot).unwrap(), U256::ZERO);

        // Accounts are applied in map order, but each removal is a wipe followed by the deletion.
        let wipe = sink.0.iter().position(|event| *event == Event::StorageWipe(emptied)).unwrap();
        assert_eq!(
            sink.0[wipe + 1],
            Event::Account {
                address: emptied,
                original: Some((U256::from(5), 0)),
                current: None,
                created: false,
                selfdestructed: false,
            }
        );
        assert!(
            !sink
                .0
                .iter()
                .any(|event| matches!(event, Event::Account { address, .. } if *address == absent))
        );
        assert!(sink.0.contains(&Event::StorageWipe(replaced)));
        assert!(sink.0.contains(&Event::Account {
            address: replaced,
            original: Some((U256::from(5), 0)),
            current: Some((U256::ZERO, 0, keccak256([]))),
            created: false,
            selfdestructed: false,
        }));
    }
}
