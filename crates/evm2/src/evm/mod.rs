//! EVM execution host.

use self::precompile::{PrecompileOutput, PrecompileProvider};
use crate::{
    AccountLoad, BaseEvmConfig, EvmTypes, SelfDestructResult, StorageLoad, Version,
    bytecode::Bytecode,
    env::{BlockEnv, TxEnv},
    interpreter::{
        Host, InstrStop, Interpreter, Message, MessageKind, MessageResult, SpecId, Word,
    },
    registry::{HandlerResult, TxRegistry},
};
use alloc::vec::Vec;
use alloy_eips::eip2718::Typed2718;
use alloy_primitives::{Address, B256, Bytes, Log};
use core::cmp::min;

pub mod config;
pub mod env;
pub mod precompile;
pub mod registry;

mod state;
pub use state::{
    Account, AccountInfo, CacheDB, Database, InMemoryDB, JournalEntry, KECCAK_EMPTY, State,
    StorageValue, logs_hash,
};

/// Result of executing a transaction.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TxResult {
    /// Whether execution succeeded.
    pub status: bool,
    /// Gas used by execution.
    pub gas_used: u64,
    /// Interpreter stop reason.
    pub stop: InstrStop,
    /// Return or revert output.
    pub output: Bytes,
}

type RunInterpreterFn<T> = fn(&mut Interpreter<T>, &mut <T as EvmTypes>::Host) -> InstrStop;

/// EVM host and transaction dispatcher.
#[derive(Debug)]
pub struct Evm<T: EvmTypes> {
    version: &'static Version,
    run_interpreter: RunInterpreterFn<T>,
    pub(crate) block: BlockEnv,
    registry: TxRegistry<T::Tx, TxResult, Self>,
    pub(crate) state: State<T::Database>,
    precompiles: T::Precompiles,
    pub(crate) logs: Vec<Log>,
}

impl<T: EvmTypes> Evm<T> {
    /// Creates an EVM for the default hard fork with the provided transaction registry,
    /// database, and precompile provider.
    #[inline]
    pub fn new(
        block: BlockEnv,
        registry: TxRegistry<T::Tx, TxResult, Self>,
        database: T::Database,
        precompiles: T::Precompiles,
    ) -> Self {
        Self::new_with_spec(SpecId::OSAKA, block, registry, database, precompiles)
    }

    /// Creates an EVM for `spec_id` with the provided transaction registry, database, and
    /// precompile provider.
    #[inline]
    pub fn new_with_spec(
        spec_id: SpecId,
        block: BlockEnv,
        registry: TxRegistry<T::Tx, TxResult, Self>,
        database: T::Database,
        precompiles: T::Precompiles,
    ) -> Self {
        Self::new_with_version(Version::base(spec_id), block, registry, database, precompiles)
    }

    /// Creates an EVM with the provided transaction registry, database, and precompile provider.
    #[inline]
    pub fn new_with_version(
        version: &'static Version,
        block: BlockEnv,
        registry: TxRegistry<T::Tx, TxResult, Self>,
        database: T::Database,
        precompiles: T::Precompiles,
    ) -> Self {
        Self {
            version,
            run_interpreter: run_interpreter_for_spec::<T>(version.spec_id()),
            block,
            registry,
            state: State::new(database),
            precompiles,
            logs: Vec::new(),
        }
    }

    /// Returns the transaction handler registry.
    pub const fn registry(&self) -> &TxRegistry<T::Tx, TxResult, Self> {
        &self.registry
    }

    /// Returns the backing database.
    pub const fn database(&self) -> &State<T::Database> {
        &self.state
    }

    /// Returns the mutable EVM state.
    pub const fn state(&self) -> &State<T::Database> {
        &self.state
    }

    /// Returns emitted logs.
    pub fn logs(&self) -> &[Log] {
        &self.logs
    }

    /// Returns the precompile provider.
    pub const fn precompiles(&self) -> &T::Precompiles {
        &self.precompiles
    }

    /// Returns the precompile provider mutably.
    pub const fn precompiles_mut(&mut self) -> &mut T::Precompiles {
        &mut self.precompiles
    }

    /// Returns the active EVM version.
    pub const fn version(&self) -> &'static Version {
        self.version
    }

    /// Returns the active hard fork specification.
    pub const fn spec_id(&self) -> SpecId {
        self.version.spec_id()
    }

    #[inline]
    fn execute_precompile(
        &mut self,
        message: &Message,
    ) -> Option<Result<PrecompileOutput, InstrStop>> {
        self.precompiles.execute(message.code_address, &message.input, message.gas_limit)
    }
}

impl<T: EvmTypes<Tx: Typed2718>> Evm<T> {
    /// Dispatches the transaction to the handler registered for its EIP-2718 type byte.
    pub fn transact(&mut self, tx: &T::Tx) -> HandlerResult<TxResult> {
        let handler = self.registry.try_get_by_type(tx.ty())?;
        let result = handler.call(tx, self);
        self.state.clear_accesses();
        result
    }

    /// Dispatches each transaction to its registered EIP-2718 handler.
    pub fn transact_iter<'a, I>(
        &'a mut self,
        txs: I,
    ) -> impl Iterator<Item = HandlerResult<TxResult>> + 'a
    where
        I: IntoIterator<Item = &'a T::Tx>,
        I::IntoIter: 'a,
        T::Tx: 'a,
        Self: 'a,
    {
        txs.into_iter().map(move |tx| self.transact(tx))
    }
}

impl<T: EvmTypes<Host = Self>> Host for Evm<T> {
    fn spec_id(&self) -> SpecId {
        self.spec_id()
    }

    fn block_env(&mut self) -> &BlockEnv {
        &self.block
    }

    fn load_account(
        &mut self,
        address: Address,
        load_code: bool,
        skip_cold_load: bool,
    ) -> Result<AccountLoad, InstrStop> {
        let is_cold =
            self.spec_id().enables(SpecId::BERLIN) && !self.state.is_account_warm(address);
        if skip_cold_load && is_cold {
            return Err(InstrStop::OutOfGas);
        }
        self.state.warm_account(address);
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
            is_cold,
        })
    }

    fn block_hash(&mut self, number: u64) -> Option<B256> {
        self.state.initial().get_block_hash(number)
    }

    fn sload(&mut self, address: Address, key: Word) -> StorageLoad {
        let is_cold =
            self.spec_id().enables(SpecId::BERLIN) && self.state.warm_storage(address, key);
        StorageLoad { value: self.state.storage(address, key), is_cold }
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
        caller_is_static: bool,
    ) -> MessageResult {
        if message.depth >= Message::CALL_DEPTH_LIMIT {
            return MessageResult {
                stop: InstrStop::CallTooDeep,
                gas_remaining: message.gas_limit,
                ..MessageResult::default()
            };
        }

        let is_create = matches!(message.kind, MessageKind::Create | MessageKind::Create2);
        if is_create {
            let caller_nonce = self.state.account_info(message.caller).map_or(0, |info| info.nonce);
            let caller_balance =
                self.state.account_info(message.caller).map_or(Word::ZERO, |info| info.balance);
            if caller_balance < message.value {
                return MessageResult {
                    stop: InstrStop::OutOfFunds,
                    gas_remaining: message.gas_limit,
                    ..MessageResult::default()
                };
            }

            let address = match message.kind {
                MessageKind::Create if message.depth == 0 => message.destination,
                MessageKind::Create => message.caller.create(caller_nonce),
                MessageKind::Create2 => message.caller.create2(message.salt, bytecode.hash_slow()),
                _ => unreachable!("invalid create message kind"),
            };

            self.state.warm_account(address);

            if message.depth > 0 {
                self.state.increment_nonce(message.caller);
            }

            let checkpoint = self.state.checkpoint();
            let log_checkpoint = self.logs.len();
            if let Err(stop) =
                self.state.create_account(message.caller, address, message.value, self.spec_id())
            {
                self.state.rollback(checkpoint);
                self.logs.truncate(log_checkpoint);
                return MessageResult {
                    stop,
                    gas_remaining: message.gas_limit,
                    ..MessageResult::default()
                };
            }

            let mut create_message = message;
            create_message.destination = address;
            create_message.code_address = address;
            create_message.input = Bytes::new();
            let mut interpreter =
                Interpreter::<T>::new(bytecode, tx_env, create_message, caller_is_static);
            let stop = (self.run_interpreter)(&mut interpreter, self);
            let mut gas = interpreter.gas();
            if stop.is_success() || stop.is_revert() {
                gas.set_final_refund(self.spec_id().enables(SpecId::LONDON));
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

            return MessageResult {
                stop,
                gas_remaining,
                output,
                created_address: stop.is_success().then_some(address),
            };
        }

        let checkpoint = self.state.checkpoint();
        let log_checkpoint = self.logs.len();
        if matches!(message.kind, MessageKind::Call)
            && !self.state.transfer(message.caller, message.destination, message.value)
        {
            return MessageResult {
                stop: InstrStop::OutOfFunds,
                gas_remaining: message.gas_limit,
                ..MessageResult::default()
            };
        }

        if let Some(result) = self.execute_precompile(&message) {
            let (stop, gas_remaining, output) = match result {
                Ok(output) if output.gas_used <= message.gas_limit => {
                    (InstrStop::Return, message.gas_limit - output.gas_used, output.output)
                }
                Ok(_) => (InstrStop::PrecompileOOG, 0, Bytes::new()),
                Err(stop) => {
                    let gas_remaining = if stop.is_error() { 0 } else { message.gas_limit };
                    (stop, gas_remaining, Bytes::new())
                }
            };
            if !stop.is_success() {
                self.state.rollback(checkpoint);
                self.logs.truncate(log_checkpoint);
            }
            return MessageResult { stop, gas_remaining, output, created_address: None };
        }

        let mut interpreter = Interpreter::<T>::new(bytecode, tx_env, message, caller_is_static);
        let stop = (self.run_interpreter)(&mut interpreter, self);
        let mut gas = interpreter.gas();
        if stop.is_success() || stop.is_revert() {
            gas.set_final_refund(self.spec_id().enables(SpecId::LONDON));
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

        MessageResult { stop, gas_remaining, output, created_address: None }
    }

    fn selfdestruct(
        &mut self,
        contract: Address,
        target: Address,
        skip_cold_load: bool,
    ) -> Result<SelfDestructResult, InstrStop> {
        // TODO: evmone applies full SELFDESTRUCT revision rules in state transition.
        let is_cold = self.spec_id().enables(SpecId::BERLIN) && !self.state.is_account_warm(target);
        if skip_cold_load && is_cold {
            return Err(InstrStop::OutOfGas);
        }
        self.state.warm_account(target);
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
            is_cold,
            previously_destroyed,
        })
    }
}

fn run_interpreter_for_spec<T: EvmTypes>(spec_id: SpecId) -> RunInterpreterFn<T> {
    macro_rules! run {
        ($spec:ident) => {
            run_interpreter::<T, { SpecId::$spec as u8 }>
        };
    }
    match spec_id {
        SpecId::FRONTIER => run!(FRONTIER),
        SpecId::HOMESTEAD => run!(HOMESTEAD),
        SpecId::TANGERINE => run!(TANGERINE),
        SpecId::SPURIOUS_DRAGON => run!(SPURIOUS_DRAGON),
        SpecId::BYZANTIUM => run!(BYZANTIUM),
        SpecId::PETERSBURG => run!(PETERSBURG),
        SpecId::ISTANBUL => run!(ISTANBUL),
        SpecId::BERLIN => run!(BERLIN),
        SpecId::LONDON => run!(LONDON),
        SpecId::MERGE => run!(MERGE),
        SpecId::SHANGHAI => run!(SHANGHAI),
        SpecId::CANCUN => run!(CANCUN),
        SpecId::PRAGUE => run!(PRAGUE),
        SpecId::OSAKA => run!(OSAKA),
        SpecId::AMSTERDAM => run!(AMSTERDAM),
    }
}

fn run_interpreter<T: EvmTypes, const SPEC_ID: u8>(
    interpreter: &mut Interpreter<T>,
    host: &mut T::Host,
) -> InstrStop {
    interpreter.run::<BaseEvmConfig<SPEC_ID>>(host)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BaseEvmTypes,
        bytecode::Bytecode,
        interpreter::{MessageKind, SpecId, op},
        registry::TxRequest,
    };
    use alloy_primitives::{Address, B256, Bytes, Log, LogData, U256, keccak256};

    const TEST_TX_TYPE: u8 = 0x7f;
    const NO_CONFIG_VERSION: &Version = Version::base(SpecId::OSAKA);

    #[derive(Debug)]
    struct TestTx {
        value: u64,
    }

    type TestEvmTypes<Tx = ()> = BaseEvmTypes<Tx>;

    #[derive(Debug)]
    struct NoConfigTypes;

    #[derive(Debug)]
    struct NoConfigHost {
        block: BlockEnv,
    }

    impl Host for NoConfigHost {
        fn spec_id(&self) -> SpecId {
            SpecId::DEFAULT
        }

        fn block_env(&mut self) -> &BlockEnv {
            &self.block
        }

        fn load_account(
            &mut self,
            _address: Address,
            _load_code: bool,
            _skip_cold_load: bool,
        ) -> Result<AccountLoad, InstrStop> {
            unreachable!("no-config transaction dispatch does not execute messages")
        }

        fn block_hash(&mut self, _number: u64) -> Option<B256> {
            unreachable!("no-config transaction dispatch does not execute messages")
        }

        fn sload(&mut self, _address: Address, _key: Word) -> StorageLoad {
            unreachable!("no-config transaction dispatch does not execute messages")
        }

        fn sstore(&mut self, _address: Address, _key: Word, _value: Word) {
            unreachable!("no-config transaction dispatch does not execute messages")
        }

        fn tload(&mut self, _address: Address, _key: Word) -> Word {
            unreachable!("no-config transaction dispatch does not execute messages")
        }

        fn tstore(&mut self, _address: Address, _key: Word, _value: Word) {
            unreachable!("no-config transaction dispatch does not execute messages")
        }

        fn log(&mut self, _log: Log) {
            unreachable!("no-config transaction dispatch does not execute messages")
        }

        fn execute_message(
            &mut self,
            _tx_env: TxEnv,
            _bytecode: Bytecode,
            _message: Message,
            _caller_is_static: bool,
        ) -> MessageResult {
            unreachable!("no-config transaction dispatch does not execute messages")
        }

        fn selfdestruct(
            &mut self,
            _contract: Address,
            _target: Address,
            _skip_cold_load: bool,
        ) -> Result<SelfDestructResult, InstrStop> {
            unreachable!("no-config transaction dispatch does not execute messages")
        }
    }

    impl EvmTypes for NoConfigTypes {
        type Tx = TestTx;
        type Host = NoConfigHost;
        type Database = InMemoryDB;
        type Precompiles = precompile::NoPrecompiles;
    }

    impl Typed2718 for TestTx {
        fn ty(&self) -> u8 {
            TEST_TX_TYPE
        }
    }

    fn extract_test_tx(tx: &TestTx) -> Option<&TestTx> {
        Some(tx)
    }

    fn handle_test_tx(
        req: TxRequest<'_, TestTx, Evm<TestEvmTypes<TestTx>>>,
    ) -> HandlerResult<TxResult> {
        let _ = req.host.spec_id();
        Ok(TxResult { status: true, gas_used: req.tx.value + 1, ..TxResult::default() })
    }

    fn handle_no_config_tx(
        req: TxRequest<'_, TestTx, Evm<NoConfigTypes>>,
    ) -> HandlerResult<TxResult> {
        Ok(TxResult { status: true, gas_used: req.tx.value + 1, ..TxResult::default() })
    }

    #[test]
    fn dispatches_transaction_by_typed_2718_type() {
        let registry =
            TxRegistry::new().with_handler(TEST_TX_TYPE, extract_test_tx, handle_test_tx);
        let mut evm = Evm::<TestEvmTypes<TestTx>>::new(
            BlockEnv::default(),
            registry,
            InMemoryDB::default(),
            Default::default(),
        );
        let tx = TestTx { value: 41 };

        assert_eq!(evm.transact(&tx).map(|result| result.gas_used), Ok(42));
    }

    #[test]
    fn dispatches_transaction_without_evm_config() {
        let registry =
            TxRegistry::new().with_handler(TEST_TX_TYPE, extract_test_tx, handle_no_config_tx);
        let mut evm = Evm::<NoConfigTypes>::new_with_version(
            NO_CONFIG_VERSION,
            BlockEnv::default(),
            registry,
            InMemoryDB::default(),
            Default::default(),
        );
        let tx = TestTx { value: 41 };

        assert_eq!(evm.transact(&tx).map(|result| result.gas_used), Ok(42));
    }

    #[test]
    fn dispatches_transaction_iter() {
        let registry =
            TxRegistry::new().with_handler(TEST_TX_TYPE, extract_test_tx, handle_test_tx);
        let mut evm = Evm::<TestEvmTypes<TestTx>>::new(
            BlockEnv::default(),
            registry,
            InMemoryDB::default(),
            Default::default(),
        );
        let txs = [TestTx { value: 1 }, TestTx { value: 2 }];
        let gas_used = evm
            .transact_iter(&txs)
            .map(|result| result.map(|result| result.gas_used))
            .collect::<HandlerResult<Vec<_>>>();

        assert_eq!(gas_used, Ok(vec![2, 3]));
    }

    #[test]
    fn host_executes_message() {
        let mut evm = Evm::<TestEvmTypes>::new(
            BlockEnv::default(),
            TxRegistry::new(),
            InMemoryDB::default(),
            Default::default(),
        );
        let contract = Address::from([0x11; 20]);
        let bytecode = Bytecode::new_legacy(Bytes::from_static(&[op::ADDRESS, op::STOP]));
        let message = Message {
            kind: MessageKind::Call,
            destination: contract,
            code_address: contract,
            gas_limit: 50_000,
            ..Message::default()
        };

        let result = Host::execute_message(&mut evm, TxEnv::default(), bytecode, message, false);
        assert!(result.stop.is_success());
    }

    #[test]
    fn logs_hash_matches_empty_logs() {
        assert_eq!(logs_hash(&[]), keccak256([alloy_rlp::EMPTY_LIST_CODE]));
    }

    #[test]
    fn logs_hash_hashes_logs() {
        let log = Log {
            address: Address::from([0x22; 20]),
            data: LogData::new_unchecked(vec![B256::with_last_byte(1)], Bytes::from_static(&[2])),
        };

        assert_ne!(logs_hash(&[log]), B256::ZERO);
    }

    #[test]
    fn account_info_with_code_sets_hash() {
        let code = Bytecode::new_legacy(Bytes::from_static(&[op::STOP]));
        let info = AccountInfo::default().with_code(code.clone());

        assert_eq!(info.code_hash, code.hash_slow());
    }

    #[test]
    fn transfer_moves_value() {
        let from = Address::from([0x01; 20]);
        let to = Address::from([0x02; 20]);
        let mut state = State::new(InMemoryDB::default());
        state.add_balance(from, U256::from(10));

        assert!(state.transfer(from, to, U256::from(7)));
        assert_eq!(
            state.account_info(from).expect("sender account should exist").balance,
            U256::from(3)
        );
        assert_eq!(
            state.account_info(to).expect("recipient account should exist").balance,
            U256::from(7)
        );
    }
}
