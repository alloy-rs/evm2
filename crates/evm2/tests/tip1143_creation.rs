//! Creation acceptance through Tempo's public EVM2 initial-frame seam.
//! These cases do not certify downstream Tempo transaction validation.

use alloy_consensus::{TxLegacy, transaction::Recovered};
use alloy_primitives::{Address, Bytes, TxKind, U256, keccak256};
use evm2::{
    BaseEvmTypes, Evm, ExecutionConfig, Precompiles, SpecId, Version,
    bytecode::Bytecode,
    env::{BlockEnvExt, TxEnvExt},
    ethereum::{
        TxEnvelope, ethereum_tx_registry, execute_initial_frame, prepare_initial_frame,
        validate_create_initcode,
    },
    evm::{AccountInfo, InMemoryDB, SystemTx, inspector::Inspector},
    interpreter::{GasTracker, Host, InstrStop, Interpreter, MessageExt, MessageKind},
    registry::TxRegistry,
};

use std::{cell::RefCell, rc::Rc};

const CHUNK: usize = 24541;

fn initcode(runtime: &[u8]) -> Bytes {
    let size = runtime.len();
    // PUSH3 size; PUSH1 16; PUSH1 0; CODECOPY; PUSH3 size; PUSH1 0; RETURN.
    let mut code = vec![
        0x62,
        (size >> 16) as u8,
        (size >> 8) as u8,
        size as u8,
        0x60,
        16,
        0x60,
        0,
        0x39,
        0x62,
        (size >> 16) as u8,
        (size >> 8) as u8,
        size as u8,
        0x60,
        0,
        0xf3,
    ];
    assert_eq!(code.len(), 16);
    code.extend(runtime);
    code.into()
}

fn create(input: Bytes, budget: u64) -> (InstrStop, u64, Option<AccountInfo>) {
    let caller = Address::repeat_byte(0x55);
    let address = caller.create(0);
    let version = Version::new(SpecId::PRAGUE).with_tip1143(true);
    validate_create_initcode(&version, TxKind::Create, &input).unwrap();
    let mut evm = Evm::<'_, BaseEvmTypes>::new_with_execution_config(
        ExecutionConfig::for_spec_and_version(SpecId::PRAGUE, version),
        SpecId::PRAGUE,
        BlockEnvExt::default(),
        TxRegistry::new(),
        InMemoryDB::default(),
        Precompiles::base(SpecId::PRAGUE),
    );
    let mut gas = GasTracker::new(budget);
    let frame =
        prepare_initial_frame(&mut evm, caller, 0, TxKind::Create, &input, U256::ZERO, &mut gas)
            .unwrap();
    let result =
        execute_initial_frame(&mut evm, &TxEnvExt::default(), frame, &mut gas, budget, 0).unwrap();
    if result.stop.is_success() {
        assert_eq!(result.created_address, Some(address));
    }
    let account = evm.state_mut().account(&address).unwrap().get().cloned();
    (result.stop, gas.spent(), account)
}

#[test]
fn tip1143_t02_aa_seam_runtime_boundaries() {
    for size in [0, 24575, 24576, 24577, 958465, 981640, 981641] {
        let runtime = vec![0; size];
        let (stop, _, account) = create(initcode(&runtime), 1_000_000_000);
        if size > 981640 {
            assert_eq!(stop, InstrStop::CreateContractSizeLimit);
            assert!(account.is_none());
            continue;
        }
        assert!(stop.is_success(), "size={size}, stop={stop:?}");
        let account = account.unwrap();
        assert_eq!(account.code_hash, keccak256(&runtime));
        assert_eq!(account.code.as_ref().unwrap().original_byte_slice(), runtime);
        if size <= CHUNK {
            assert!(account.code_metadata().is_none());
        } else {
            let metadata = account.code_metadata().unwrap();
            assert_eq!(metadata.code_size() as usize, size);
            assert_eq!(
                metadata.chunk_hashes(),
                runtime.chunks(CHUNK).map(keccak256).collect::<Vec<_>>()
            );
        }
    }
}

#[test]
fn tip1143_t03_aa_seam_initcode_boundaries() {
    let version = Version::new(SpecId::PRAGUE).with_tip1143(true);
    let mut input = vec![0; 1966080];
    input[..5].copy_from_slice(&[0x60, 0, 0x60, 0, 0xf3]);
    let (stop, gas, account) = create(input.clone().into(), 1_000_000_000);
    assert!(stop.is_success());
    assert_eq!(gas, 6);
    assert!(account.unwrap().code_metadata().is_none());
    input.push(0);
    assert!(validate_create_initcode(&version, TxKind::Create, &input.into()).is_err());
}

#[test]
fn tip1143_t04_code_deposit_bracket() {
    for size in [1, CHUNK, CHUNK + 1] {
        let input = initcode(&vec![0; size]);
        let words = (size as u64).div_ceil(32);
        // Five PUSH instructions (15) plus CODECOPY opcode (3).
        let cost = 18 + 3 * words + (3 * words + words * words / 512) + 200 * size as u64;
        let success = create(input.clone(), cost);
        assert!(success.0.is_success(), "size={size}, stop={:?}", success.0);
        assert_eq!(success.1, cost);
        let failure = create(input, cost - 1);
        assert!(!failure.0.is_success());
        assert!(failure.2.is_none());
    }
}

#[test]
fn tip1143_t05_single_chunk_truncated_push_deployment() {
    for width in 1..=32 {
        for present in 0..width {
            for size in [width + 1, CHUNK - 1, CHUNK] {
                let mut runtime = vec![0; size];
                let opcode = size - present - 1;
                runtime[opcode] = 0x5f + width as u8;
                runtime[opcode + 1..].fill(0xab);
                let (stop, _, account) = create(initcode(&runtime), 10_000_000);
                assert!(stop.is_success(), "width={width}, present={present}, size={size}");
                let account = account.unwrap();
                assert!(account.code_metadata().is_none());
                assert_eq!(account.code.unwrap().original_byte_slice(), runtime);
            }
        }
    }
}

#[test]
fn tip1143_t06_crossing_pushes_and_boundary_data_are_prepared() {
    for boundary in [CHUNK, 39 * CHUNK] {
        for width in 1..=32 {
            for overrun in 1..=width {
                let mut runtime = vec![0; boundary + 33];
                runtime[boundary - (width + 1 - overrun)] = 0x5f + width as u8;
                let (stop, _, account) = create(initcode(&runtime), 1_000_000_000);
                assert!(stop.is_success(), "boundary={boundary}, width={width}, overrun={overrun}");
                assert_eq!(account.unwrap().code.unwrap().original_byte_slice(), runtime);
            }
            let mut runtime = vec![0; boundary + 33];
            runtime[boundary - width - 1] = 0x5f + width as u8;
            // A boundary inside immediate data no longer requires a decoded STOP.
            assert!(create(initcode(&runtime), 1_000_000_000).0.is_success());
            runtime[boundary - width - 1] = 0;
            runtime[boundary - width - 2] = 0x5f + width as u8;
            assert!(create(initcode(&runtime), 1_000_000_000).0.is_success());
        }
    }
}

#[test]
fn tip1143_t06_final_push_zero_pads_missing_immediate() {
    for width in 1..=32 {
        for present in 0..=width {
            let mut runtime = vec![0; CHUNK];
            runtime.push(0x5f + width as u8);
            runtime.extend(std::iter::repeat_n(0xab, present));
            let result = create(initcode(&runtime), 10_000_000);
            assert!(result.0.is_success(), "width={width}, present={present}");
            assert_eq!(result.2.unwrap().code.unwrap().original_byte_slice(), runtime);
        }
    }
    for terminal in [0xf3, 0xfd, 0xfe, 0x56] {
        let mut runtime = vec![0; CHUNK + 1];
        runtime[CHUNK - 1] = terminal;
        assert!(create(initcode(&runtime), 10_000_000).0.is_success());
    }
}

#[test]
fn tip1143_t14_resident_initcode_crosses_boundary_and_observes_original_input() {
    // Jump to a PUSH2 whose immediate straddles the runtime chunk boundary.
    // Initcode is resident and must not use the runtime boundary validator.
    let target = CHUNK - 2;
    let mut input = vec![0x61, (target >> 8) as u8, target as u8, 0x56];
    input.resize(target, 0);
    input.extend([0x5b, 0x61, 0x12, 0x34, 0x60, 0, 0x52]);
    let pc = input.len();
    input.extend([0x58, 0x60, 32, 0x52, 0x38, 0x60, 64, 0x52]);
    // Preserve the original PUSH opcode and both immediate bytes in output.
    input.extend([0x60, 3, 0x61, 0x5f, 0xdc, 0x60, 96, 0x39]);
    input.extend([0x60, 128, 0x60, 0, 0xf3]);
    let size = input.len();
    let (stop, spent, account) = create(input.into(), 1_000_000);
    assert!(stop.is_success());
    // 70 ordinary execution gas plus 128 original output bytes deposited.
    // Own PC, CODESIZE, CODECOPY and the jump add no chunk tariffs.
    assert_eq!(spent, 70 + 128 * 200);
    let account = account.unwrap();
    assert!(account.code_metadata().is_none());
    let output = account.code.unwrap();
    let output = output.original_byte_slice();
    assert_eq!(output.len(), 128);
    assert_eq!(U256::from_be_slice(&output[..32]), U256::from(0x1234));
    assert_eq!(U256::from_be_slice(&output[32..64]), U256::from(pc));
    assert_eq!(U256::from_be_slice(&output[64..96]), U256::from(size));
    assert_eq!(&output[96..99], &[0x61, 0x12, 0x34]);
    assert_eq!(&output[99..], &[0; 29]);
    assert_eq!(account.code_hash, keccak256(output));
}

fn push_size(code: &mut Vec<u8>, size: usize) {
    assert!(size < 1 << 24);
    code.extend([0x62, (size >> 16) as u8, (size >> 8) as u8, size as u8]);
}

// Run CREATE/CREATE2 from resident factory initcode. This isolates the child
// limits from the runtime size/entry tariff of an unrelated factory contract.
fn nested_create(input: Bytes, create2: bool) -> (InstrStop, Address, Option<AccountInfo>, Bytes) {
    let (stop, address, account, output, _) = nested_create_metered(input, create2);
    (stop, address, account, output)
}

fn nested_create_metered(
    input: Bytes,
    create2: bool,
) -> (InstrStop, Address, Option<AccountInfo>, Bytes, u64) {
    let caller = Address::repeat_byte(0x55);
    let factory = caller.create(0);
    let expected =
        if create2 { factory.create2_from_code([0u8; 32], &input) } else { factory.create(1) };
    let mut code = Vec::new();
    push_size(&mut code, input.len());
    push_size(&mut code, 0);
    code.extend([0x60, 0, 0x37]);
    if create2 {
        code.extend([0x60, 0]); // salt
    }
    push_size(&mut code, input.len());
    code.extend([0x60, 0, 0x60, 0, if create2 { 0xf5 } else { 0xf0 }]);
    code.extend([0x60, 0, 0x52, 0x60, 32, 0x60, 0, 0xf3]);
    let version = Version::new(SpecId::PRAGUE).with_tip1143(true);
    let mut evm = Evm::<'_, BaseEvmTypes>::new_with_execution_config(
        ExecutionConfig::for_spec_and_version(SpecId::PRAGUE, version),
        SpecId::PRAGUE,
        BlockEnvExt::default(),
        TxRegistry::new(),
        InMemoryDB::default(),
        Precompiles::base(SpecId::PRAGUE),
    );
    evm.state_mut().account(&factory).unwrap().set_nonce(1);
    let mut message = MessageExt {
        kind: MessageKind::Call,
        caller,
        destination: factory,
        call_target: factory,
        code_address: factory,
        gas_limit: 1_000_000_000,
        input,
        code_chunk: (Bytecode::new_legacy(code.into())).into(),
        ..Default::default()
    };
    let result = Host::execute_message(&mut evm, &TxEnvExt::default(), &mut message).unwrap();
    let child = evm.state_mut().account(&expected).unwrap().get().cloned();
    (result.stop, expected, child, result.output, result.gas.spent())
}

#[test]
fn tip1143_t02_create_and_create2_runtime_boundaries() {
    for create2 in [false, true] {
        for size in [0, 24575, 24576, 24577, 958465, 981640, 981641] {
            let runtime = vec![0; size];
            let (stop, expected, account, returned) = nested_create(initcode(&runtime), create2);
            assert!(stop.is_success(), "create2={create2}, size={size}, stop={stop:?}");
            assert_eq!(returned.len(), 32);
            if size > 981640 {
                assert_eq!(returned.as_ref(), &[0; 32]);
                assert!(account.is_none());
            } else {
                assert_eq!(&returned[12..], expected.as_slice());
                let account = account.unwrap();
                assert_eq!(account.code_hash, keccak256(&runtime));
                assert_eq!(account.code.as_ref().unwrap().original_byte_slice(), runtime);
                if size <= CHUNK {
                    assert!(account.code_metadata().is_none());
                } else {
                    let metadata = account.code_metadata().unwrap();
                    assert_eq!(metadata.code_size() as usize, size);
                    assert_eq!(
                        metadata.chunk_hashes(),
                        runtime.chunks(CHUNK).map(keccak256).collect::<Vec<_>>()
                    );
                }
            }
        }
    }
}

#[test]
fn tip1143_t03_create_and_create2_initcode_boundaries() {
    for create2 in [false, true] {
        for size in [1966080, 1966081] {
            let mut input = vec![0; size];
            input[..5].copy_from_slice(&[0x60, 0, 0x60, 0, 0xf3]);
            let (stop, expected, account, returned) = nested_create(input.into(), create2);
            if size == 1966080 {
                assert!(stop.is_success(), "create2={create2}, stop={stop:?}");
                assert_eq!(&returned[12..], expected.as_slice());
                let account = account.unwrap();
                assert_eq!(account.code_hash, keccak256([]));
                assert!(account.code_metadata().is_none());
            } else {
                assert_eq!(stop, InstrStop::CreateInitCodeSizeLimit);
                assert!(account.is_none());
            }
        }
    }
}

#[test]
fn tip1143_t04_nested_failure_does_not_publish_code() {
    for create2 in [false, true] {
        for input in [
            initcode(&[0xef, 0]),
            Bytes::from_static(&[0x60, 0, 0x60, 0, 0xfd]),
            Bytes::from_static(&[0xfe]),
        ] {
            let (stop, _, account, returned) = nested_create(input, create2);
            assert!(stop.is_success());
            assert_eq!(returned.as_ref(), &[0; 32]);
            assert!(account.is_none());
        }
    }
}

#[test]
fn tip1143_t02_creation_transaction_runtime_boundaries() {
    let caller = Address::repeat_byte(0x55);
    for size in [0, 24575, 24576, 24577, 958465, 981640, 981641] {
        let runtime = vec![0; size];
        let mut db = InMemoryDB::default();
        db.insert_account_info(
            &caller,
            AccountInfo { balance: U256::from(u64::MAX), ..Default::default() },
        );
        let block = BlockEnvExt { gas_limit: U256::from(1_000_000_000u64), ..Default::default() };
        let mut evm = Evm::<'_, BaseEvmTypes>::new_with_execution_config(
            ExecutionConfig::for_spec_and_version(
                SpecId::PRAGUE,
                Version::new(SpecId::PRAGUE).with_tip1143(true),
            ),
            SpecId::PRAGUE,
            block,
            ethereum_tx_registry(SpecId::PRAGUE),
            db,
            Precompiles::base(SpecId::PRAGUE),
        );
        let tx = Recovered::new_unchecked(
            TxEnvelope::Legacy(TxLegacy {
                gas_limit: 1_000_000_000,
                to: TxKind::Create,
                input: initcode(&runtime),
                ..Default::default()
            }),
            caller,
        );
        let result = evm.transact(&tx).unwrap().detach();
        if size > 981640 {
            assert_eq!(result.result.stop, InstrStop::CreateContractSizeLimit);
            assert!(result.pending_state.account_info(&caller.create(0)).is_none());
        } else {
            assert!(result.result.status, "size={size}, stop={:?}", result.result.stop);
            assert_eq!(result.result.created_address, Some(caller.create(0)));
            let info = result.pending_state.account_info(&caller.create(0)).unwrap();
            assert_eq!(info.code_hash, keccak256(&runtime));
            assert_eq!(info.code.as_ref().unwrap().original_byte_slice(), runtime);
            assert_eq!(info.code_metadata().is_some(), size > CHUNK);
            if let Some(metadata) = &info.code_metadata() {
                assert_eq!(metadata.code_size() as usize, size);
                assert_eq!(
                    metadata.chunk_hashes(),
                    runtime.chunks(CHUNK).map(keccak256).collect::<Vec<_>>()
                );
            }
        }
    }
}

#[test]
fn tip1143_t03_creation_transaction_initcode_boundaries() {
    let caller = Address::repeat_byte(0x55);
    for size in [1966080, 1966081] {
        let mut input = vec![0; size];
        input[..5].copy_from_slice(&[0x60, 0, 0x60, 0, 0xf3]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(
            &caller,
            AccountInfo { balance: U256::from(u64::MAX), ..Default::default() },
        );
        let block = BlockEnvExt { gas_limit: U256::from(1_000_000_000u64), ..Default::default() };
        let mut evm = Evm::<'_, BaseEvmTypes>::new_with_execution_config(
            ExecutionConfig::for_spec_and_version(
                SpecId::PRAGUE,
                Version::new(SpecId::PRAGUE).with_tip1143(true),
            ),
            SpecId::PRAGUE,
            block,
            ethereum_tx_registry(SpecId::PRAGUE),
            db,
            Precompiles::base(SpecId::PRAGUE),
        );
        let tx = Recovered::new_unchecked(
            TxEnvelope::Legacy(TxLegacy {
                gas_limit: 1_000_000_000,
                to: TxKind::Create,
                input: input.into(),
                ..Default::default()
            }),
            caller,
        );
        if size == 1966080 {
            let result = evm.transact(&tx).unwrap().detach();
            assert!(result.result.status);
            // Three nonzero calldata bytes, ordinary creation intrinsic, EIP3860
            // word metering, and two PUSH1 operations; no own-code tariff.
            assert_eq!(
                result.result.total_gas_spent,
                53000 + (size as u64 - 3) * 4 + 3 * 16 + 2 * (size as u64).div_ceil(32) + 6
            );
            let info = result.pending_state.account_info(&caller.create(0)).unwrap();
            assert_eq!(info.code_hash, keccak256([]));
            assert!(info.code_metadata().is_none());
        } else {
            assert!(evm.transact(&tx).is_err());
            assert!(evm.state_mut().account(&caller.create(0)).unwrap().get().is_none());
        }
    }
}

struct TailPushObserver {
    target: usize,
    pending: bool,
    values: Rc<RefCell<Vec<U256>>>,
}

impl Inspector<BaseEvmTypes> for TailPushObserver {
    fn step(&mut self, interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
        self.pending = interp.pc() == self.target;
        if self.pending {
            assert!((0x60..=0x7f).contains(&interp.opcode()));
        }
    }

    fn step_end(&mut self, interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
        if self.pending {
            self.values.borrow_mut().push(*interp.stack().last().unwrap());
            self.pending = false;
        }
    }
}

#[test]
fn tip1143_t05_reached_truncated_pushes_preserve_zero_padding_for_old_and_new_code() {
    for width in 1..=32 {
        for present in 0..width {
            for size in [width + 8, CHUNK - 1, CHUNK] {
                let opcode = size - present - 1;
                let destination = opcode - 1;
                let mut runtime = vec![0; size];
                runtime[..4].copy_from_slice(&[
                    0x61,
                    (destination >> 8) as u8,
                    destination as u8,
                    0x56,
                ]);
                runtime[destination] = 0x5b;
                runtime[opcode] = 0x5f + width as u8;
                runtime[opcode + 1..].fill(0xab);
                let mut expected = [0; 32];
                expected[32 - width..32 - width + present].fill(0xab);
                let created = create(initcode(&runtime), 10_000_000);
                assert!(created.0.is_success());
                let legacy = AccountInfo {
                    nonce: 1,
                    code_hash: keccak256(&runtime),
                    code: Some(Bytecode::new_legacy(Bytes::copy_from_slice(&runtime))),
                    ..Default::default()
                };
                for info in [legacy, created.2.unwrap()] {
                    assert!(info.code_metadata().is_none());
                    assert_eq!(info.code_hash, keccak256(&runtime));
                    assert_eq!(info.code.as_ref().unwrap().original_byte_slice(), runtime);
                    let owner = Address::repeat_byte(0x44);
                    let mut db = InMemoryDB::default();
                    db.insert_account_info(&owner, info);
                    let mut execution = Evm::<'_, BaseEvmTypes>::new_with_execution_config(
                        ExecutionConfig::for_spec_and_version(
                            SpecId::PRAGUE,
                            Version::new(SpecId::PRAGUE).with_tip1143(true),
                        ),
                        SpecId::PRAGUE,
                        BlockEnvExt::default(),
                        TxRegistry::new(),
                        db,
                        Precompiles::base(SpecId::PRAGUE),
                    );
                    let values = Rc::default();
                    execution.set_inspector(TailPushObserver {
                        target: opcode,
                        pending: false,
                        values: Rc::clone(&values),
                    });
                    let result =
                        execution.execute_system_call(SystemTx::new(owner, Bytes::new())).unwrap();
                    assert_eq!(result.stop, InstrStop::Stop);
                    assert_eq!(result.total_gas_spent, 28680 + 3 + 8 + 1 + 3);
                    assert_eq!(
                        *values.borrow(),
                        [U256::from_be_bytes(expected)],
                        "width={width}, present={present}, size={size}"
                    );
                }
            }
        }
    }
}

#[test]
fn tip1143_t07_t17_complete_final_push_executes_at_global_pc_without_terminal_stop() {
    for width in 1..=32 {
        let opcode = CHUNK + 1;
        let mut runtime = vec![0x61, 0x5f, 0xdd, 0x56];
        runtime.resize(CHUNK, 0);
        runtime.extend([0x5b, 0x5f + width as u8]);
        runtime.extend(std::iter::repeat_n(0xab, width));
        let created = create(initcode(&runtime), 10_000_000);
        assert!(created.0.is_success());
        let info = created.2.unwrap();
        assert_eq!(info.code_hash, keccak256(&runtime));
        assert_eq!(info.code.as_ref().unwrap().original_byte_slice(), runtime);
        let owner = Address::repeat_byte(0x44);
        let mut db = InMemoryDB::default();
        db.insert_account_info(&owner, info);
        let mut execution = Evm::<'_, BaseEvmTypes>::new_with_execution_config(
            ExecutionConfig::for_spec_and_version(
                SpecId::PRAGUE,
                Version::new(SpecId::PRAGUE).with_tip1143(true),
            ),
            SpecId::PRAGUE,
            BlockEnvExt::default(),
            TxRegistry::new(),
            db,
            Precompiles::base(SpecId::PRAGUE),
        );
        let values = Rc::default();
        execution.set_inspector(TailPushObserver {
            target: opcode,
            pending: false,
            values: Rc::clone(&values),
        });
        let result = execution.execute_system_call(SystemTx::new(owner, Bytes::new())).unwrap();
        assert_eq!(result.stop, InstrStop::Stop);
        assert_eq!(result.total_gas_spent, 2 * 28680 + 15);
        let mut expected = [0; 32];
        expected[32 - width..].fill(0xab);
        assert_eq!(*values.borrow(), [U256::from_be_bytes(expected)], "width={width}");
        assert_eq!(
            execution.state_mut().account(&owner).unwrap().code_size(),
            Some(runtime.len() as u32)
        );
    }
}

#[test]
fn tip1143_t04_creation_collision_preserves_existing_code_and_metadata() {
    let caller = Address::repeat_byte(0x55);
    let owner = caller.create(0);
    for existing_size in [0, 1, CHUNK + 1] {
        let original = vec![0; existing_size];
        let mut provider = InMemoryDB::default();
        let mut info = AccountInfo {
            nonce: 7,
            balance: U256::from(123),
            code_hash: keccak256(&original),
            code: Some(Bytecode::new_legacy(Bytes::copy_from_slice(&original))),
            ..Default::default()
        };
        if existing_size > CHUNK {
            info.extension = evm2::evm::AccountExtension::chunked(
                evm2::bytecode::CodeMetadata::new(
                    existing_size as u32,
                    original.chunks(CHUNK).map(keccak256).collect(),
                )
                .unwrap(),
            );
        }
        provider.insert_account_info(&owner, info.clone());
        let mut execution = Evm::<'_, BaseEvmTypes>::new_with_execution_config(
            ExecutionConfig::for_spec_and_version(
                SpecId::PRAGUE,
                Version::new(SpecId::PRAGUE).with_tip1143(true),
            ),
            SpecId::PRAGUE,
            BlockEnvExt::default(),
            TxRegistry::new(),
            provider,
            Precompiles::base(SpecId::PRAGUE),
        );
        // A reached SSTORE would reveal accidental initcode execution on collision.
        let input = Bytes::from_static(&[0x60, 99, 0x60, 0, 0x55, 0]);
        let mut gas = GasTracker::new(100_000);
        let frame = prepare_initial_frame(
            &mut execution,
            caller,
            0,
            TxKind::Create,
            &input,
            U256::ZERO,
            &mut gas,
        )
        .unwrap();
        let result = execute_initial_frame(
            &mut execution,
            &TxEnvExt::default(),
            frame,
            &mut gas,
            100_000,
            0,
        )
        .unwrap();
        assert_eq!(result.stop, InstrStop::CreateCollision);
        let restored = execution.state_mut().account(&owner).unwrap().get().unwrap().clone();
        assert_eq!(restored, info);
        assert_eq!(
            execution
                .state_mut()
                .account(&owner)
                .unwrap()
                .storage()
                .into_slot(U256::ZERO)
                .unwrap()
                .current(),
            U256::ZERO
        );
        assert!(execution.state_mut().account(&owner).unwrap().code_chunks().is_empty());
        assert!(execution.state_mut().logs().is_empty());
    }
}

fn creation_transaction(input: Bytes, budget: u64) -> (InstrStop, u64, Option<AccountInfo>) {
    let caller = Address::repeat_byte(0x55);
    let mut db = InMemoryDB::default();
    db.insert_account_info(
        &caller,
        AccountInfo { balance: U256::from(u64::MAX), ..Default::default() },
    );
    let block = BlockEnvExt { gas_limit: U256::from(1_000_000_000u64), ..Default::default() };
    let mut evm = Evm::<'_, BaseEvmTypes>::new_with_execution_config(
        ExecutionConfig::for_spec_and_version(
            SpecId::PRAGUE,
            Version::new(SpecId::PRAGUE).with_tip1143(true),
        ),
        SpecId::PRAGUE,
        block,
        ethereum_tx_registry(SpecId::PRAGUE),
        db,
        Precompiles::base(SpecId::PRAGUE),
    );
    let tx = Recovered::new_unchecked(
        TxEnvelope::Legacy(TxLegacy {
            gas_limit: budget,
            to: TxKind::Create,
            input,
            ..Default::default()
        }),
        caller,
    );
    let detached = evm.transact(&tx).unwrap().detach();
    (
        detached.result.stop,
        detached.result.total_gas_spent,
        detached.pending_state.account_info(&caller.create(0)).cloned(),
    )
}

/// Validation must run in common completion, including nested and envelope paths.
#[test]
fn tip1143_t06_boundary_preparation_reaches_every_creation_path() {
    for boundary in [CHUNK, 39 * CHUNK] {
        for width in 1..=32 {
            for violation in [0, 1, 2] {
                let mut runtime = vec![0; boundary + width + 1];
                match violation {
                    // A boundary byte that is PUSH data, even though it is zero.
                    0 => runtime[boundary - width - 1] = 0x5f + width as u8,
                    // One byte of the immediate belongs to the next chunk.
                    1 => runtime[boundary - width] = 0x5f + width as u8,
                    // Unreachable truncated immediate at the deployed-code end.
                    2 => runtime[boundary + 1] = 0x5f + width as u8,
                    _ => unreachable!(),
                }
                let input = initcode(&runtime);
                for create2 in [false, true] {
                    let (stop, address, account, returned) = nested_create(input.clone(), create2);
                    assert!(
                        stop.is_success(),
                        "boundary={boundary}, width={width}, kind={violation}"
                    );
                    assert_eq!(&returned[12..], address.as_slice());
                    assert_eq!(account.unwrap().code.unwrap().original_byte_slice(), runtime);
                }
                let result = creation_transaction(input, 1_000_000_000);
                assert!(result.0.is_success());
                assert_eq!(result.2.unwrap().code.unwrap().original_byte_slice(), runtime);
            }
            // Adjacent valid control: complete PUSH, decoded STOP, complete final PUSH.
            let mut runtime = vec![0; boundary + width + 1];
            runtime[boundary - width - 2] = 0x5f + width as u8;
            runtime[boundary] = 0x5f + width as u8;
            runtime[boundary + 1..].fill(0xab);
            for create2 in [false, true] {
                let (stop, address, account, returned) = nested_create(initcode(&runtime), create2);
                assert!(stop.is_success());
                assert_eq!(&returned[12..], address.as_slice());
                assert_eq!(account.unwrap().code.unwrap().original_byte_slice(), runtime);
            }
            let result = creation_transaction(initcode(&runtime), 1_000_000_000);
            assert!(result.0.is_success());
            assert_eq!(result.2.unwrap().code.unwrap().original_byte_slice(), runtime);
        }
    }
}

#[test]
fn tip1143_t04_transaction_deposit_budget_and_failure_classes() {
    for size in [1, CHUNK - 1, CHUNK, CHUNK + 1] {
        let runtime = vec![0; size];
        let input = initcode(&runtime);
        let input_words = (input.len() as u64).div_ceil(32);
        let calldata = input.iter().map(|byte| if *byte == 0 { 4 } else { 16 }).sum::<u64>();
        let words = (size as u64).div_ceil(32);
        // Five PUSH instructions (15) plus CODECOPY opcode (3).
        let execution = 18 + 6 * words + words * words / 512 + 200 * size as u64;
        let total = 53000 + calldata + 2 * input_words + execution;
        for budget in [total - 1, total, total + 1] {
            let (stop, spent, account) = creation_transaction(input.clone(), budget);
            if budget < total {
                assert_eq!(stop, InstrStop::OutOfGas);
                assert!(account.is_none());
                assert_eq!(spent, budget);
            } else {
                assert!(stop.is_success());
                assert_eq!(spent, total);
                assert_eq!(account.unwrap().code.unwrap().original_byte_slice(), runtime);
            }
        }
    }
    for (input, expected) in [
        (initcode(&[0xef, 0]), InstrStop::CreateContractStartingWithEF),
        (Bytes::from_static(&[0x60, 0, 0x60, 0, 0xfd]), InstrStop::Revert),
        (Bytes::from_static(&[0xfe]), InstrStop::InvalidFEOpcode),
    ] {
        let seam = create(input.clone(), 10_000_000);
        assert_eq!(seam.0, expected);
        assert!(seam.2.is_none());
        let transaction = creation_transaction(input, 10_000_000);
        assert_eq!(transaction.0, expected);
        assert!(transaction.2.is_none());
    }
}

#[test]
fn tip1143_t03_nested_initcode_metering_and_create2_hashing_are_preserved() {
    for size in [0, 1, 31, 32, 33, CHUNK, CHUNK + 1, 1966080] {
        let words = (size as u64).div_ceil(32);
        let memory_words = words.max(1); // The factory returns a 32-byte address.
        let memory = 3 * memory_words + memory_words * memory_words / 512;
        for create2 in [false, true] {
            // Empty or STOP-prefixed child input executes no charged instruction.
            // The resident factory copies that input and deploys its returned
            // address as return data; it has no chunk tariff.
            let (stop, expected, account, returned, spent) =
                nested_create_metered(Bytes::from(vec![0; size]), create2);
            assert!(stop.is_success(), "size={size}, create2={create2}");
            assert_eq!(&returned[12..], expected.as_slice());
            let account = account.unwrap();
            assert_eq!(account.code_hash, keccak256([]));
            assert!(account.code_metadata().is_none());
            let expected_gas = 32033 + 5 * words + memory + if create2 { 3 + 6 * words } else { 0 };
            assert_eq!(spent, expected_gas, "size={size}, create2={create2}");
        }
    }
}

#[test]
fn rjump_executes_in_resident_initcode_without_chunk_tariffs() {
    let (stop, spent, account) = create(Bytes::from_static(&[0xe0, 0x80, 0x81, 0xfe, 0]), 100_000);
    assert_eq!(stop, InstrStop::Stop);
    assert_eq!(spent, 2);
    assert!(account.is_some());
}

#[test]
fn rjumpi_executes_in_resident_initcode() {
    for condition in [0, 1] {
        let (stop, spent, account) =
            create(Bytes::from(vec![0x60, condition, 0xe1, 0x80, 0x81, 0, 0]), 100_000);
        assert_eq!(stop, InstrStop::Stop);
        assert_eq!(spent, 7);
        assert!(account.is_some());
    }
}
