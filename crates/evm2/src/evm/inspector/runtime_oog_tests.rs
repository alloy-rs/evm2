use super::*;
use crate::{
    BaseEvmTypes, Evm, ExecutionError, Precompiles, SpecId,
    env::BlockEnvExt,
    ethereum::{LazyTxEip7702, TxEnvelope, ethereum_tx_registry, intrinsic_gas},
    evm::{AccountInfo, InMemoryDB},
    interpreter::{GasTracker, InstrStop, MessageResultExt},
};
use alloc::{string::ToString, vec, vec::Vec};
use alloy_consensus::{
    TxEip1559, TxEip2930, TxEip4844, TxEip7702, TxLegacy,
    transaction::{Recovered, Transaction},
};
use alloy_eips::eip7702::{Authorization, RecoveredAuthority, RecoveredAuthorization};
use alloy_primitives::{B256, Bytes, TxKind};

#[derive(Default)]
struct RootInspector {
    events: Vec<&'static str>,
    message: Option<Message>,
    spec: Option<SpecId>,
    result: Option<MessageResult>,
    initialized: usize,
    steps: usize,
    override_start: bool,
    override_end: bool,
    fail_end: bool,
}

impl RootInspector {
    fn start(
        &mut self,
        interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
        message: &Message,
    ) -> Option<MessageResult> {
        self.spec = Some(interp.spec());
        self.message = Some(message.clone());
        self.override_start.then(|| Self::override_result(message))
    }

    fn end(
        &mut self,
        interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
        message: &Message,
        result: &mut MessageResult,
    ) {
        self.result = Some(result.clone());
        if self.override_end {
            *result = Self::override_result(message);
        }
        if self.fail_end {
            interp.fail(ExecutionError::Fatal("root hook failed".into()));
        }
    }

    fn override_result(message: &Message) -> MessageResult {
        let mut gas =
            GasTracker::new_with_execution_gas_and_reservoir(message.gas_limit, message.reservoir);
        gas.spend(123).unwrap();
        MessageResultExt {
            stop: InstrStop::Revert,
            gas,
            output: Bytes::from_static(&[0x42]),
            ..Default::default()
        }
    }
}

impl Inspector<BaseEvmTypes> for RootInspector {
    fn initialize_interp(&mut self, _interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
        self.initialized += 1;
    }

    fn step(&mut self, _interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
        self.steps += 1;
    }

    fn call(
        &mut self,
        interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
        message: &mut Message,
    ) -> Option<MessageResult> {
        self.events.push("call");
        self.start(interp, message)
    }

    fn call_end(
        &mut self,
        interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
        message: &Message,
        result: &mut MessageResult,
    ) {
        self.events.push("call_end");
        self.end(interp, message, result);
    }

    fn create(
        &mut self,
        interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
        message: &mut Message,
    ) -> Option<MessageResult> {
        self.events.push("create");
        self.start(interp, message)
    }

    fn create_end(
        &mut self,
        interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
        message: &Message,
        result: &mut MessageResult,
    ) {
        self.events.push("create_end");
        self.end(interp, message, result);
    }
}

fn new_evm(spec: SpecId, database: InMemoryDB) -> Evm<'static, BaseEvmTypes> {
    let mut evm = Evm::new(
        spec,
        BlockEnvExt::default(),
        ethereum_tx_registry(spec),
        database,
        Precompiles::base(spec),
    );
    evm.state_mut().enable_bal_builder();
    evm
}

fn caller_db(caller: Address) -> InMemoryDB {
    let mut db = InMemoryDB::default();
    db.insert_account_info(
        &caller,
        AccountInfo { balance: U256::from(1_000_000), nonce: 7, ..Default::default() },
    );
    db
}

fn assert_root(
    spec: SpecId,
    database: InMemoryDB,
    tx: TxEnvelope,
    caller: Address,
    runtime_oog: bool,
) {
    let tx = Recovered::new_unchecked(tx, caller);
    let mut plain = new_evm(spec, database.clone());
    let expected = plain.transact(&tx).unwrap().detach();
    let mut inspected = new_evm(spec, database);
    inspected.set_inspector(RootInspector::default());
    let result = inspected.transact(&tx).unwrap().detach();
    assert_eq!(result, expected);
    assert_eq!(inspected.state().bal_builder(), plain.state().bal_builder());
    let result = result.result;
    let inspector = inspected.clear_inspector_as::<RootInspector>().unwrap();
    let kind = tx.inner().kind();
    assert_eq!(
        inspector.events,
        if kind.is_create() { ["create", "create_end"] } else { ["call", "call_end"] }
    );
    assert_eq!(inspector.spec, Some(spec));
    let message = inspector.message.as_ref().unwrap();
    assert_eq!(message.depth, 0);
    assert_eq!(message.caller, caller);
    assert_eq!(message.destination, kind.to().copied().unwrap_or_else(|| caller.create(7)));
    assert_eq!(message.call_target, message.destination);
    assert_eq!(message.value, tx.inner().value());
    assert_eq!(&message.input, tx.inner().input());
    assert_eq!(inspector.result.as_ref().unwrap().stop, result.stop);
    if runtime_oog {
        assert_eq!(result.stop, InstrStop::OutOfGas);
        assert_eq!(result.tx_gas_used(), tx.inner().gas_limit());
        assert_eq!(result.state_gas_spent, 0);
        assert_eq!(inspector.initialized, 0);
        assert_eq!(inspector.steps, 0);
        assert_eq!(inspector.result.unwrap().gas.remaining(), 0);
    } else {
        assert!(result.status);
    }
}

#[test]
fn runtime_oog_root_hooks_for_call_and_create() {
    let caller = Address::repeat_byte(0xaa);
    let recipient = Address::repeat_byte(0xbb);
    for spec in [SpecId::OSAKA, SpecId::AMSTERDAM] {
        for to in [TxKind::Call(recipient), TxKind::Create] {
            for input in [Bytes::new(), Bytes::from_static(&[0x00])] {
                for value in [U256::ZERO, U256::ONE] {
                    let runtime_oog =
                        spec == SpecId::AMSTERDAM && (to.is_create() || !value.is_zero());
                    assert_root(
                        spec,
                        caller_db(caller),
                        TxEnvelope::Legacy(TxLegacy {
                            nonce: 7,
                            gas_limit: 200_000,
                            to,
                            value,
                            input: input.clone(),
                            ..Default::default()
                        }),
                        caller,
                        runtime_oog,
                    );
                }
            }
        }
    }
}

#[test]
fn runtime_oog_root_hooks_for_typed_transactions() {
    let caller = Address::repeat_byte(0xaa);
    let recipient = Address::repeat_byte(0xbb);
    for spec in [SpecId::OSAKA, SpecId::AMSTERDAM] {
        let txs = [
            TxEnvelope::Eip2930(TxEip2930 {
                chain_id: 1,
                nonce: 7,
                gas_limit: 200_000,
                to: recipient.into(),
                value: U256::ONE,
                ..Default::default()
            }),
            TxEnvelope::Eip1559(TxEip1559 {
                chain_id: 1,
                nonce: 7,
                gas_limit: 200_000,
                to: recipient.into(),
                value: U256::ONE,
                ..Default::default()
            }),
            TxEnvelope::Eip4844(
                TxEip4844 {
                    chain_id: 1,
                    nonce: 7,
                    gas_limit: 200_000,
                    to: recipient,
                    value: U256::ONE,
                    max_fee_per_blob_gas: 1,
                    blob_versioned_hashes: vec![B256::repeat_byte(1)],
                    ..Default::default()
                }
                .into(),
            ),
        ];
        for tx in txs {
            assert_root(spec, caller_db(caller), tx, caller, spec == SpecId::AMSTERDAM);
        }
    }
}

#[test]
fn runtime_oog_root_hooks_for_delegated_recipient() {
    let caller = Address::repeat_byte(0xaa);
    let recipient = Address::repeat_byte(0xbb);
    let delegate = Address::repeat_byte(0xcc);
    for gas_limit in [15_000, 15_100, 17_500] {
        let mut db = caller_db(caller);
        db.insert_account_info(
            &recipient,
            AccountInfo::default().with_code(crate::bytecode::Bytecode::new_eip7702(delegate)),
        );
        assert_root(
            SpecId::AMSTERDAM,
            db,
            TxEnvelope::Legacy(TxLegacy {
                nonce: 7,
                gas_limit,
                to: recipient.into(),
                ..Default::default()
            }),
            caller,
            true,
        );
    }
}

#[test]
fn runtime_oog_root_hooks_for_authorizations() {
    let caller = Address::repeat_byte(0xaa);
    let recipient = Address::repeat_byte(0xbb);
    let authority = Address::repeat_byte(0xcc);
    let later_authority = Address::repeat_byte(0xdd);
    for authority_balance in [U256::ZERO, U256::ONE] {
        for gas_limit in [40_000, 70_000, 200_000] {
            let mut db = caller_db(caller);
            if !authority_balance.is_zero() {
                db.insert_account_info(
                    &authority,
                    AccountInfo::default().with_balance(authority_balance),
                );
            }
            let tx = LazyTxEip7702::from_cached_recovered_authorizations(
                TxEip7702 {
                    chain_id: 1,
                    nonce: 7,
                    gas_limit,
                    to: recipient,
                    value: U256::ONE,
                    ..Default::default()
                },
                [authority, later_authority]
                    .into_iter()
                    .map(|authority| {
                        RecoveredAuthorization::new_unchecked(
                            Authorization { chain_id: U256::ONE, address: recipient, nonce: 0 },
                            RecoveredAuthority::Valid(authority),
                        )
                    })
                    .collect(),
            );
            assert_root(SpecId::AMSTERDAM, db, tx.into(), caller, true);
        }
    }
}

#[test]
fn runtime_oog_root_hook_overrides_are_settled() {
    let caller = Address::repeat_byte(0xaa);
    for to in [TxKind::Call(Address::repeat_byte(0xbb)), TxKind::Create] {
        for override_start in [false, true] {
            let tx = Recovered::new_unchecked(
                TxEnvelope::Legacy(TxLegacy {
                    nonce: 7,
                    gas_limit: 200_000,
                    to,
                    value: U256::ONE,
                    ..Default::default()
                }),
                caller,
            );
            let mut evm = new_evm(SpecId::AMSTERDAM, caller_db(caller));
            evm.set_inspector(RootInspector {
                override_start,
                override_end: !override_start,
                ..Default::default()
            });
            let result = evm.call_tx(&tx).unwrap();
            assert_eq!(result.stop, InstrStop::Revert);
            assert_eq!(result.output, Bytes::from_static(&[0x42]));
            let intrinsic =
                intrinsic_gas(evm.version(), caller, to, tx.inner().input(), 0, 0, U256::ONE);
            assert_eq!(result.tx_gas_used(), intrinsic + 123);
            let inspector = evm.clear_inspector_as::<RootInspector>().unwrap();
            assert_eq!(inspector.events.len(), 2);
            assert_eq!(inspector.initialized, 0);
            assert_eq!(inspector.steps, 0);
        }
    }
}

#[test]
fn runtime_oog_hook_error_is_propagated_and_evm_can_be_reused() {
    let caller = Address::repeat_byte(0xaa);
    let mut evm = new_evm(SpecId::AMSTERDAM, caller_db(caller));
    let tx = Recovered::new_unchecked(
        TxEnvelope::Legacy(TxLegacy {
            nonce: 7,
            gas_limit: 200_000,
            to: Address::repeat_byte(0xbb).into(),
            value: U256::ONE,
            ..Default::default()
        }),
        caller,
    );
    evm.set_inspector(RootInspector { fail_end: true, ..Default::default() });
    assert_eq!(evm.call_tx(&tx).unwrap_err().to_string(), "fatal error: root hook failed");
    evm.clear_inspector();
    assert_eq!(evm.call_tx(&tx).unwrap().stop, InstrStop::OutOfGas);
}

#[test]
fn invalid_transactions_do_not_emit_root_hooks() {
    let caller = Address::repeat_byte(0xaa);
    let mut evm = new_evm(SpecId::AMSTERDAM, caller_db(caller));
    let tx = Recovered::new_unchecked(
        TxEnvelope::Legacy(TxLegacy { nonce: 7, gas_limit: 100, ..Default::default() }),
        caller,
    );
    evm.set_inspector(RootInspector::default());
    assert!(evm.call_tx(&tx).is_err());
    let inspector = evm.clear_inspector_as::<RootInspector>().unwrap();
    assert!(inspector.events.is_empty());
    assert_eq!(inspector.initialized, 0);
    assert_eq!(inspector.steps, 0);
}
