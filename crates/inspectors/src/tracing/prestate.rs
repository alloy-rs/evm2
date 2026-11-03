//! Prestate tracing inspector
//!
//! The prestateTracer reports the accounts and storage slots a transaction needs, or in diff mode
//! how the transaction changed them. This inspector follows geth's `prestateTracer`: it collects
//! the accounts and slots the transaction names, and reads their values from the state before and
//! after the transaction when the result is built.
//!
//! Like geth, it names the accounts of the sender, the recipient, the coinbase and the EIP-7702
//! authorities, and every account and slot that an opcode addresses. It takes the slots and the
//! accounts of `SLOAD`, `SSTORE`, `BALANCE`, `EXTCODE*` and `SELFDESTRUCT`, and the addresses of
//! `CREATE` and `CREATE2`, from their steps, and the targets of calls from the call hook, which
//! runs for every call opcode that gets past its gas.
//!
//! geth skips an opcode that fails before its body, because it passes the failure to the hook, and
//! this inspector skips an opcode whose step ends in a halt, and a call that does not start. The
//! two agree for the failures geth raises before the body: stack bounds, gas, and write protection
//! for `SSTORE`, `CREATE`, `CREATE2`, `SELFDESTRUCT` and a `CALL` with value, which geth checks in
//! the gas function.
//!
//! The inspector only asks for the steps of the opcodes it looks at.
//!
//! See also <https://github.com/ethereum/go-ethereum/blob/a0cba163b1f17f6044df84502ff13036dffb5de1/eth/tracers/native/prestate.go>

use crate::tracing::{keccak::memory_copy_padded, utils::load_account_code};
use alloc::{collections::BTreeMap, vec::Vec};
use alloy_consensus::{Transaction, transaction::Recovered};
use alloy_primitives::{Address, B256, Bytes, U256, keccak256, map::AddressSet};
use alloy_rpc_types_trace::geth::{
    AccountState, DiffMode, PreStateConfig, PreStateFrame, PreStateMode,
};
use evm2::{
    AccountInfo, EvmTypes, EvmTypesHost, Inspector, InspectorConfig, OpcodeSet, SpecId,
    TxResultWithState,
    env::BlockEnv,
    evm::{AccountChanges, DbResult, DynDatabase, PendingState},
    interpreter::{Interpreter, Message, MessageResult, opcode::op},
};

/// Inspector that collects what geth's `prestateTracer` reports for a transaction.
#[derive(Clone, Debug)]
pub struct PrestateInspector {
    /// The tracer config.
    config: PreStateConfig,
    /// The accounts an opcode addressed, with repeats.
    accounts: Vec<Address>,
    /// The slots an opcode addressed, with repeats. Sorted by account once the result is built.
    slots: Vec<(Address, B256)>,
    /// The addresses of the contracts the transaction created.
    created: AddressSet,
    /// The accounts that executed `SELFDESTRUCT` and that geth deletes.
    deleted: AddressSet,
    /// What the opcode that is running addresses, kept until its step ends.
    pending: Option<Lookup>,
    /// The addresses that the `CREATE` and `CREATE2` that are running create, one per depth. They
    /// run frames of their own before their steps end, which `pending` does not survive.
    creates: Vec<(u16, Address)>,
}

/// The opcodes whose steps the inspector looks at: they address an account or a slot, or create
/// one. The call opcodes are left to the call hook.
const LOOKUP_OPCODES: OpcodeSet = {
    let mut set = OpcodeSet::EMPTY;
    set.insert(op::SLOAD);
    set.insert(op::SSTORE);
    set.insert(op::EXTCODECOPY);
    set.insert(op::EXTCODEHASH);
    set.insert(op::EXTCODESIZE);
    set.insert(op::BALANCE);
    set.insert(op::SELFDESTRUCT);
    set.insert(op::CREATE);
    set.insert(op::CREATE2);
    set
};

/// An account or slot an opcode addresses, taken before it runs and kept if its step does not end
/// in a halt.
#[derive(Clone, Copy, Debug)]
enum Lookup {
    /// A storage slot of the executing account.
    Slot(Address, B256),
    /// An account.
    Account(Address),
    /// The beneficiary of a `SELFDESTRUCT` in the given account.
    SelfDestruct(Address, Address),
}

impl PrestateInspector {
    /// Returns an inspector that collects the result of the given tracer config.
    pub fn new(config: PreStateConfig) -> Self {
        Self {
            config,
            accounts: Vec::new(),
            slots: Vec::new(),
            created: AddressSet::default(),
            deleted: AddressSet::default(),
            pending: None,
            creates: Vec::new(),
        }
    }

    /// Clears what the inspector collected, to trace the next transaction.
    pub fn clear(&mut self) {
        self.accounts.clear();
        self.slots.clear();
        self.created.clear();
        self.deleted.clear();
        self.pending = None;
        self.creates.clear();
    }

    /// Returns the result of the tracer for a transaction that ran with this inspector.
    ///
    /// `db` is the state before the transaction.
    pub fn frame<T: EvmTypesHost<Tx: Transaction>>(
        &mut self,
        tx: &Recovered<T::Tx>,
        block_env: &BlockEnv<T>,
        result: &TxResultWithState<T>,
        db: &mut dyn DynDatabase,
    ) -> DbResult<PreStateFrame> {
        self.slots.sort_unstable();
        self.slots.dedup();
        let accounts = self.tx_accounts::<T>(tx, block_env, db)?;
        if self.config.is_diff_mode() {
            self.diff_frame(&accounts, &result.pending_state, db)
        } else {
            self.pre_frame(&accounts, &result.pending_state, db)
        }
    }

    /// Returns the accounts the result reports: the ones an opcode addressed and the ones the
    /// transaction names.
    fn tx_accounts<T: EvmTypesHost<Tx: Transaction>>(
        &self,
        tx: &Recovered<T::Tx>,
        block_env: &BlockEnv<T>,
        db: &mut dyn DynDatabase,
    ) -> DbResult<AddressSet> {
        let mut accounts = AddressSet::from_iter(self.accounts.iter().copied());
        accounts.extend(self.slots.iter().map(|(account, _)| *account));
        let from = tx.signer();
        let to = match tx.to() {
            Some(to) => {
                // geth looks up the delegation target of the recipient before the authorizations
                // of the transaction apply. Only EIP-7702 makes such code.
                if let Some(info) = db.get_account(&to)?
                    && let Some(code) = load_account_code(db, &info)?
                    && let Some(target) = parse_delegation(&code)
                {
                    accounts.insert(target);
                }
                to
            }
            None => from.create(db.get_account(&from)?.unwrap_or_default().nonce),
        };
        accounts.extend([from, to, block_env.beneficiary]);
        // An authorization whose signature does not recover names no account.
        let authorizations = tx.authorization_list().unwrap_or_default();
        accounts.extend(authorizations.iter().filter_map(|auth| auth.recover_authority().ok()));
        Ok(accounts)
    }

    /// Returns the accounts the transaction needs, as they were before it.
    fn pre_frame(
        &self,
        accounts: &AddressSet,
        state: &PendingState,
        db: &mut dyn DynDatabase,
    ) -> DbResult<PreStateFrame> {
        let mut prestate = BTreeMap::new();
        for addr in accounts {
            let changes = state.account_changes_for(addr);
            let original = original_account(changes, addr, db)?;
            // Geth omits accounts that were empty before execution, even if fees fund them.
            // Check the original code hash before applying the disableCode output option.
            if original.is_empty() {
                continue;
            }
            let code =
                if self.config.code_enabled() { load_account_code(db, &original)? } else { None };
            let mut account =
                AccountState::from_account_info(original.nonce, original.balance, code);
            self.insert_pre_slots(&mut account, addr, changes, db)?;
            prestate.insert(*addr, account);
        }
        Ok(PreStateFrame::Default(PreStateMode(prestate)))
    }

    /// Returns the difference between the state before and after the transaction.
    fn diff_frame(
        &self,
        accounts: &AddressSet,
        state: &PendingState,
        db: &mut dyn DynDatabase,
    ) -> DbResult<PreStateFrame> {
        let (code_enabled, storage_enabled) =
            (self.config.code_enabled(), self.config.storage_enabled());
        let mut diff = DiffMode::default();
        let mut empty_before = AddressSet::default();
        for addr in accounts {
            let changes = state.account_changes_for(addr);
            let original = original_account(changes, addr, db)?;
            let pre_code = if code_enabled { load_account_code(db, &original)? } else { None };

            let mut pre_state =
                AccountState::from_account_info(original.nonce, original.balance, pre_code.clone());

            // Geth drops every account that was empty before the transaction from the pre state,
            // whether the transaction created it or only funded it.
            // See <https://etherscan.io/tx/0x391f4b6a382d3bcc3120adc2ea8c62003e604e487d97281129156fd284a1a89d>
            // <https://github.com/paradigmxyz/reth/issues/19703#issuecomment-3527067849>
            if original.is_empty() {
                empty_before.insert(*addr);
            }

            // Geth keeps the pre state of a deleted account as it looked it up, with every slot
            // an opcode addressed, unchanged and zero ones included, and reports no post state.
            if self.deleted.contains(addr) {
                self.insert_pre_slots(&mut pre_state, addr, changes, db)?;
                diff.pre.insert(*addr, pre_state);
                continue;
            }

            // A deleted account is treated as drained: the post info is empty.
            let info = match changes {
                Some(changes) => changes.current.cloned().unwrap_or_default(),
                None => original.clone(),
            };
            let mut post_state = AccountState::from_account_info(
                info.nonce,
                info.balance,
                if code_enabled {
                    if info.code_hash == original.code_hash {
                        pre_code
                    } else {
                        // Note: the changed account from the state output always holds the code
                        info.code.as_ref().map(|code| code.original_bytes())
                    }
                } else {
                    None
                },
            );
            // Geth reports `"code":"0x"` when the transaction clears the code the account had.
            if pre_state.code.is_some() && post_state.code.is_none() {
                post_state.code = Some(Bytes::new());
            }

            if storage_enabled {
                for (_, key) in self.slots_of(addr) {
                    let slot = U256::from_be_bytes(key.0);
                    // The state of the transaction has every slot it loaded.
                    let tracked = changes.and_then(|changes| changes.storage.slots.get(&slot));
                    let pre = match tracked {
                        Some(tracked) => tracked.value.original,
                        None => db.get_storage(addr, &slot)?,
                    };
                    let current = tracked.map_or(pre, |tracked| tracked.value.current);
                    // Geth leaves an unchanged slot out and a slot that is zero before out of pre.
                    if current != pre {
                        if !pre.is_zero() {
                            pre_state.storage.insert(*key, pre.into());
                        }
                        if !current.is_zero() {
                            post_state.storage.insert(*key, current.into());
                        }
                    }
                }
            }

            diff.pre.insert(*addr, pre_state);
            diff.post.insert(*addr, post_state);
        }

        // Keep only changed entries.
        diff.retain_changed();
        for (addr, post_state) in &mut diff.post {
            if let Some(pre_state) = diff.pre.get(addr) {
                post_state.remove_matching_account_info(pre_state);
            }
        }
        diff.pre.retain(|addr, _| !empty_before.contains(addr));
        Ok(PreStateFrame::Diff(diff))
    }

    /// Returns the slots an opcode addressed in an account. They are sorted and have no repeats.
    fn slots_of(&self, addr: &Address) -> &[(Address, B256)] {
        let start = self.slots.partition_point(|(account, _)| account < addr);
        let len = self.slots[start..].partition_point(|(account, _)| account == addr);
        &self.slots[start..start + len]
    }

    /// Adds the slots an opcode addressed to the state, with their values before the transaction.
    fn insert_pre_slots(
        &self,
        account: &mut AccountState,
        addr: &Address,
        changes: Option<AccountChanges<'_>>,
        db: &mut dyn DynDatabase,
    ) -> DbResult<()> {
        if self.config.storage_enabled() {
            for (_, key) in self.slots_of(addr) {
                let slot = U256::from_be_bytes(key.0);
                let value = match changes.and_then(|changes| changes.storage.slots.get(&slot)) {
                    Some(tracked) => tracked.value.original,
                    None => db.get_storage(addr, &slot)?,
                };
                account.storage.insert(*key, value.into());
            }
        }
        Ok(())
    }

    /// Returns the address that the `CREATE` or `CREATE2` about to run creates.
    fn created_address<T: EvmTypes>(interp: &mut Interpreter<'_, '_, T>) -> Option<Address> {
        let account = interp.message().destination;
        let stack = interp.stack();
        let stack = stack.as_slice();
        // `back(0)` is the top of the stack.
        let back = |index: usize| stack.len().checked_sub(index + 1).map(|at| stack[at]);
        if interp.opcode() == op::CREATE {
            back(2)?;
            let info = interp.host().state_mut().account_info_untracked(&account).ok()?;
            return Some(account.create(info.unwrap_or_default().nonce));
        }
        let (offset, size, salt) = (back(1)?, back(2)?, back(3)?);
        // The operands are truncated to 64 bits, and geth fails on a negative one.
        let (offset, size) = (offset.as_limbs()[0], size.as_limbs()[0]);
        i64::try_from(offset).ok()?;
        i64::try_from(size).ok()?;
        let init_code = memory_copy_padded(interp.memory().as_slice(), offset, size)?;
        Some(account.create2(B256::from(salt), keccak256(init_code)))
    }

    /// Returns what the opcode about to run addresses.
    fn lookup<T: EvmTypes>(interp: &mut Interpreter<'_, '_, T>) -> Option<Lookup> {
        let account = interp.message().destination;
        let stack = interp.stack();
        // The top of the stack.
        let top = *stack.as_slice().last()?;
        Some(match interp.opcode() {
            op::SLOAD | op::SSTORE => Lookup::Slot(account, top.into()),
            op::SELFDESTRUCT => Lookup::SelfDestruct(account, Address::from_word(top.into())),
            _ => Lookup::Account(Address::from_word(top.into())),
        })
    }
}

impl<T: EvmTypes> Inspector<T> for PrestateInspector {
    #[inline]
    fn inspector_config(&self) -> InspectorConfig {
        InspectorConfig::new().with_steps(LOOKUP_OPCODES)
    }

    #[inline]
    fn step(&mut self, interp: &mut Interpreter<'_, '_, T>) {
        let op = interp.opcode();
        // The EVM may step more opcodes than asked for. A step of another opcode drops a lookup
        // whose `step_end` did not run.
        self.pending = None;
        if !LOOKUP_OPCODES.contains(op) {
            return;
        }
        if matches!(op, op::CREATE | op::CREATE2) {
            let depth = interp.message().depth;
            while self.creates.last().is_some_and(|(at, _)| *at >= depth) {
                self.creates.pop();
            }
            if let Some(address) = Self::created_address(interp) {
                self.creates.push((depth, address));
            }
        } else {
            self.pending = Self::lookup(interp);
        }
    }

    #[inline]
    fn step_end(&mut self, interp: &mut Interpreter<'_, '_, T>) {
        let halted = interp.result().is_err_and(|stop| stop.is_halt());
        // While a create runs its frames, only a step at its own depth can end its step.
        if let Some(&(depth, address)) = self.creates.last()
            && depth == interp.message().depth
        {
            self.creates.pop();
            // geth ignores an opcode that fails before its body runs.
            if !halted {
                self.accounts.push(address);
                self.created.insert(address);
            }
            return;
        }
        let Some(lookup) = self.pending.take() else { return };
        if halted {
            return;
        }
        match lookup {
            Lookup::Slot(account, key) => self.slots.push((account, key)),
            Lookup::Account(address) => self.accounts.push(address),
            Lookup::SelfDestruct(account, beneficiary) => {
                self.accounts.push(beneficiary);
                // EIP-6780: from Cancun only a contract created in the same transaction goes.
                if !interp.spec().enables(SpecId::CANCUN) || self.created.contains(&account) {
                    self.deleted.insert(account);
                }
            }
        }
    }

    fn call(
        &mut self,
        interp: &mut Interpreter<'_, '_, T>,
        message: &mut Message<T>,
    ) -> Option<MessageResult<T>> {
        self.accounts.push(message.call_target);
        // geth looks up the delegation target of the callee of a call from Prague, and that of the
        // recipient of the transaction before its authorizations apply, which `frame` takes from
        // the state before the transaction. evm2 resolves the target when it builds the message.
        if message.depth > interp.version().initial_call_depth
            && message.code_address != message.call_target
        {
            self.accounts.push(message.code_address);
        }
        None
    }

    fn create(
        &mut self,
        _interp: &mut Interpreter<'_, '_, T>,
        message: &mut Message<T>,
    ) -> Option<MessageResult<T>> {
        // The transaction of a create names the address it creates, and `frame` takes it from the
        // sender's nonce.
        self.created.insert(message.destination);
        None
    }
}

/// Returns the account as it was before the transaction: from the state of the transaction if it
/// loaded the account, otherwise from `db`.
fn original_account(
    changes: Option<AccountChanges<'_>>,
    addr: &Address,
    db: &mut dyn DynDatabase,
) -> DbResult<AccountInfo> {
    match changes {
        Some(changes) => Ok(changes.original.cloned().unwrap_or_default()),
        None => Ok(db.get_account(addr)?.unwrap_or_default()),
    }
}

/// Returns the address an EIP-7702 delegation designator delegates to.
fn parse_delegation(code: &[u8]) -> Option<Address> {
    code.strip_prefix(&[0xef, 0x01, 0x00]).filter(|rest| rest.len() == 20).map(Address::from_slice)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::address;
    use evm2::{
        AccountInfo, DatabaseError,
        bytecode::Bytecode,
        evm::{CacheDB, EmptyDB},
        interpreter::Word,
    };

    struct FailingDb {
        error: DatabaseError,
    }

    impl DynDatabase for FailingDb {
        fn get_account(&mut self, _address: &Address) -> DbResult<Option<AccountInfo>> {
            Err(self.error.clone())
        }

        fn get_code_by_hash(&mut self, _code_hash: &B256) -> DbResult<Bytecode> {
            Err(self.error.clone())
        }

        fn get_storage(&mut self, _address: &Address, _key: &Word) -> DbResult<Word> {
            Err(self.error.clone())
        }

        fn get_block_hash(&mut self, _number: &Word) -> DbResult<B256> {
            Err(self.error.clone())
        }
    }

    fn diff_inspector() -> PrestateInspector {
        PrestateInspector::new(PreStateConfig { diff_mode: Some(true), ..Default::default() })
    }

    #[test]
    fn diff_keeps_prefunded_accounts() {
        let mut state = PendingState::default();
        let prefunded_addr = address!("1000000000000000000000000000000000000001");
        let empty_addr = address!("2000000000000000000000000000000000000002");

        state.insert_account(
            prefunded_addr,
            Some(AccountInfo::default().with_balance(U256::from(10))),
            Some(AccountInfo::default().with_balance(U256::from(1)).with_nonce(1)),
        );
        state.insert_account(empty_addr, None, Some(AccountInfo::default().with_nonce(1)));

        let mut db = CacheDB::new(EmptyDB::default());
        let accounts = AddressSet::from_iter([prefunded_addr, empty_addr]);
        let PreStateFrame::Diff(diff) =
            diff_inspector().diff_frame(&accounts, &state, &mut db).unwrap()
        else {
            panic!("expected diff prestate frame");
        };
        assert!(diff.pre.contains_key(&prefunded_addr), "prefunded contract must remain in pre");
        assert!(
            !diff.pre.contains_key(&empty_addr),
            "contracts created on empty addresses are still filtered out"
        );
    }

    #[test]
    fn diff_loads_post_code_from_state_changes() {
        let mut state = PendingState::default();
        let address = address!("1000000000000000000000000000000000000001");
        let code = Bytecode::new_legacy(Bytes::from_static(&[op::PUSH1, 0x01, op::STOP]));
        // The changed account from the state output always holds the code.
        let account = AccountInfo::default().with_nonce(1).with_code(code.clone());
        state.insert_account(address, None, Some(account));

        let mut db = CacheDB::new(EmptyDB::default());
        let accounts = AddressSet::from_iter([address]);
        let PreStateFrame::Diff(diff) =
            diff_inspector().diff_frame(&accounts, &state, &mut db).unwrap()
        else {
            panic!("expected diff prestate frame");
        };
        assert!(!diff.post.contains_key(&Address::ZERO));
        assert_eq!(
            diff.post.get(&address).and_then(|account| account.code.as_ref()),
            Some(&code.original_bytes())
        );
    }

    #[test]
    fn propagates_db_account_errors() {
        let address = address!("1000000000000000000000000000000000000001");
        let error = DatabaseError::new(evm2::AnyError::from("test database error"), true);
        let mut db = FailingDb { error: error.clone() };

        // The transaction did not load the account, so its original state comes from `db`.
        let accounts = AddressSet::from_iter([address]);
        assert!(matches!(
            PrestateInspector::new(PreStateConfig::default()).pre_frame(
                &accounts,
                &PendingState::default(),
                &mut db
            ),
            Err(err) if err == error
        ));
    }

    #[test]
    fn pre_omits_initially_empty_accounts() {
        let absent = Address::with_last_byte(1);
        let empty = Address::with_last_byte(2);
        let funded = Address::with_last_byte(3);
        let nonce_account = Address::with_last_byte(4);
        let contract = Address::with_last_byte(5);
        let code = Bytecode::new_legacy(Bytes::from_static(&[0x00]));
        let mut db = CacheDB::new(EmptyDB::default());
        db.insert_account_info(&empty, AccountInfo::default());
        db.insert_account_info(&funded, AccountInfo::default().with_balance(U256::from(10)));
        db.insert_account_info(&nonce_account, AccountInfo::default().with_nonce(1));
        db.insert_account_info(&contract, AccountInfo::default().with_code(code.clone()));

        let mut state = PendingState::default();
        let fee = Some(AccountInfo::default().with_balance(U256::from(1)));
        state.insert_account(absent, None, fee.clone());
        state.insert_account(empty, Some(AccountInfo::default()), fee);
        // An account emptied by execution must retain its original prestate.
        let originals = [
            (funded, AccountInfo::default().with_balance(U256::from(10))),
            (nonce_account, AccountInfo::default().with_nonce(1)),
            (contract, AccountInfo::default().with_code(code.clone())),
        ];
        for (address, original) in originals {
            state.insert_account(address, Some(original), None);
        }

        let accounts = AddressSet::from_iter([absent, empty, funded, nonce_account, contract]);
        for code_enabled in [false, true] {
            let inspector = PrestateInspector::new(PreStateConfig {
                disable_code: Some(!code_enabled),
                ..Default::default()
            });
            assert_eq!(
                inspector.pre_frame(&accounts, &state, &mut db).unwrap(),
                PreStateFrame::Default(PreStateMode(BTreeMap::from([
                    (funded, AccountState::from_account_info(0, U256::from(10), None)),
                    (nonce_account, AccountState::from_account_info(1, U256::ZERO, None)),
                    (
                        contract,
                        AccountState::from_account_info(
                            0,
                            U256::ZERO,
                            code_enabled.then(|| code.original_bytes()),
                        )
                    ),
                ]))),
            );
        }
    }
}
