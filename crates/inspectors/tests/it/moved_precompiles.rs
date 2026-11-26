//! After a precompile move, inspectors treat the addresses from before the move as precompiles,
//! because those stay warm.

use alloy_consensus::{TxLegacy, transaction::Recovered};
use alloy_eip2930::{AccessList, AccessListItem};
use alloy_primitives::{Address, Bytes, TxKind, address, hex};
use evm2::{
    BaseEvmTypes, Evm, Precompiles, SpecId, TxResultWithState,
    bytecode::Bytecode,
    ethereum::{TxEnvelope, ethereum_tx_registry},
    evm::{AccountInfo, InMemoryDB},
    precompile::PrecompileProvider,
};
use evm2_inspectors::access_list::AccessListInspector;

#[cfg(feature = "js-tracer")]
use evm2_inspectors::tracing::js::JsInspector;

const CALLER: Address = Address::repeat_byte(0xaa);
const PROBE: Address = Address::repeat_byte(0xbb);
const IDENTITY: Address = Address::with_last_byte(0x04);
const MOVED_IDENTITY: Address = address!("0x0000000000000000000000000000000000001000");
/// `BALANCE(IDENTITY)`, `BALANCE(MOVED_IDENTITY)`, `CALL(gas, MOVED_IDENTITY, 0, 0, 0, 0, 0)`,
/// `STOP`.
const TOUCH_BOTH: [u8; 21] = hex!("600431506110003150" "5f5f5f5f5f6110005af150" "00");

/// Runs a call to `PROBE`, which has `code`, with the identity precompile moved to
/// `MOVED_IDENTITY`, and returns the EVM, the transaction and its result.
fn run_probe<'a>(
    code: &[u8],
    inspector: impl evm2::Inspector<BaseEvmTypes> + 'a,
) -> (Evm<'a, BaseEvmTypes>, Recovered<TxEnvelope>, TxResultWithState<BaseEvmTypes>) {
    let mut db = InMemoryDB::default();
    db.insert_account_info(
        &PROBE,
        AccountInfo::default().with_code(Bytecode::new_raw(Bytes::copy_from_slice(code))),
    );
    let mut precompiles = Precompiles::base(SpecId::CANCUN);
    PrecompileProvider::<BaseEvmTypes>::move_precompiles(
        &mut precompiles,
        &[(IDENTITY, MOVED_IDENTITY)],
    )
    .unwrap();
    let mut evm = Evm::<BaseEvmTypes>::new(
        SpecId::CANCUN,
        Default::default(),
        ethereum_tx_registry(SpecId::CANCUN),
        db,
        precompiles,
    );
    evm.set_inspector(inspector);
    let tx = TxEnvelope::Legacy(TxLegacy {
        gas_limit: 100_000,
        to: TxKind::Call(PROBE),
        ..Default::default()
    });
    let tx = Recovered::new_unchecked(tx, CALLER);
    let res = evm.transact(&tx).unwrap().detach();
    assert!(res.result.is_success(), "{res:#?}");
    (evm, tx, res)
}

fn access_list(addresses: &[Address]) -> AccessList {
    AccessList(
        addresses
            .iter()
            .map(|address| AccessListItem { address: *address, storage_keys: vec![] })
            .collect(),
    )
}

#[test]
fn access_list_excludes_warm_precompiles_after_move() {
    let mut inspector = AccessListInspector::default();
    drop(run_probe(&TOUCH_BOTH, &mut inspector));
    assert_eq!(inspector.into_access_list(), access_list(&[MOVED_IDENTITY]));
}

#[test]
fn access_list_prunes_seeded_warm_precompiles_after_move() {
    let mut inspector = AccessListInspector::new(access_list(&[IDENTITY, MOVED_IDENTITY]));
    drop(run_probe(&hex!("00"), &mut inspector));
    assert_eq!(inspector.into_access_list(), access_list(&[MOVED_IDENTITY]));
}

#[cfg(feature = "js-tracer")]
#[test]
fn js_is_precompiled_uses_warm_precompiles_after_move() {
    let code = r#"{
        fault: function() {},
        result: function() {
            return [
                isPrecompiled("0x04"),
                isPrecompiled("0x0000000000000000000000000000000000001000"),
            ];
        }
    }"#;
    let inspector = JsInspector::new(code.to_string(), serde_json::Value::Null).unwrap();
    let (mut evm, tx, res) = run_probe(&hex!("00"), inspector);
    let mut inspector = evm.clear_inspector_as::<JsInspector>().unwrap();
    let block = *evm.block();
    let result = inspector.json_result(&res, &tx, &block, evm.database_mut()).unwrap();
    assert_eq!(result, serde_json::json!([true, false]));
}
