//! EVM execution host.

use crate::{
    AccountLoad, EvmConfig, SelfDestructResult,
    bytecode::Bytecode,
    env::{BlockEnv, TxEnv},
    interpreter::{Host, InstrStop, Interpreter, Message, SpecId, Word},
    registry::{HandlerResult, TxRegistry},
};
use alloc::vec::Vec;
use alloy_eips::eip2718::Typed2718;
use alloy_primitives::{
    Address, B256, Log,
    map::{self, HashMap},
};

pub mod config;
pub mod env;
pub mod registry;

mod state;
pub use state::{
    Account, AccountInfo, AccountState, Cache, CacheDB, Database, InMemoryDB, KECCAK_EMPTY,
    StorageSlot,
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
    database: C::Database,
    transient_storage: HashMap<(Address, Word), Word>,
    logs: Vec<Log>,
    current_address: Address,
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
        Self {
            block,
            registry,
            database,
            transient_storage: map::HashMap::default(),
            logs: Vec::new(),
            current_address: Address::ZERO,
        }
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
    pub const fn database(&self) -> &C::Database {
        &self.database
    }

    /// Returns the backing database mutably.
    pub const fn database_mut(&mut self) -> &mut C::Database {
        &mut self.database
    }

    /// Returns the active message destination used for implicit storage host calls.
    pub const fn current_address(&self) -> Address {
        self.current_address
    }

    /// Returns transient storage.
    pub const fn transient_storage(&self) -> &HashMap<(Address, Word), Word> {
        &self.transient_storage
    }

    /// Returns emitted logs.
    pub fn logs(&self) -> &[Log] {
        &self.logs
    }
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
        _load_code: bool,
        _skip_cold_load: bool,
    ) -> Result<AccountLoad, InstrStop> {
        // TODO: revm can return ColdLoadSkipped when `skip_cold_load` is true. This host does
        // not track access lists yet, so every load is treated as warm.
        let info = self.database.basic(address).unwrap_or_default();
        Ok(AccountLoad {
            balance: info.balance,
            code_hash: if info.is_empty() { B256::ZERO } else { info.code_hash },
            code: info
                .code
                .clone()
                .unwrap_or_else(|| self.database.code_by_hash(info.code_hash))
                .original_bytes(),
            is_empty: info.is_empty(),
            is_cold: false,
        })
    }

    fn block_hash(&mut self, number: u64) -> Option<B256> {
        self.database.block_hash(number)
    }

    fn sload(&mut self, index: Word) -> Word {
        self.database.storage(self.current_address, index)
    }

    fn sstore(&mut self, index: Word, value: Word) {
        // TODO: revm records refunds and warm/cold status in its journal.
        self.database.set_storage(self.current_address, index, value);
    }

    fn tload(&mut self, index: Word) -> Word {
        self.transient_storage.get(&(self.current_address, index)).copied().unwrap_or_default()
    }

    fn tstore(&mut self, index: Word, value: Word) {
        self.transient_storage.insert((self.current_address, index), value);
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
        let current_address = core::mem::replace(&mut self.current_address, message.destination);
        let stop = execute_message_with_host::<C>(self, bytecode, tx_env, message);
        self.current_address = current_address;
        if matches!(stop, InstrStop::Stop | InstrStop::Return) {
            return Ok(Word::from(1));
        }
        Err(stop)
    }

    fn selfdestruct(
        &mut self,
        contract: Address,
        target: Address,
        _skip_cold_load: bool,
    ) -> Result<SelfDestructResult, InstrStop> {
        // TODO: revm journals selfdestruct so it can be reverted, tracks cold target access, and
        // applies post-Cancun created-in-tx cleanup rules. This only marks state and transfers
        // balance in-place.
        let target_exists = self.database.basic(target).is_some_and(|info| !info.is_empty());
        let previously_destroyed = self.database.is_selfdestructed(contract);
        let balance = self.database.basic(contract).map_or(Word::ZERO, |info| info.balance);

        if contract != target && !balance.is_zero() {
            let mut target_info = self.database.basic(target).unwrap_or_default();
            target_info.balance = target_info.balance.saturating_add(balance);
            self.database.insert_account_info(target, target_info);
        }

        let mut contract_info = self.database.basic(contract).unwrap_or_default();
        contract_info.balance = Word::ZERO;
        self.database.insert_account_info(contract, contract_info);
        self.database.mark_selfdestructed(contract);

        Ok(SelfDestructResult {
            had_value: !balance.is_zero(),
            target_exists,
            is_cold: false,
            previously_destroyed,
        })
    }
}

fn execute_message_with_host<C: EvmConfig>(
    host: &mut C::Host,
    bytecode: Bytecode,
    tx_env: TxEnv,
    message: Message,
) -> InstrStop {
    let mut interpreter = Interpreter::new(bytecode, tx_env, message);
    interpreter.run::<C>(host)
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
        evm.current_address = address;

        Host::sstore(&mut evm, Word::from(1), Word::from(0xcafe));

        assert_eq!(Host::sload(&mut evm, Word::from(1)), Word::from(0xcafe));
        assert_eq!(
            evm.database().cache.storage.get(&(address, Word::from(1))),
            Some(&StorageSlot::new_changed(Word::ZERO, Word::from(0xcafe)))
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
        evm.current_address = address;

        Host::sstore(&mut evm, Word::from(1), Word::from(20));
        Host::sstore(&mut evm, Word::from(1), Word::from(30));

        assert_eq!(
            evm.database().cache.storage.get(&(address, Word::from(1))),
            Some(&StorageSlot {
                original_value: Word::from(10),
                previous_value: Word::from(20),
                present_value: Word::from(30),
            })
        );
    }

    #[test]
    fn host_stores_transient_storage_for_current_account() {
        let address = Address::from([0x44; 20]);
        let other = Address::from([0x45; 20]);
        let mut evm = Evm::<EvmVersion<TestTx>>::new(BlockEnv::default(), TxRegistry::new());
        evm.current_address = address;

        Host::tstore(&mut evm, Word::from(1), Word::from(0xabcd));
        assert_eq!(Host::tload(&mut evm, Word::from(1)), Word::from(0xabcd));

        evm.current_address = other;
        assert_eq!(Host::tload(&mut evm, Word::from(1)), Word::ZERO);
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
        assert!(evm.database().account_ref(contract).unwrap().is_selfdestructed());
        assert_eq!(evm.database().account_info(target).unwrap().balance, Word::from(101));
    }
}
