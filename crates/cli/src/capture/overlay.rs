use super::{
    MainnetBlock,
    builder::AccountField,
    parse::{parse_address, parse_b256},
};
use alloy_consensus::{
    EthereumTxEnvelope, TxEip4844, transaction::Transaction as AlloyTransaction,
};
use alloy_primitives::{Address, B256, U256};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

const ONE_GWEI: u64 = 1_000_000_000;

#[derive(Default)]
pub(super) struct Overlay {
    accounts: BTreeMap<Address, OverlayAccount>,
}

impl Overlay {
    pub(super) fn account_exists(&self, address: &Address) -> bool {
        matches!(self.accounts.get(address), Some(OverlayAccount::Written(_)))
    }

    pub(super) fn account_field(&self, address: &Address, field: AccountField) -> bool {
        self.accounts.get(address).is_some_and(|account| match account {
            OverlayAccount::Deleted => true,
            OverlayAccount::Written(account) => account.has(field),
        })
    }

    pub(super) fn storage_slot(&self, address: &Address, slot: &B256) -> bool {
        self.accounts.get(address).is_some_and(|account| match account {
            OverlayAccount::Deleted => true,
            OverlayAccount::Written(account) => account.storage.contains(slot),
        })
    }

    pub(super) fn withdrawal_balance(&self, address: &Address) -> U256 {
        match self.accounts.get(address) {
            Some(OverlayAccount::Written(account)) => account.withdrawal_balance,
            Some(OverlayAccount::Deleted) | None => U256::ZERO,
        }
    }

    pub(super) fn apply_post(&mut self, post: &Value) {
        let Some(accounts) = post.as_object() else {
            return;
        };
        for (address, value) in accounts {
            let Ok(address) = parse_address(address) else {
                continue;
            };
            if value.is_null() {
                self.accounts.insert(address, OverlayAccount::Deleted);
                continue;
            }
            let Some(object) = value.as_object() else {
                continue;
            };
            let balance = object.contains_key("balance");
            let nonce = object.contains_key("nonce");
            let code = object.contains_key("code");
            let storage = object.get("storage").and_then(Value::as_object);
            if !(balance || nonce || code || storage.is_some()) {
                continue;
            }
            let account = self.written(address);
            account.balance |= balance;
            account.nonce |= nonce;
            account.code |= code;
            if let Some(storage) = storage {
                account.storage.extend(storage.keys().filter_map(|slot| parse_b256(slot).ok()));
            }
        }
    }

    pub(super) fn apply_authorization_writes(
        &mut self,
        tx: &EthereumTxEnvelope<TxEip4844>,
        chain_id: u64,
    ) {
        let Some(authorizations) = tx.authorization_list() else {
            return;
        };
        for authorization in authorizations {
            if !authorization.chain_id().is_zero()
                && authorization.chain_id() != &U256::from(chain_id)
            {
                continue;
            }
            if authorization.nonce() == u64::MAX {
                continue;
            }
            let Ok(authority) = authorization.recover_authority() else {
                continue;
            };
            let account = self.written(authority);
            account.nonce = true;
            account.code = true;
        }
    }

    pub(super) fn apply_withdrawals(&mut self, block: &MainnetBlock) {
        let Some(withdrawals) = &block.body.withdrawals else {
            return;
        };
        for withdrawal in withdrawals.iter() {
            self.apply_withdrawal(
                withdrawal.address,
                U256::from(withdrawal.amount).saturating_mul(U256::from(ONE_GWEI)),
            );
        }
    }

    pub(super) fn apply_withdrawal(&mut self, address: Address, amount: U256) {
        let account = self.written(address);
        account.withdrawal_balance = account.withdrawal_balance.saturating_add(amount);
    }

    /// Returns the written fields of `address`, recreating it if it was deleted.
    fn written(&mut self, address: Address) -> &mut WrittenAccount {
        let account = self.accounts.entry(address).or_insert(OverlayAccount::Deleted);
        if let OverlayAccount::Deleted = account {
            *account = OverlayAccount::Written(WrittenAccount::default());
        }
        let OverlayAccount::Written(account) = account else { unreachable!("recreated above") };
        account
    }
}

/// An account written inside the captured range.
enum OverlayAccount {
    /// Deleted: every field and storage slot is known to be empty.
    Deleted,
    /// Live: only the written fields and storage slots are known.
    Written(WrittenAccount),
}

/// The fields and storage slots a live account had written inside the captured range.
#[derive(Default)]
struct WrittenAccount {
    balance: bool,
    nonce: bool,
    code: bool,
    withdrawal_balance: U256,
    storage: BTreeSet<B256>,
}

impl WrittenAccount {
    const fn has(&self, field: AccountField) -> bool {
        match field {
            AccountField::Balance => self.balance,
            AccountField::Nonce => self.nonce,
            AccountField::Code => self.code,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ADDRESS: Address = Address::repeat_byte(0x11);

    fn post(account: Value) -> Value {
        json!({ ADDRESS.to_string(): account })
    }

    #[test]
    fn written_fields_are_known_individually() {
        let mut overlay = Overlay::default();
        let slot = B256::with_last_byte(1);
        overlay
            .apply_post(&post(json!({ "balance": "0x1", "storage": { slot.to_string(): "0x2" } })));

        assert!(overlay.account_exists(&ADDRESS));
        assert!(overlay.account_field(&ADDRESS, AccountField::Balance));
        assert!(!overlay.account_field(&ADDRESS, AccountField::Nonce));
        assert!(!overlay.account_field(&ADDRESS, AccountField::Code));
        assert!(overlay.storage_slot(&ADDRESS, &slot));
        assert!(!overlay.storage_slot(&ADDRESS, &B256::with_last_byte(2)));
    }

    #[test]
    fn deleted_account_knows_every_field_and_slot() {
        let mut overlay = Overlay::default();
        overlay.apply_withdrawal(ADDRESS, U256::from(7));
        overlay.apply_post(&post(Value::Null));

        assert!(!overlay.account_exists(&ADDRESS));
        assert!(overlay.account_field(&ADDRESS, AccountField::Nonce));
        assert!(overlay.storage_slot(&ADDRESS, &B256::with_last_byte(1)));
        assert_eq!(overlay.withdrawal_balance(&ADDRESS), U256::ZERO);
    }

    #[test]
    fn write_recreates_deleted_account() {
        let mut overlay = Overlay::default();
        overlay.apply_post(&post(Value::Null));
        overlay.apply_withdrawal(ADDRESS, U256::from(7));

        assert!(overlay.account_exists(&ADDRESS));
        assert!(!overlay.account_field(&ADDRESS, AccountField::Balance));
        assert!(!overlay.storage_slot(&ADDRESS, &B256::with_last_byte(1)));
        assert_eq!(overlay.withdrawal_balance(&ADDRESS), U256::from(7));
    }

    #[test]
    fn empty_post_entry_writes_nothing() {
        let mut overlay = Overlay::default();
        overlay.apply_post(&post(json!({})));
        assert!(!overlay.account_exists(&ADDRESS));

        overlay.apply_post(&post(Value::Null));
        overlay.apply_post(&post(json!({})));
        assert!(!overlay.account_exists(&ADDRESS));
        assert!(overlay.account_field(&ADDRESS, AccountField::Code));
    }
}
