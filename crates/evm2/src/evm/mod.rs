//! EVM execution host.

use crate::{
    AccountLoad, EvmConfig, SelfDestructResult,
    bytecode::Bytecode,
    env::{BlockEnv, TxEnv},
    interpreter::{Host, InstrStop, Interpreter, Message, SpecId, Word},
    registry::{HandlerResult, TxRegistry},
};
use alloy_eips::eip2718::Typed2718;
use alloy_primitives::{Address, B256, Log};

pub mod config;
pub mod env;
pub mod registry;

mod state;
pub use state::{Account, Database, MemoryDb};

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
pub struct Evm<C: EvmConfig, DB = MemoryDb> {
    block: BlockEnv,
    registry: TxRegistry<C::Tx, TxResult>,
    database: DB,
    current_address: Address,
}

impl<C: EvmConfig> Evm<C> {
    /// Creates an EVM with the provided transaction handler registry and hard fork specification.
    #[inline]
    pub fn new(block: BlockEnv, registry: TxRegistry<C::Tx, TxResult>) -> Self {
        Self::with_database(block, registry, MemoryDb::default())
    }
}

impl<C: EvmConfig, DB> Evm<C, DB> {
    /// Creates an EVM with the provided database.
    #[inline]
    pub const fn with_database(
        block: BlockEnv,
        registry: TxRegistry<C::Tx, TxResult>,
        database: DB,
    ) -> Self {
        Self { block, registry, database, current_address: Address::ZERO }
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
    pub const fn database(&self) -> &DB {
        &self.database
    }

    /// Returns the backing database mutably.
    pub const fn database_mut(&mut self) -> &mut DB {
        &mut self.database
    }

    /// Returns the active message destination used for implicit storage host calls.
    pub const fn current_address(&self) -> Address {
        self.current_address
    }
}

impl<C: EvmConfig<Tx: Typed2718>, DB> Evm<C, DB> {
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

impl<C: EvmConfig<Host = Self>, DB: Database> Host for Evm<C, DB> {
    fn block_env(&mut self) -> &BlockEnv {
        &self.block
    }

    fn load_account(
        &mut self,
        address: Word,
        _load_code: bool,
        _skip_cold_load: bool,
    ) -> Result<AccountLoad, InstrStop> {
        // TODO: revm can return ColdLoadSkipped when `skip_cold_load` is true. This host does
        // not track access lists yet, so every load is treated as warm.
        let address = Address::from_word(B256::from(address.to_be_bytes::<32>()));
        let account = self.database.account(address).unwrap_or_default();
        Ok(AccountLoad {
            balance: account.balance,
            code_hash: if account.is_empty() { B256::ZERO } else { account.code_hash },
            code: account.code.original_bytes(),
            is_empty: account.is_empty(),
            is_cold: false,
        })
    }

    fn block_hash(&mut self, number: u64) -> Option<B256> {
        self.database.block_hash(number)
    }

    fn sload(&mut self, index: Word) -> Word {
        self.database
            .account(self.current_address)
            .and_then(|account| account.storage.get(&index).copied())
            .unwrap_or_default()
    }

    fn sstore(&mut self, index: Word, value: Word) {
        // TODO: revm records original slot values, refunds, and warm/cold status in its journal.
        self.database.account_mut(self.current_address).storage.insert(index, value);
    }

    fn tload(&mut self, index: Word) -> Word {
        self.database.tload(self.current_address, index)
    }

    fn tstore(&mut self, index: Word, value: Word) {
        self.database.tstore(self.current_address, index, value);
    }

    fn log(&mut self, log: Log) {
        self.database.log(log);
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
        let target_exists =
            self.database.account(target).is_some_and(|account| !account.is_empty());
        let previously_destroyed =
            self.database.account(contract).is_some_and(|account| account.selfdestructed);
        let balance = self.database.account(contract).map_or(Word::ZERO, |account| account.balance);

        if contract != target && !balance.is_zero() {
            let target_account = self.database.account_mut(target);
            target_account.balance = target_account.balance.saturating_add(balance);
        }

        let contract_account = self.database.account_mut(contract);
        contract_account.balance = Word::ZERO;
        contract_account.selfdestructed = true;

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
    use alloy_primitives::{Address, B256, Bytes, Log, LogData, U256};

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
        let mut account = Account { balance: Word::from(0xbeef), nonce: 1, ..Default::default() };
        account.set_code(code.clone());
        let mut database = MemoryDb::default();
        database.insert_account(address, account);
        let mut evm = Evm::<EvmVersion<TestTx>>::with_database(
            BlockEnv::default(),
            TxRegistry::new(),
            database,
        );

        let load = Host::load_account(&mut evm, address.into_word().into(), true, false).unwrap();

        assert_eq!(load.balance, Word::from(0xbeef));
        assert_eq!(load.code_hash, code.hash_slow());
        assert_eq!(load.code, code.original_bytes());
        assert!(!load.is_empty);
        assert!(!load.is_cold);
    }

    #[test]
    fn host_uses_database_block_hashes() {
        let mut database = MemoryDb::default();
        database.insert_block_hash(42, B256::with_last_byte(0x42));
        let mut evm = Evm::<EvmVersion<TestTx>>::with_database(
            BlockEnv::default(),
            TxRegistry::new(),
            database,
        );

        assert_eq!(Host::block_hash(&mut evm, 42), Some(B256::with_last_byte(0x42)));
        assert_eq!(Host::block_hash(&mut evm, 43), None);
    }

    #[test]
    fn host_stores_persistent_storage_for_current_account() {
        let address = Address::from([0x33; 20]);
        let mut evm = Evm::<EvmVersion<TestTx>>::new(BlockEnv::default(), TxRegistry::new());
        evm.current_address = address;

        Host::sstore(&mut evm, Word::from(1), Word::from(0xcafe));

        assert_eq!(Host::sload(&mut evm, Word::from(1)), Word::from(0xcafe));
        assert_eq!(
            evm.database()
                .account_ref(address)
                .and_then(|account| account.storage.get(&Word::from(1))),
            Some(&Word::from(0xcafe))
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

        assert_eq!(evm.database().logs, [log]);
    }

    #[test]
    fn host_selfdestruct_transfers_balance_and_marks_account() {
        let contract = Address::from([0x66; 20]);
        let target = Address::from([0x77; 20]);
        let mut database = MemoryDb::default();
        database
            .insert_account(contract, Account { balance: Word::from(100), ..Default::default() });
        database.insert_account(
            target,
            Account { balance: Word::from(1), nonce: 1, ..Default::default() },
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
        assert_eq!(evm.database().account_ref(contract).unwrap().balance, Word::ZERO);
        assert!(evm.database().account_ref(contract).unwrap().selfdestructed);
        assert_eq!(evm.database().account_ref(target).unwrap().balance, Word::from(101));
    }
}
