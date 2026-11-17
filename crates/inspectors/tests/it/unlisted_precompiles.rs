//! Precompiles a provider dispatches to without listing them, like Tempo's token precompiles,
//! trace like contracts.

use alloy_primitives::{Address, Bytes, TxKind, U256, hex};
use alloy_rpc_types_eth::TransactionInfo;
use alloy_rpc_types_trace::parity::Action;
use evm2::{
    BaseEvmTypes, Evm, ExecutionConfig, Precompiles, SpecId, Version,
    bytecode::Bytecode,
    ethereum::{ethereum_tx_registry, execute_initial_frame, prepare_initial_frame},
    evm::{AccountInfo, InMemoryDB},
    interpreter::{GasTracker, Message},
    precompile::{PrecompileOutput, PrecompileProvider},
    precompiles::{MovePrecompileError, PrecompileId, PrecompileResult},
};
use evm2_inspectors::tracing::{FourByteInspector, TracingInspector, TracingInspectorConfig};

const CALLER: Address = Address::repeat_byte(0xaa);
/// Calls `ON_DEMAND` and the identity precompile with the selector `0xaabbccdd`, then stops.
const PROBE: Address = Address::with_last_byte(0x45);
/// A precompile the provider dispatches to without listing it.
const ON_DEMAND: Address = Address::repeat_byte(0x20);

/// Runs a call to `PROBE` under `inspector`.
fn run_probe<I: evm2::Inspector<BaseEvmTypes>>(inspector: &mut I) {
    let mut db = InMemoryDB::default();
    let code = hex!(
        // mem[0..4] = 0xaabbccdd
        "63aabbccdd60e01b5f52"
        // CALL(gas, ON_DEMAND, 0, 0, 4, 0, 0)
        "5f5f60045f5f7320202020202020202020202020202020202020205af150"
        // CALL(gas, identity, 0, 0, 4, 0, 0)
        "5f5f60045f5f60045af15000"
    );
    db.insert_account_info(
        &PROBE,
        AccountInfo::default().with_code(Bytecode::new_raw(Bytes::copy_from_slice(&code))),
    );
    let mut evm = Evm::<BaseEvmTypes>::new_with_execution_config(
        ExecutionConfig::for_spec_and_version(SpecId::CANCUN, Version::new(SpecId::CANCUN)),
        SpecId::CANCUN,
        Default::default(),
        ethereum_tx_registry(SpecId::CANCUN),
        db,
        OnDemandPrecompiles(Precompiles::base(SpecId::CANCUN)),
    );
    evm.set_inspector(inspector);
    let mut gas = GasTracker::new(100_000);
    let frame = prepare_initial_frame(
        &mut evm,
        CALLER,
        0,
        TxKind::Call(PROBE),
        &Bytes::new(),
        U256::ZERO,
        &mut gas,
    )
    .unwrap();
    assert!(execute_initial_frame(&mut evm, frame, &mut gas, 100_000, 0).unwrap().is_success());
}

#[test]
fn unlisted_precompiles_trace_like_contracts() {
    let mut tracer = TracingInspector::new(TracingInspectorConfig::default_parity());
    run_probe(&mut tracer);
    let targets: Vec<_> = tracer
        .into_parity_builder()
        .into_localized_transaction_traces(TransactionInfo::default())
        .into_iter()
        .map(|trace| match trace.trace.action {
            Action::Call(call) => (trace.trace.trace_address, call.to),
            action => panic!("unexpected action {action:?}"),
        })
        .collect();
    // The zero-value identity call is left out; the unlisted precompile is not.
    assert_eq!(targets, [(vec![], PROBE), (vec![0], ON_DEMAND)]);

    let mut fourbyte = FourByteInspector::default();
    run_probe(&mut fourbyte);
    let selectors: Vec<_> = fourbyte.inner().iter().map(|(key, count)| (*key, *count)).collect();
    assert_eq!(selectors, [((hex!("aabbccdd").into(), 0), 1)]);
}

/// The base precompiles, plus [`ON_DEMAND`], which is dispatched to but not listed.
struct OnDemandPrecompiles(Precompiles<BaseEvmTypes>);

impl PrecompileProvider<BaseEvmTypes> for OnDemandPrecompiles {
    fn addresses(&self) -> Vec<Address> {
        self.0.addresses()
    }

    fn precompile_ids(&self) -> Vec<(Address, PrecompileId)> {
        self.0.precompile_ids()
    }

    fn contains(&self, address: &Address) -> bool {
        *address == ON_DEMAND || self.0.contains(address)
    }

    fn move_precompiles(
        &mut self,
        moves: &[(Address, Address)],
    ) -> Result<(), MovePrecompileError> {
        self.0.move_precompiles(moves)
    }

    fn execute(
        &mut self,
        evm: &mut Evm<'_, BaseEvmTypes>,
        message: &Message<BaseEvmTypes>,
        gas: &mut GasTracker,
    ) -> Option<PrecompileResult> {
        if message.call_target == ON_DEMAND {
            return Some(Ok(PrecompileOutput::new(Bytes::new())));
        }
        self.0.execute(evm, message, gas)
    }
}
