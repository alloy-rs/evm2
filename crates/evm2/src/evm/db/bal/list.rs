//! The top-level Block Access List structure.

use super::{AccountBal, BalError, BlockAccessIndex};
use crate::evm::state::AccountChange;
use alloy_eip7928::BlockAccessList as AlloyBal;
use alloy_primitives::{Address, U256, map::AddressMap};

/// BAL structure.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Bal {
    /// Accounts bal.
    pub accounts: AddressMap<AccountBal>,
}

impl FromIterator<(Address, AccountBal)> for Bal {
    fn from_iter<I: IntoIterator<Item = (Address, AccountBal)>>(iter: I) -> Self {
        Self { accounts: iter.into_iter().collect() }
    }
}

impl Bal {
    /// Create a new BAL builder.
    pub fn new() -> Self {
        Self { accounts: AddressMap::default() }
    }

    /// Pretty print the entire BAL structure in a human-readable format.
    #[cfg(feature = "std")]
    pub fn pretty_print(&self) {
        println!("=== Block Access List (BAL) ===");
        println!("Total accounts: {}", self.accounts.len());
        println!();

        if self.accounts.is_empty() {
            println!("(empty)");
            return;
        }

        // Sort accounts by address before printing
        let mut sorted_accounts: Vec<_> = self.accounts.iter().collect();
        sorted_accounts.sort_unstable_by_key(|(address, _)| *address);

        for (idx, (address, account)) in sorted_accounts.into_iter().enumerate() {
            println!("Account #{idx} - Address: {address:?}");
            println!("  Account Info:");

            // Print nonce writes
            if account.account_info.nonce.is_empty() {
                println!("    Nonce: (read-only, no writes)");
            } else {
                println!("    Nonce writes:");
                for (bal_index, nonce) in &account.account_info.nonce.writes {
                    println!("      [{bal_index}] -> {nonce}");
                }
            }

            // Print balance writes
            if account.account_info.balance.is_empty() {
                println!("    Balance: (read-only, no writes)");
            } else {
                println!("    Balance writes:");
                for (bal_index, balance) in &account.account_info.balance.writes {
                    println!("      [{bal_index}] -> {balance}");
                }
            }

            // Print code writes
            if account.account_info.code.is_empty() {
                println!("    Code: (read-only, no writes)");
            } else {
                println!("    Code writes:");
                for (bal_index, (code_hash, bytecode)) in &account.account_info.code.writes {
                    println!(
                        "      [{}] -> hash: {:?}, size: {} bytes",
                        bal_index,
                        code_hash,
                        bytecode.len()
                    );
                }
            }

            // Print storage writes
            println!("  Storage:");
            if account.storage.storage.is_empty() {
                println!("    (no storage slots)");
            } else {
                println!("    Total slots: {}", account.storage.storage.len());
                for (storage_key, storage_writes) in &account.storage.storage {
                    println!("    Slot: {storage_key:#x}");
                    if storage_writes.is_empty() {
                        println!("      (read-only, no writes)");
                    } else {
                        println!("      Writes:");
                        for (bal_index, value) in &storage_writes.writes {
                            println!("        [{bal_index}] -> {value:?}");
                        }
                    }
                }
            }

            println!();
        }
        println!("=== End of BAL ===");
    }

    #[inline]
    /// Extend BAL with an [`AccountChange`] produced by transaction execution.
    pub fn update_account(
        &mut self,
        bal_index: BlockAccessIndex,
        address: Address,
        account: &AccountChange,
    ) {
        let bal_account = self.accounts.entry(address).or_default();
        bal_account.update(bal_index, account);
    }

    /// Populate storage slot from BAL by account address.
    ///
    /// If the account is not found in the BAL, or the slot is not listed under it, an error is
    /// returned.
    #[inline]
    pub fn populate_storage_slot(
        &self,
        account_address: Address,
        bal_index: BlockAccessIndex,
        key: U256,
        value: &mut U256,
    ) -> Result<(), BalError> {
        let Some(bal_account) = self.accounts.get(&account_address) else {
            return Err(BalError::AccountNotFound { address: account_address });
        };

        if let Some(bal_value) = bal_account.storage.get(&account_address, key, bal_index)? {
            *value = bal_value;
        };
        Ok(())
    }

    /// Consume `Bal` and create a canonical EIP-7928 [`AlloyBal`].
    ///
    /// The returned access list is ordered deterministically: accounts are
    /// sorted lexicographically by address, and each account's nested reads and
    /// changes are sorted by [`AccountBal::into_alloy_account`].
    ///
    /// This matches the EIP-7928 ordering requirements:
    /// <https://eips.ethereum.org/EIPS/eip-7928#ordering-uniqueness-and-determinism>.
    pub fn into_alloy_bal(self) -> AlloyBal {
        let mut alloy_bal = AlloyBal::from_iter(
            self.accounts.into_iter().map(|(address, account)| account.into_alloy_account(address)),
        );
        alloy_bal.sort_unstable_by_key(|a| a.address);
        alloy_bal
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        bytecode::Bytecode,
        evm::{
            db::bal::{AccountInfoBal, BalWrites, StorageBal},
            state::{AccountChange, AccountInfo, Tracked},
        },
    };
    use alloc::collections::BTreeMap;
    use alloy_eip7928::{
        AccountChanges as AlloyAccountChanges, BalanceChange as AlloyBalanceChange,
        CodeChange as AlloyCodeChange, NonceChange as AlloyNonceChange,
        SlotChanges as AlloySlotChanges, StorageChange as AlloyStorageChange,
    };
    use alloy_primitives::{B256, Bytes, U256};

    fn code(byte: u8) -> (B256, Bytecode) {
        let bytecode = Bytecode::new_raw(vec![byte].into());
        (bytecode.hash_slow(), bytecode)
    }

    const fn idx(index: u64) -> BlockAccessIndex {
        BlockAccessIndex::new(index)
    }

    #[test]
    fn into_alloy_bal_canonicalizes_eip_7928_ordering() {
        let low_address = Address::with_last_byte(1);
        let high_address = Address::with_last_byte(2);

        let unordered_account = AccountBal {
            account_info: AccountInfoBal {
                nonce: BalWrites { writes: vec![(idx(9), 90), (idx(4), 40)] },
                balance: BalWrites {
                    writes: vec![(idx(5), U256::from(50)), (idx(2), U256::from(20))],
                },
                code: BalWrites { writes: vec![(idx(7), code(7)), (idx(3), code(3))] },
            },
            storage: StorageBal {
                storage: BTreeMap::from([
                    (
                        U256::from(4),
                        BalWrites {
                            writes: vec![(idx(8), U256::from(80)), (idx(6), U256::from(60))],
                        },
                    ),
                    (U256::from(1), BalWrites { writes: vec![] }),
                    (
                        U256::from(2),
                        BalWrites {
                            writes: vec![(idx(3), U256::from(30)), (idx(1), U256::from(10))],
                        },
                    ),
                    (U256::from(3), BalWrites { writes: vec![] }),
                ]),
            },
        };

        let alloy_bal = Bal::from_iter([
            (high_address, AccountBal::default()),
            (low_address, unordered_account),
        ])
        .into_alloy_bal();

        assert_eq!(
            alloy_bal.iter().map(|account| account.address).collect::<Vec<_>>(),
            vec![low_address, high_address]
        );

        let account = &alloy_bal[0];
        assert_eq!(account.storage_reads, vec![U256::from(1), U256::from(3)]);
        assert_eq!(
            account.storage_changes.iter().map(|slot| slot.slot).collect::<Vec<_>>(),
            vec![U256::from(2), U256::from(4)]
        );
        assert_eq!(
            account.storage_changes[0]
                .changes
                .iter()
                .map(|change| change.block_access_index)
                .collect::<Vec<_>>(),
            vec![idx(1), idx(3)]
        );
        assert_eq!(
            account.storage_changes[1]
                .changes
                .iter()
                .map(|change| change.block_access_index)
                .collect::<Vec<_>>(),
            vec![idx(6), idx(8)]
        );
        assert_eq!(
            account
                .balance_changes
                .iter()
                .map(|change| change.block_access_index)
                .collect::<Vec<_>>(),
            vec![idx(2), idx(5)]
        );
        assert_eq!(
            account
                .nonce_changes
                .iter()
                .map(|change| change.block_access_index)
                .collect::<Vec<_>>(),
            vec![idx(4), idx(9)]
        );
        assert_eq!(
            account.code_changes.iter().map(|change| change.block_access_index).collect::<Vec<_>>(),
            vec![idx(3), idx(7)]
        );
    }

    #[test]
    fn update_account_builds_bal_from_account_change() {
        let address = Address::with_last_byte(1);

        // A freshly created account: no original info, present nonce/balance set, and one changed
        // storage slot plus one loaded-but-unchanged (read) slot.
        let mut change = AccountChange {
            original: None,
            current: Some(AccountInfo::default().with_nonce(1).with_balance(U256::from(100))),
            ..Default::default()
        };
        change.storage.insert(U256::from(5), Tracked::from_parts(U256::ZERO, U256::from(42)));
        change.storage.insert(U256::from(6), Tracked::new(U256::from(7)));

        let mut bal = Bal::new();
        bal.update_account(idx(1), address, &change);

        let account = bal.accounts.get(&address).unwrap();
        assert_eq!(account.account_info.nonce.writes, vec![(idx(1), 1)]);
        assert_eq!(account.account_info.balance.writes, vec![(idx(1), U256::from(100))]);
        // No code change, so no code writes.
        assert!(account.account_info.code.writes.is_empty());
        // Changed slot recorded as a write.
        assert_eq!(
            account.storage.storage.get(&U256::from(5)).unwrap().writes,
            vec![(idx(1), U256::from(42))]
        );
        // Loaded-but-unchanged slot recorded as a read (empty writes).
        assert!(account.storage.storage.get(&U256::from(6)).unwrap().writes.is_empty());
    }

    #[test]
    fn update_account_selfdestruct_uses_finalized_post_state() {
        let address = Address::with_last_byte(1);

        // Transaction finalization already resolved the selfdestructed account: removed
        // (`current: None`), storage overlay wiped, with a leftover loaded-but-unchanged slot
        // surfacing as a read. The BAL derives from that diff without special-casing.
        let mut change = AccountChange {
            original: Some(AccountInfo::default().with_balance(U256::from(100))),
            current: None,
            ..Default::default()
        };
        change.mark_selfdestruct();
        change.storage.insert(U256::from(5), Tracked::new(U256::from(42)));

        let mut bal = Bal::new();
        bal.update_account(idx(2), address, &change);

        let account = bal.accounts.get(&address).unwrap();
        // Balance goes to zero on removal.
        assert_eq!(account.account_info.balance.writes, vec![(idx(2), U256::ZERO)]);
        // The loaded-but-unchanged slot stays a read (empty writes).
        assert!(account.storage.storage.get(&U256::from(5)).unwrap().writes.is_empty());
    }

    #[test]
    fn try_from_alloy_decodes_block_access_list() {
        let address = Address::with_last_byte(1);
        let code_bytes = Bytes::from_static(&[0x60, 0x00]);
        let alloy_bal = vec![AlloyAccountChanges {
            address,
            code_changes: vec![AlloyCodeChange::new(idx(1), code_bytes.clone())],
            ..Default::default()
        }];

        let bal = Bal::try_from_alloy(alloy_bal).unwrap();
        let account = bal.accounts.get(&address).unwrap();
        let (_, bytecode) = &account.account_info.code.writes[0].1;

        assert_eq!(bytecode.original_bytes(), code_bytes);
    }

    #[test]
    fn clone_from_alloy_matches_owned_conversion() {
        let address = Address::with_last_byte(1);
        let code_bytes = Bytes::from_static(&[0x60, 0x00]);
        let alloy_bal = vec![AlloyAccountChanges {
            address,
            storage_changes: vec![AlloySlotChanges::new(
                U256::from(1),
                vec![AlloyStorageChange::new(idx(1), U256::from(10))],
            )],
            storage_reads: vec![U256::from(2)],
            balance_changes: vec![AlloyBalanceChange::new(idx(2), U256::from(20))],
            nonce_changes: vec![AlloyNonceChange::new(idx(3), 30)],
            code_changes: vec![AlloyCodeChange::new(idx(4), code_bytes.clone())],
        }];

        let borrowed = Bal::clone_from_alloy(&alloy_bal).unwrap();
        let owned = Bal::try_from_alloy(alloy_bal.clone()).unwrap();

        assert_eq!(borrowed, owned);
        assert_eq!(alloy_bal[0].code_changes[0].new_code(), &code_bytes);
    }

    #[test]
    fn try_from_alloy_errors_on_invalid_code_change() {
        let alloy_bal = vec![AlloyAccountChanges {
            address: Address::with_last_byte(1),
            code_changes: vec![AlloyCodeChange::new(idx(1), vec![0xef, 0x01, 0xde].into())],
            ..Default::default()
        }];

        assert!(Bal::try_from_alloy(alloy_bal).is_err());
    }

    #[test]
    fn clone_from_alloy_errors_on_invalid_code_change() {
        let alloy_bal = vec![AlloyAccountChanges {
            address: Address::with_last_byte(1),
            code_changes: vec![AlloyCodeChange::new(idx(1), vec![0xef, 0x01, 0xde].into())],
            ..Default::default()
        }];

        assert!(Bal::clone_from_alloy(&alloy_bal).is_err());
    }
}
