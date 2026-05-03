//! EVM execution host.

use crate::{
    AccountLoad, EvmConfig, SelfDestructResult,
    bytecode::Bytecode,
    env::{BlockEnv, TxEnv},
    interpreter::{Host, InstrStop, Interpreter, Message, MessageKind, SpecId, Word},
    registry::{HandlerResult, TxRegistry},
};
use alloc::vec::Vec;
use alloy_eips::eip2718::Typed2718;
use alloy_primitives::{Address, B256, Bytes, Log, U256, keccak256};
use alloy_rlp::{Encodable, Header};
use core::cmp::min;
use transaction::{EvmError, ExecutionResult, Transaction, intrinsic_gas};

pub mod config;
pub mod env;
pub mod precompile;
pub mod registry;
pub mod transaction;

mod state;
pub use state::{
    Account, AccountInfo, CacheDB, Database, InMemoryDB, JournalEntry, KECCAK_EMPTY, State,
    StorageValue, logs_hash,
};

/// Result of executing a transaction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct TxResult {
    /// Whether execution succeeded.
    pub status: bool,
    /// Gas used by execution.
    pub gas_used: u64,
}

/// EVM host and transaction dispatcher.
#[derive(Debug)]
pub struct Evm<C: EvmConfig> {
    block: BlockEnv,
    registry: TxRegistry<C::Tx, TxResult>,
    state: State<C::Database>,
    logs: Vec<Log>,
}

impl<C: EvmConfig<Database: Default>> Evm<C> {
    /// Creates an EVM with the provided transaction handler registry and hard fork specification.
    #[inline]
    pub fn new(block: BlockEnv, registry: TxRegistry<C::Tx, TxResult>) -> Self {
        Self::with_database(block, registry, C::Database::default())
    }
}

impl<C: EvmConfig> Evm<C> {
    /// Creates an EVM with the provided database.
    #[inline]
    pub fn with_database(
        block: BlockEnv,
        registry: TxRegistry<C::Tx, TxResult>,
        database: C::Database,
    ) -> Self {
        Self { block, registry, state: State::new(database), logs: Vec::new() }
    }

    /// Returns the transaction handler registry.
    pub const fn registry(&self) -> &TxRegistry<C::Tx, TxResult> {
        &self.registry
    }

    /// Returns the active hard fork specification.
    pub const fn spec_id(&self) -> SpecId {
        C::SPEC_ID
    }

    /// Returns the backing database.
    pub const fn database(&self) -> &State<C::Database> {
        &self.state
    }

    /// Returns the backing database mutably.
    pub const fn database_mut(&mut self) -> &mut State<C::Database> {
        &mut self.state
    }

    /// Returns the mutable EVM state.
    pub const fn state(&self) -> &State<C::Database> {
        &self.state
    }

    /// Returns emitted logs.
    pub fn logs(&self) -> &[Log] {
        &self.logs
    }

    /// Executes a transaction against the host state.
    pub fn execute(&mut self, tx: &Transaction) -> Result<ExecutionResult, EvmError>
    where
        C: EvmConfig<Host = Self>,
    {
        let intrinsic = intrinsic_gas(C::SPEC_ID, tx);
        if tx.gas_limit < intrinsic {
            return Err(EvmError::IntrinsicGasTooLow { required: intrinsic, got: tx.gas_limit });
        }

        let max_gas_cost = U256::from(tx.gas_limit) * tx.gas_price;
        let max_upfront = max_gas_cost.saturating_add(tx.value);
        let sender_info = self.state.account_info(tx.caller).unwrap_or_default();
        if sender_info.nonce != tx.nonce {
            return Err(EvmError::InvalidNonce { expected: sender_info.nonce, got: tx.nonce });
        }
        if sender_info.balance < max_upfront {
            return Err(EvmError::InsufficientFunds);
        }

        self.state.add_balance(tx.caller, Word::ZERO.wrapping_sub(max_gas_cost));
        self.state.increment_nonce(tx.caller);
        let execution_checkpoint = self.state.checkpoint();
        let log_checkpoint = self.logs.len();

        let stop;
        let mut output = Bytes::new();
        let mut gas_remaining = tx.gas_limit - intrinsic;

        let tx_env = TxEnv { origin: tx.caller, gas_price: tx.gas_price, ..TxEnv::default() };
        if let Some(to) = tx.to {
            if self.state.transfer(tx.caller, to, tx.value) {
                let code = self.state.get_code(to);
                let message = Message {
                    kind: MessageKind::Call,
                    depth: 0,
                    gas_limit: gas_remaining,
                    destination: to,
                    caller: tx.caller,
                    input: tx.data.clone(),
                    value: tx.value,
                    code_address: to,
                    salt: B256::ZERO,
                };
                let result = self.execute_frame(tx_env, code, message);
                stop = result.stop;
                output = result.output;
                gas_remaining = result.gas_remaining;
            } else {
                stop = InstrStop::OutOfFunds;
                gas_remaining = 0;
            }
        } else {
            let created_address = create_address(tx.caller, tx.nonce);
            let message = Message {
                kind: MessageKind::Create,
                depth: 0,
                gas_limit: gas_remaining,
                destination: created_address,
                caller: tx.caller,
                input: Bytes::new(),
                value: tx.value,
                code_address: created_address,
                salt: B256::ZERO,
            };
            let result =
                self.execute_create(tx_env, Bytecode::new_legacy(tx.data.clone()), message);
            stop = result.stop;
            output = result.output;
            gas_remaining = result.gas_remaining;
        }

        if !stop.is_success() {
            self.state.rollback(execution_checkpoint);
            self.logs.truncate(log_checkpoint);
            if stop.is_error() {
                gas_remaining = 0;
            }
        }

        let gas_used = tx.gas_limit - gas_remaining;
        self.state.add_balance(tx.caller, U256::from(gas_remaining) * tx.gas_price);
        self.state.add_balance(self.block.beneficiary, U256::from(gas_used) * tx.gas_price);
        self.state.prune_empty_accounts();

        Ok(ExecutionResult { stop, gas_used, output })
    }

    fn execute_create(&mut self, tx_env: TxEnv, bytecode: Bytecode, message: Message) -> FrameResult
    where
        C: EvmConfig<Host = Self>,
    {
        if message.depth >= Message::CALL_DEPTH_LIMIT {
            return FrameResult {
                stop: InstrStop::CallTooDeep,
                gas_remaining: message.gas_limit,
                output: Bytes::new(),
            };
        }

        let caller_nonce = self.state.account_info(message.caller).map_or(0, |info| info.nonce);
        let caller_balance =
            self.state.account_info(message.caller).map_or(Word::ZERO, |info| info.balance);
        if caller_balance < message.value {
            return FrameResult {
                stop: InstrStop::OutOfFunds,
                gas_remaining: message.gas_limit,
                output: Bytes::new(),
            };
        }

        let address = match message.kind {
            MessageKind::Create if message.depth == 0 => message.destination,
            MessageKind::Create => create_address(message.caller, caller_nonce),
            MessageKind::Create2 => {
                create2_address(message.caller, message.salt, bytecode.original_byte_slice())
            }
            _ => unreachable!("invalid create message kind"),
        };

        if message.depth > 0 {
            self.state.increment_nonce(message.caller);
        }

        let checkpoint = self.state.checkpoint();
        let log_checkpoint = self.logs.len();
        if let Err(stop) =
            self.state.create_account(message.caller, address, message.value, C::SPEC_ID)
        {
            self.state.rollback(checkpoint);
            self.logs.truncate(log_checkpoint);
            return FrameResult { stop, gas_remaining: message.gas_limit, output: Bytes::new() };
        }

        let mut create_message = message;
        create_message.destination = address;
        create_message.code_address = address;
        create_message.input = Bytes::new();
        let mut interpreter = Interpreter::new(bytecode, tx_env, create_message);
        let stop = interpreter.run::<C>(self);
        let mut gas = interpreter.gas();
        if stop.is_success() || stop.is_revert() {
            gas.set_final_refund(C::SPEC_ID.enables(SpecId::LONDON));
        }
        let output = Bytes::copy_from_slice(interpreter.output());
        let mut gas_remaining =
            min(gas.remaining().saturating_add(gas.refunded() as u64), gas.limit());

        if stop.is_success() {
            self.state.set_code(address, Bytecode::new_legacy(output.clone()));
        } else {
            self.state.rollback(checkpoint);
            self.logs.truncate(log_checkpoint);
            if stop.is_error() {
                gas_remaining = 0;
            }
        }

        FrameResult { stop, gas_remaining, output }
    }

    fn execute_frame(&mut self, tx_env: TxEnv, bytecode: Bytecode, message: Message) -> FrameResult
    where
        C: EvmConfig<Host = Self>,
    {
        if message.depth >= Message::CALL_DEPTH_LIMIT {
            return FrameResult {
                stop: InstrStop::CallTooDeep,
                gas_remaining: message.gas_limit,
                output: Bytes::new(),
            };
        }

        let checkpoint = self.state.checkpoint();
        let log_checkpoint = self.logs.len();
        if message.depth > 0
            && matches!(message.kind, MessageKind::Call)
            && !self.state.transfer(message.caller, message.destination, message.value)
        {
            return FrameResult {
                stop: InstrStop::OutOfFunds,
                gas_remaining: message.gas_limit,
                output: Bytes::new(),
            };
        }

        let mut interpreter = Interpreter::new(bytecode, tx_env, message);
        let stop = interpreter.run::<C>(self);
        let mut gas = interpreter.gas();
        if stop.is_success() || stop.is_revert() {
            gas.set_final_refund(C::SPEC_ID.enables(SpecId::LONDON));
        }
        let output = Bytes::copy_from_slice(interpreter.output());
        let mut gas_remaining =
            min(gas.remaining().saturating_add(gas.refunded() as u64), gas.limit());

        if !stop.is_success() {
            self.state.rollback(checkpoint);
            self.logs.truncate(log_checkpoint);
            if stop.is_error() {
                gas_remaining = 0;
            }
        }

        FrameResult { stop, gas_remaining, output }
    }
}

#[derive(Clone, Debug)]
struct FrameResult {
    stop: InstrStop,
    gas_remaining: u64,
    output: Bytes,
}

impl<C: EvmConfig<Tx: Typed2718>> Evm<C> {
    /// Dispatches the transaction to the handler registered for its EIP-2718 type byte.
    pub fn transact(&self, tx: &C::Tx) -> HandlerResult<TxResult> {
        self.registry.try_get_by_type(tx.ty())?.call(tx)
    }

    /// Dispatches each transaction to its registered EIP-2718 handler.
    pub fn transact_iter<'a, I>(
        &'a self,
        txs: I,
    ) -> impl Iterator<Item = HandlerResult<TxResult>> + 'a
    where
        I: IntoIterator<Item = &'a C::Tx>,
        I::IntoIter: 'a,
        C::Tx: 'a,
        Self: 'a,
    {
        txs.into_iter().map(move |tx| self.transact(tx))
    }
}

impl<C: EvmConfig<Host = Self>> Host for Evm<C> {
    fn block_env(&mut self) -> &BlockEnv {
        &self.block
    }

    fn load_account(
        &mut self,
        address: Address,
        load_code: bool,
        _skip_cold_load: bool,
    ) -> Result<AccountLoad, InstrStop> {
        let info = self.state.account_info(address).unwrap_or_default();
        Ok(AccountLoad {
            balance: info.balance,
            code_hash: if info.is_empty() { B256::ZERO } else { info.code_hash },
            code: if load_code {
                self.state.get_code(address).original_bytes()
            } else {
                Bytes::new()
            },
            is_empty: info.is_empty(),
            is_cold: false,
        })
    }

    fn block_hash(&mut self, number: u64) -> Option<B256> {
        self.state.initial().get_block_hash(number)
    }

    fn sload(&mut self, address: Address, key: Word) -> Word {
        self.state.storage(address, key)
    }

    fn sstore(&mut self, address: Address, key: Word, value: Word) {
        self.state.set_storage(address, key, value);
    }

    fn tload(&mut self, address: Address, key: Word) -> Word {
        self.state.transient_storage(address, key)
    }

    fn tstore(&mut self, address: Address, key: Word, value: Word) {
        self.state.set_transient_storage(address, key, value);
    }

    fn log(&mut self, log: Log) {
        self.logs.push(log);
    }

    fn execute_message(
        &mut self,
        tx_env: TxEnv,
        bytecode: Bytecode,
        message: Message,
    ) -> Result<Word, InstrStop> {
        let is_create = matches!(message.kind, MessageKind::Create | MessageKind::Create2);
        let address = if is_create {
            match message.kind {
                MessageKind::Create => {
                    let nonce =
                        self.state.account_info(message.caller).map_or(0, |info| info.nonce);
                    create_address(message.caller, nonce)
                }
                MessageKind::Create2 => {
                    create2_address(message.caller, message.salt, bytecode.original_byte_slice())
                }
                _ => unreachable!("checked above"),
            }
        } else {
            Address::ZERO
        };
        let result = if is_create {
            self.execute_create(tx_env, bytecode, message)
        } else {
            self.execute_frame(tx_env, bytecode, message)
        };
        if result.stop.is_success() {
            return Ok(if is_create { Word::from_be_slice(address.as_slice()) } else { Word::ONE });
        }
        Err(result.stop)
    }

    fn selfdestruct(
        &mut self,
        contract: Address,
        target: Address,
        _skip_cold_load: bool,
    ) -> Result<SelfDestructResult, InstrStop> {
        let target_exists = self.state.account_info(target).is_some_and(|info| !info.is_empty());
        let previously_destroyed =
            self.state.account_ref(contract).is_some_and(|account| account.destructed);
        let balance = self.state.account_info(contract).map_or(Word::ZERO, |info| info.balance);

        if contract != target && !balance.is_zero() {
            self.state.transfer(contract, target, balance);
        }

        let previous = self.state.get_or_insert(contract).balance;
        self.state.journal.push(JournalEntry::BalanceChange { address: contract, previous });
        self.state.get_or_insert(contract).balance = Word::ZERO;
        self.state.mark_destructed(contract);

        Ok(SelfDestructResult {
            had_value: !balance.is_zero(),
            target_exists,
            is_cold: false,
            previously_destroyed,
        })
    }
}

fn create_address(caller: Address, nonce: u64) -> Address {
    let mut out = Vec::new();
    Header { list: true, payload_length: caller.length() + nonce.length() }.encode(&mut out);
    caller.encode(&mut out);
    nonce.encode(&mut out);
    Address::from_slice(&keccak256(out)[12..])
}

fn create2_address(caller: Address, salt: B256, initcode: &[u8]) -> Address {
    let mut input = Vec::with_capacity(85);
    input.push(0xff);
    input.extend_from_slice(caller.as_slice());
    input.extend_from_slice(salt.as_slice());
    input.extend_from_slice(keccak256(initcode).as_slice());
    Address::from_slice(&keccak256(input)[12..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        EvmVersion,
        bytecode::Bytecode,
        interpreter::{MessageKind, op},
        registry::TxRequest,
    };
    use alloy_primitives::{Address, B256, Bytes, Log, LogData, U256, keccak256};

    const TEST_TX_TYPE: u8 = 0x7f;

    #[derive(Debug)]
    struct TestTx {
        value: u64,
    }

    impl Typed2718 for TestTx {
        fn ty(&self) -> u8 {
            TEST_TX_TYPE
        }
    }

    fn extract_test_tx(tx: &TestTx) -> Option<&TestTx> {
        Some(tx)
    }

    fn handle_test_tx(req: TxRequest<'_, TestTx>) -> HandlerResult<TxResult> {
        Ok(TxResult { status: true, gas_used: req.tx.value + 1 })
    }

    #[test]
    fn dispatches_transaction_by_typed_2718_type() {
        let registry =
            TxRegistry::new().with_handler(TEST_TX_TYPE, extract_test_tx, handle_test_tx);
        let evm = Evm::<EvmVersion<TestTx>>::new(BlockEnv::default(), registry);
        let tx = TestTx { value: 41 };

        assert_eq!(evm.transact(&tx).map(|result| result.gas_used), Ok(42));
    }

    #[test]
    fn dispatches_transaction_iter() {
        let registry =
            TxRegistry::new().with_handler(TEST_TX_TYPE, extract_test_tx, handle_test_tx);
        let evm = Evm::<EvmVersion<TestTx>>::new(BlockEnv::default(), registry);
        let txs = [TestTx { value: 1 }, TestTx { value: 2 }];
        let gas_used = evm
            .transact_iter(&txs)
            .map(|result| result.map(|result| result.gas_used))
            .collect::<HandlerResult<Vec<_>>>();

        assert_eq!(gas_used, Ok(vec![2, 3]));
    }

    #[test]
    fn runs_interpreter_with_message() {
        let mut evm = Evm::<EvmVersion<TestTx>>::new(BlockEnv::default(), TxRegistry::new());
        let bytecode = Bytecode::new_legacy(Bytes::from_static(&[op::ADDRESS, op::STOP]));
        let destination = Address::from([0x11; 20]);
        let message = Message {
            kind: MessageKind::Call,
            gas_limit: 10_000,
            destination,
            code_address: destination,
            value: U256::ZERO,
            ..Message::default()
        };

        let result = Host::execute_message(&mut evm, TxEnv::default(), bytecode, message);

        assert_eq!(result, Ok(Word::from(1)));
    }

    #[test]
    fn host_loads_accounts_from_database() {
        let address = Address::from([0x22; 20]);
        let code = Bytecode::new_legacy(Bytes::from_static(&[op::ADDRESS, op::STOP]));
        let mut info = AccountInfo { balance: Word::from(0xbeef), nonce: 1, ..Default::default() };
        info.set_code(code.clone());
        let mut database = InMemoryDB::default();
        database.insert_account_info(address, info);
        let mut evm = Evm::<EvmVersion<TestTx>>::with_database(
            BlockEnv::default(),
            TxRegistry::new(),
            database,
        );

        let load = Host::load_account(&mut evm, address, true, false).unwrap();

        assert_eq!(load.balance, Word::from(0xbeef));
        assert_eq!(load.code_hash, code.hash_slow());
        assert_eq!(load.code, code.original_bytes());
        assert!(!load.is_empty);
        assert!(!load.is_cold);
    }

    #[test]
    fn host_uses_database_block_hashes() {
        let mut database = InMemoryDB::default();
        database.insert_block_hash(42, B256::with_last_byte(0x42));
        let mut evm = Evm::<EvmVersion<TestTx>>::with_database(
            BlockEnv::default(),
            TxRegistry::new(),
            database,
        );

        assert_eq!(Host::block_hash(&mut evm, 42), Some(B256::with_last_byte(0x42)));
        assert_eq!(Host::block_hash(&mut evm, 43), Some(keccak256(b"43")));
    }

    #[test]
    fn host_stores_persistent_storage_for_current_account() {
        let address = Address::from([0x33; 20]);
        let mut evm = Evm::<EvmVersion<TestTx>>::new(BlockEnv::default(), TxRegistry::new());

        Host::sstore(&mut evm, address, Word::from(1), Word::from(0xcafe));

        assert_eq!(Host::sload(&mut evm, address, Word::from(1)), Word::from(0xcafe));
        assert_eq!(
            evm.database().account_ref(address).unwrap().storage.get(&Word::from(1)),
            Some(&StorageValue { current: Word::from(0xcafe), original: Word::ZERO })
        );
    }

    #[test]
    fn host_storage_tracks_previous_and_original_values() {
        let address = Address::from([0x34; 20]);
        let mut database = InMemoryDB::default();
        database.insert_account_storage(address, Word::from(1), Word::from(10));
        let mut evm = Evm::<EvmVersion<TestTx>>::with_database(
            BlockEnv::default(),
            TxRegistry::new(),
            database,
        );

        Host::sstore(&mut evm, address, Word::from(1), Word::from(20));
        Host::sstore(&mut evm, address, Word::from(1), Word::from(30));

        assert_eq!(
            evm.database().account_ref(address).unwrap().storage.get(&Word::from(1)),
            Some(&StorageValue { current: Word::from(30), original: Word::from(10) })
        );
    }

    #[test]
    fn host_stores_transient_storage_for_current_account() {
        let address = Address::from([0x44; 20]);
        let other = Address::from([0x45; 20]);
        let mut evm = Evm::<EvmVersion<TestTx>>::new(BlockEnv::default(), TxRegistry::new());

        Host::tstore(&mut evm, address, Word::from(1), Word::from(0xabcd));
        assert_eq!(Host::tload(&mut evm, address, Word::from(1)), Word::from(0xabcd));
        assert_eq!(Host::tload(&mut evm, other, Word::from(1)), Word::ZERO);
    }

    #[test]
    fn host_records_logs() {
        let mut evm = Evm::<EvmVersion<TestTx>>::new(BlockEnv::default(), TxRegistry::new());
        let log = Log {
            address: Address::from([0x55; 20]),
            data: LogData::new_unchecked(
                vec![B256::with_last_byte(1)],
                Bytes::from_static(&[1, 2]),
            ),
        };

        Host::log(&mut evm, log.clone());

        assert_eq!(evm.logs(), [log]);
    }

    #[test]
    fn host_selfdestruct_transfers_balance_and_marks_account() {
        let contract = Address::from([0x66; 20]);
        let target = Address::from([0x77; 20]);
        let mut database = InMemoryDB::default();
        database.insert_account_info(
            contract,
            AccountInfo { balance: Word::from(100), ..Default::default() },
        );
        database.insert_account_info(
            target,
            AccountInfo { balance: Word::from(1), nonce: 1, ..Default::default() },
        );
        let mut evm = Evm::<EvmVersion<TestTx>>::with_database(
            BlockEnv::default(),
            TxRegistry::new(),
            database,
        );

        let result = Host::selfdestruct(&mut evm, contract, target, false).unwrap();

        assert_eq!(
            result,
            SelfDestructResult {
                had_value: true,
                target_exists: true,
                is_cold: false,
                previously_destroyed: false,
            }
        );
        assert_eq!(evm.database().account_info(contract).unwrap().balance, Word::ZERO);
        assert!(evm.database().account_ref(contract).unwrap().destructed);
        assert_eq!(evm.database().account_info(target).unwrap().balance, Word::from(101));
    }

    #[test]
    fn execute_commits_value_transfer_and_storage() {
        let caller = Address::from([0xaa; 20]);
        let contract = Address::from([0xbb; 20]);
        let mut database = InMemoryDB::default();
        database.insert_account_info(
            caller,
            AccountInfo { balance: U256::from(1_000_000), ..Default::default() },
        );
        database.insert_account_info(
            contract,
            AccountInfo::default().with_code(Bytecode::new_legacy(Bytes::from_static(&[
                op::PUSH1,
                0x02,
                op::PUSH1,
                0x01,
                op::SSTORE,
                op::STOP,
            ]))),
        );
        let mut evm = Evm::<EvmVersion<(), { SpecId::FRONTIER as u8 }>>::with_database(
            BlockEnv::default(),
            TxRegistry::new(),
            database,
        );

        let result = evm.execute(&Transaction {
            caller,
            to: Some(contract),
            gas_limit: 100_000,
            gas_price: U256::ONE,
            value: U256::from(7),
            ..Transaction::default()
        });

        assert!(result.unwrap().is_success());
        assert_eq!(evm.state().account_info(contract).unwrap().balance, U256::from(7));
        assert_eq!(
            evm.state()
                .account_ref(contract)
                .unwrap()
                .storage
                .get(&U256::from(1))
                .map(|value| value.current),
            Some(U256::from(2))
        );
    }

    #[test]
    fn execute_reverts_frame_state_and_logs() {
        let caller = Address::from([0xaa; 20]);
        let contract = Address::from([0xbb; 20]);
        let mut database = InMemoryDB::default();
        database.insert_account_info(
            caller,
            AccountInfo { balance: U256::from(1_000_000), ..Default::default() },
        );
        database.insert_account_info(
            contract,
            AccountInfo::default().with_code(Bytecode::new_legacy(Bytes::from_static(&[
                op::PUSH1,
                0x02,
                op::PUSH1,
                0x01,
                op::SSTORE,
                op::PUSH0,
                op::PUSH0,
                op::LOG0,
                op::PUSH0,
                op::PUSH0,
                op::REVERT,
            ]))),
        );
        let mut evm = Evm::<EvmVersion<(), { SpecId::CANCUN as u8 }>>::with_database(
            BlockEnv::default(),
            TxRegistry::new(),
            database,
        );

        let result = evm.execute(&Transaction {
            caller,
            to: Some(contract),
            gas_limit: 100_000,
            gas_price: U256::ONE,
            ..Transaction::default()
        });

        assert_eq!(result.unwrap().stop, InstrStop::Revert);
        let caller_info = evm.state().account_info(caller).unwrap();
        assert_eq!(caller_info.nonce, 1);
        assert!(caller_info.balance < U256::from(1_000_000));
        assert_eq!(evm.state().account_info(contract).unwrap().balance, U256::ZERO);
        assert_eq!(
            evm.state()
                .account_ref(contract)
                .and_then(|account| account.storage.get(&U256::from(1)).map(|value| value.current)),
            Some(U256::ZERO)
        );
        assert_eq!(logs_hash(evm.logs()), logs_hash(&[]));
    }

    #[test]
    fn execute_create_deploys_code() {
        let caller = Address::from([0xaa; 20]);
        let mut database = InMemoryDB::default();
        database.insert_account_info(
            caller,
            AccountInfo { balance: U256::from(1_000_000), ..Default::default() },
        );
        let mut evm = Evm::<EvmVersion<(), { SpecId::CANCUN as u8 }>>::with_database(
            BlockEnv::default(),
            TxRegistry::new(),
            database,
        );
        let initcode = Bytes::from_static(&[
            op::PUSH1,
            0x42,
            op::PUSH0,
            op::MSTORE8,
            op::PUSH1,
            0x01,
            op::PUSH0,
            op::RETURN,
        ]);

        let result = evm.execute(&Transaction {
            caller,
            to: None,
            gas_limit: 100_000,
            gas_price: U256::ONE,
            data: initcode,
            ..Transaction::default()
        });

        assert!(result.unwrap().is_success());
        let caller_info = evm.state().account_info(caller).unwrap();
        assert_eq!(caller_info.nonce, 1);
        assert!(caller_info.balance < U256::from(1_000_000));
        let created = create_address(caller, 0);
        let code = evm.state.get_code(created);
        assert_eq!(code.original_byte_slice(), &[0x42]);
    }
}
