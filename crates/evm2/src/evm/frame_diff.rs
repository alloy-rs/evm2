//! Assertion-frame state diffs.

use super::{AccountInfo, State};
use alloc::vec::Vec;
use alloy_eips::eip7906::TxTraceParam;
use alloy_primitives::{Address, U256};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct FrameDiff {
    balances: Vec<(Address, U256, U256)>,
    slots: Vec<(Address, U256, U256, U256)>,
    codes: Vec<(Address, U256, U256)>,
    nonces: Vec<(Address, u64, u64)>,
}

impl FrameDiff {
    pub(super) fn capture(state: &State<'_>) -> Self {
        let mut diff = Self::default();
        let absent = AccountInfo::default();
        for a in state.transaction_changes().account_changes() {
            let before = a.original.unwrap_or(&absent);
            let after = a.current.unwrap_or(&absent);
            if before.nonce != after.nonce {
                diff.nonces.push((a.address, before.nonce, after.nonce));
            }
            if before.balance != after.balance {
                diff.balances.push((a.address, before.balance, after.balance));
            }
            for (&key, slot) in &a.storage.slots {
                if slot.value.original != slot.value.current {
                    diff.slots.push((a.address, key, slot.value.original, slot.value.current));
                }
            }
            if before.code_hash != after.code_hash {
                diff.codes.push((
                    a.address,
                    U256::from_be_bytes(before.code_hash.0),
                    U256::from_be_bytes(after.code_hash.0),
                ));
            }
        }
        diff.balances.sort_unstable_by_key(|v| v.0);
        diff.slots.sort_unstable_by_key(|v| (v.0, v.1));
        diff.codes.sort_unstable_by_key(|v| v.0);
        diff.nonces.sort_unstable_by_key(|v| v.0);
        diff
    }

    pub(super) fn value(&self, selector: TxTraceParam, index: usize) -> Option<U256> {
        Some(match selector {
            TxTraceParam::BalancesChanged => U256::from(self.balances.len()),
            TxTraceParam::SlotsChanged => U256::from(self.slots.len()),
            TxTraceParam::CodesChanged => U256::from(self.codes.len()),
            TxTraceParam::NoncesChanged => U256::from(self.nonces.len()),
            TxTraceParam::BalanceChangeAddress => {
                U256::from_be_slice(self.balances.get(index)?.0.as_slice())
            }
            TxTraceParam::BalanceBefore => self.balances.get(index)?.1,
            TxTraceParam::BalanceAfter => self.balances.get(index)?.2,
            TxTraceParam::SlotChangeAddress => {
                U256::from_be_slice(self.slots.get(index)?.0.as_slice())
            }
            TxTraceParam::SlotValueBefore => self.slots.get(index)?.2,
            TxTraceParam::SlotValueAfter => self.slots.get(index)?.3,
            TxTraceParam::SlotKey => self.slots.get(index)?.1,
            TxTraceParam::CodeChangeAddress => {
                U256::from_be_slice(self.codes.get(index)?.0.as_slice())
            }
            TxTraceParam::CodeHashBefore => self.codes.get(index)?.1,
            TxTraceParam::CodeHashAfter => self.codes.get(index)?.2,
            TxTraceParam::NonceChangeAddress => {
                U256::from_be_slice(self.nonces.get(index)?.0.as_slice())
            }
            TxTraceParam::NonceBefore => U256::from(self.nonces.get(index)?.1),
            TxTraceParam::NonceAfter => U256::from(self.nonces.get(index)?.2),
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BaseEvmTypes, Evm, Precompiles, SpecId,
        bytecode::Bytecode,
        env::BlockEnvExt,
        ethereum::ethereum_tx_registry,
        evm::{AccountInfo, InMemoryDB, frame::FrameTransactionRuntime},
        interpreter::{Gas, Host},
    };
    use alloc::sync::Arc;
    use alloy_eips::eip8141::{Frame, FrameMode};
    use alloy_primitives::{B256, Bytes, KECCAK256_EMPTY};

    #[test]
    fn code_changes_include_stored_delegation_replacement_and_clearing() {
        let deployed = Address::with_last_byte(0xa1);
        let delegated = Address::with_last_byte(0xa2);
        let replaced = Address::with_last_byte(0xa3);
        let unchanged = Address::with_last_byte(0xa4);
        let cleared = Address::with_last_byte(0xa5);
        let empty = Address::with_last_byte(0xa6);
        let code = Bytecode::new_legacy(Bytes::from_static(&[0x00]));
        let delegation = Bytecode::new_eip7702(deployed);
        let replacement = Bytecode::new_legacy(Bytes::from_static(&[0x5f, 0x00]));
        let mut db = InMemoryDB::default();
        db.insert_account_info(&replaced, AccountInfo::default().with_code(code.clone()));
        db.insert_account_info(&unchanged, AccountInfo::default().with_code(code.clone()));
        db.insert_account_info(&cleared, AccountInfo::default().with_code(delegation.clone()));
        let mut host = Evm::<BaseEvmTypes>::new(
            SpecId::BOGOTA,
            BlockEnvExt::default(),
            ethereum_tx_registry(SpecId::BOGOTA),
            db,
            Precompiles::base(SpecId::BOGOTA),
        );
        host.state.account(&deployed).unwrap().set_code_slow(code.clone());
        host.state.account(&delegated).unwrap().set_code_slow(delegation.clone());
        host.state.account(&replaced).unwrap().set_code_slow(replacement.clone());
        host.state.account(&unchanged).unwrap().set_code_slow(replacement.clone());
        host.state.account(&unchanged).unwrap().set_code_slow(code.clone());
        host.state.account(&cleared).unwrap().set_code_slow(Bytecode::default());
        host.state.account(&empty).unwrap().set_nonce(1);

        let diff = FrameDiff::capture(&host.state);
        let hash = |code: &Bytecode| U256::from_be_bytes(code.hash_slow().0);
        let empty_hash = U256::from_be_bytes(KECCAK256_EMPTY.0);
        assert_eq!(
            diff.codes,
            [
                (deployed, empty_hash, hash(&code)),
                (delegated, empty_hash, hash(&delegation)),
                (replaced, hash(&code), hash(&replacement)),
                (cleared, hash(&delegation), empty_hash),
            ]
        );
        assert_eq!(diff.nonces, [(empty, 0, 1)]);
    }

    #[test]
    fn code_adoption_reports_stored_delegation_hash_before_execution() {
        let authority = Address::with_last_byte(0xa1);
        let delegated = Address::with_last_byte(0xa2);
        let source = Address::with_last_byte(0xa3);
        let delegation = Bytecode::new_eip7702(delegated);
        let code = Bytecode::new_legacy(Bytes::from_static(&[0x00]));
        let mut db = InMemoryDB::default();
        db.insert_account_info(&authority, AccountInfo::default().with_code(delegation.clone()));
        db.insert_account_info(&source, AccountInfo::default().with_code(code.clone()));
        let mut host = Evm::<BaseEvmTypes>::new(
            SpecId::BOGOTA,
            BlockEnvExt::default(),
            ethereum_tx_registry(SpecId::BOGOTA),
            db,
            Precompiles::base(SpecId::BOGOTA),
        );
        host.set_code_from(&authority, &source, &mut Gas::new(50_000)).unwrap();
        let diff = FrameDiff::capture(&host.state);
        assert_eq!(
            diff.codes,
            [(
                authority,
                U256::from_be_bytes(delegation.hash_slow().0),
                U256::from_be_bytes(code.hash_slow().0),
            )]
        );
    }

    #[test]
    fn trace_view_does_not_load_or_warm_accounts_and_slots() {
        let address = Address::with_last_byte(0x90);
        let slot = U256::from(9);
        let mut db = InMemoryDB::default();
        db.insert_account_info(&address, AccountInfo::default().with_balance(U256::from(11)));
        db.insert_account_storage(&address, &slot, &U256::from(33));
        let mut host = Evm::<BaseEvmTypes>::new(
            SpecId::BOGOTA,
            BlockEnvExt::default(),
            ethereum_tx_registry(SpecId::BOGOTA),
            db,
            Precompiles::base(SpecId::BOGOTA),
        );
        let runtime = FrameTransactionRuntime::new(
            Arc::new(alloy_consensus::TxEip8141 {
                sender: address,
                frames: alloc::vec![Frame { mode: FrameMode::Assert, ..Default::default() }],
                ..Default::default()
            }),
            B256::ZERO,
            U256::ZERO,
            0,
        );
        host.frame_tx = Some(runtime);
        for selector in [0, 1, 2, 3, 0x11] {
            assert_eq!(host.txtrace(U256::from(selector), U256::ZERO), Some(U256::ZERO));
            assert_eq!(host.state.transaction_changes().account_changes().count(), 0);
            assert!(!host.state.is_storage_warm(&address, &slot));
        }
    }

    #[test]
    fn nonce_changes_only_include_net_account_nonce_changes_sorted_by_address() {
        let sender = Address::with_last_byte(0x90);
        let created = Address::with_last_byte(0x80);
        let restored = Address::with_last_byte(0x70);
        let mut db = InMemoryDB::default();
        db.insert_account_info(&sender, AccountInfo::default().with_nonce(7));
        db.insert_account_info(&restored, AccountInfo::default().with_nonce(3));
        let mut host = Evm::<BaseEvmTypes>::new(
            SpecId::BOGOTA,
            BlockEnvExt::default(),
            ethereum_tx_registry(SpecId::BOGOTA),
            db,
            Precompiles::base(SpecId::BOGOTA),
        );
        host.state.account(&sender).unwrap();
        host.state.account(&restored).unwrap().set_nonce(4);
        host.state.account(&restored).unwrap().set_nonce(3);
        host.state.account(&created).unwrap().set_nonce(1);
        assert_eq!(FrameDiff::capture(&host.state).nonces, [(created, 0, 1)]);
        host.state.account(&sender).unwrap().set_nonce(8);
        assert_eq!(FrameDiff::capture(&host.state).nonces, [(created, 0, 1), (sender, 7, 8)]);
    }

    #[test]
    fn sendall_preserves_created_accounts_and_inflight_code_and_nonce_changes() {
        let created = Address::with_last_byte(0x80);
        let recipient = Address::with_last_byte(0x90);
        let code = Bytecode::new_legacy(Bytes::from_static(&[0xff]));
        let mut host = Evm::<BaseEvmTypes>::new(
            SpecId::BOGOTA,
            BlockEnvExt::default(),
            ethereum_tx_registry(SpecId::BOGOTA),
            InMemoryDB::default(),
            Precompiles::base(SpecId::BOGOTA),
        );
        {
            let mut account = host.state.account(&created).unwrap();
            account.mark_created();
            account.set_nonce(1);
            account.set_code_slow(code.clone());
        }
        host.selfdestruct(&created, &recipient, false).unwrap();
        assert!(!host.state.account(&created).unwrap().is_destructed());
        let diff = FrameDiff::capture(&host.state);
        assert_eq!(diff.nonces, [(created, 0, 1)]);
        assert_eq!(
            diff.codes,
            [(
                created,
                U256::from_be_bytes(KECCAK256_EMPTY.0),
                U256::from_be_bytes(code.hash_slow().0),
            )]
        );
    }
}
