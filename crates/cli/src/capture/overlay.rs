use super::{
    block::MainnetBlock,
    builder::AccountField,
    parse::{parse_address, parse_b256},
};
use alloy_consensus::{
    EthereumTxEnvelope, TxEip4844, transaction::Transaction as AlloyTransaction,
};
use alloy_primitives::{Address, B256, U256};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
pub(super) struct Overlay {
    accounts: BTreeMap<Address, OverlayAccount>,
}

impl Overlay {
    pub(super) fn account_exists(&self, address: &Address) -> bool {
        self.accounts.get(address).is_some_and(|account| account.exists && !account.deleted)
    }

    pub(super) fn account_field(&self, address: &Address, field: AccountField) -> bool {
        self.accounts.get(address).is_some_and(|account| account.deleted || account.has(field))
    }

    pub(super) fn storage_slot(&self, address: &Address, slot: &B256) -> bool {
        self.accounts
            .get(address)
            .is_some_and(|account| account.deleted || account.storage.contains(slot))
    }

    pub(super) fn apply_post(&mut self, post: &Value) {
        let Some(accounts) = post.as_object() else {
            return;
        };
        for (address, value) in accounts {
            let Ok(address) = parse_address(address) else {
                continue;
            };
            let account = self.accounts.entry(address).or_default();
            if value.is_null() {
                account.deleted = true;
                account.exists = false;
                account.balance = false;
                account.nonce = false;
                account.code = false;
                account.storage.clear();
                continue;
            }
            let Some(object) = value.as_object() else {
                continue;
            };
            if object.contains_key("balance") {
                account.deleted = false;
                account.exists = true;
                account.balance = true;
            }
            if object.contains_key("nonce") {
                account.deleted = false;
                account.exists = true;
                account.nonce = true;
            }
            if object.contains_key("code") {
                account.deleted = false;
                account.exists = true;
                account.code = true;
            }
            if let Some(storage) = object.get("storage").and_then(Value::as_object) {
                account.deleted = false;
                account.exists = true;
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
            let account = self.accounts.entry(authority).or_default();
            account.deleted = false;
            account.exists = true;
            account.nonce = true;
            account.code = true;
        }
    }

    pub(super) fn apply_withdrawals(&mut self, block: &MainnetBlock) {
        let Some(withdrawals) = &block.body.withdrawals else {
            return;
        };
        for withdrawal in withdrawals.iter() {
            let account = self.accounts.entry(withdrawal.address).or_default();
            account.deleted = false;
            account.exists = true;
            account.balance = true;
        }
    }
}

#[derive(Default)]
struct OverlayAccount {
    exists: bool,
    deleted: bool,
    balance: bool,
    nonce: bool,
    code: bool,
    storage: BTreeSet<B256>,
}

impl OverlayAccount {
    const fn has(&self, field: AccountField) -> bool {
        match field {
            AccountField::Balance => self.balance,
            AccountField::Nonce => self.nonce,
            AccountField::Code => self.code,
        }
    }
}
