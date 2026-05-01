//! EVM execution host.

use crate::{
    bytecode::Bytecode,
    env::{BlockEnv, TxEnv},
    interpreter::{
        Host, InstrStop, Interpreter, Message, SpecId, Table, Word,
        table::{DEFAULT_TABLE, new_gas_table},
    },
    registry::{HandlerResult, TxRegistry},
};
use alloy_eips::eip2718::Typed2718;
use alloy_primitives::{B256, Log};

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
pub struct Evm<Tx> {
    block: BlockEnv,
    registry: TxRegistry<Tx, TxResult>,
    spec_id: SpecId,
}

impl<Tx> Evm<Tx> {
    /// Creates an EVM with the provided transaction handler registry and hard fork specification.
    pub const fn new(block: BlockEnv, registry: TxRegistry<Tx, TxResult>, spec_id: SpecId) -> Self {
        Self { block, registry, spec_id }
    }

    /// Returns the transaction handler registry.
    pub const fn registry(&self) -> &TxRegistry<Tx, TxResult> {
        &self.registry
    }

    /// Returns the active hard fork specification.
    pub const fn spec_id(&self) -> SpecId {
        self.spec_id
    }
}

impl<Tx> Evm<Tx>
where
    Tx: Typed2718,
{
    /// Dispatches the transaction to the handler registered for its EIP-2718 type byte.
    pub fn transact(&self, tx: &Tx) -> HandlerResult<TxResult> {
        self.registry.try_get_by_type(tx.ty())?.call(tx)
    }

    /// Dispatches each transaction to its registered EIP-2718 handler.
    pub fn transact_iter<'a, I>(
        &'a self,
        txs: I,
    ) -> impl Iterator<Item = HandlerResult<TxResult>> + 'a
    where
        I: IntoIterator<Item = &'a Tx>,
        I::IntoIter: 'a,
        Tx: 'a,
        Self: 'a,
    {
        txs.into_iter().map(move |tx| self.transact(tx))
    }
}

impl<Tx> Host for Evm<Tx> {
    fn block_env(&mut self) -> &BlockEnv {
        &self.block
    }

    fn balance(&mut self, _address: Word) -> Word {
        Word::ZERO
    }

    fn get_code_size(&mut self, _address: Word) -> usize {
        0
    }

    fn get_code_hash(&mut self, _address: Word) -> B256 {
        B256::ZERO
    }

    fn block_hash(&mut self, _number: u64) -> Option<B256> {
        None
    }

    fn sload(&mut self, _index: Word) -> Word {
        Word::ZERO
    }

    fn sstore(&mut self, _index: Word, _value: Word) {}

    fn tload(&mut self, _index: Word) -> Word {
        Word::ZERO
    }

    fn tstore(&mut self, _index: Word, _value: Word) {}

    fn log(&mut self, _log: Log) {}

    fn run_interpreter(
        &mut self,
        tx_env: TxEnv,
        bytecode: Bytecode,
        message: Message,
    ) -> InstrStop {
        run_interpreter_with_host(self, bytecode, self.spec_id, tx_env, message)
    }
}

fn run_interpreter_with_host<H>(
    host: &mut H,
    bytecode: Bytecode,
    spec_id: SpecId,
    tx_env: TxEnv,
    message: Message,
) -> InstrStop
where
    H: Host,
{
    let gas_table = new_gas_table(spec_id);
    let mut interpreter = Interpreter::new(bytecode, spec_id, tx_env, message);
    interpreter.run_with_table(Table::Normal(&DEFAULT_TABLE), &gas_table, host)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        bytecode::Bytecode,
        interpreter::{MessageKind, op},
        registry::TxRequest,
    };
    use alloy_primitives::{Address, Bytes, U256};

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
        let evm = Evm::new(BlockEnv::default(), registry, SpecId::OSAKA);
        let tx = TestTx { value: 41 };

        assert_eq!(evm.transact(&tx).map(|result| result.gas_used), Ok(42));
    }

    #[test]
    fn dispatches_transaction_iter() {
        let registry =
            TxRegistry::new().with_handler(TEST_TX_TYPE, extract_test_tx, handle_test_tx);
        let evm = Evm::new(BlockEnv::default(), registry, SpecId::OSAKA);
        let txs = [TestTx { value: 1 }, TestTx { value: 2 }];
        let gas_used = evm
            .transact_iter(&txs)
            .map(|result| result.map(|result| result.gas_used))
            .collect::<HandlerResult<Vec<_>>>();

        assert_eq!(gas_used, Ok(vec![2, 3]));
    }

    #[test]
    fn runs_interpreter_with_message() {
        let mut evm =
            Evm::new(BlockEnv::default(), TxRegistry::<TestTx, TxResult>::new(), SpecId::OSAKA);
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

        let stop = Host::run_interpreter(&mut evm, TxEnv::default(), bytecode, message);

        assert!(matches!(stop, InstrStop::Stop));
    }
}
