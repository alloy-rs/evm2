//! TIP1143-T36: executable public creation-to-provider integration example.
//! PendingState and CacheDB are production transition consumers; hashes and
//! payload ranges below are calculated directly from original submitted bytes.

use alloy_primitives::{Address, Bytes, TxKind, U256, keccak256};
use evm2::{
    BaseEvmTypes, Evm, ExecutionConfig, Precompiles, SpecId, Version,
    bytecode::Bytecode,
    env::{BlockEnvExt, TxEnvExt},
    ethereum::{execute_initial_frame, prepare_initial_frame, validate_create_initcode},
    evm::{AccountInfo, DynDatabase, InMemoryDB, PendingState, SystemTx},
    interpreter::{GasTracker, InstrStop},
    registry::TxRegistry,
};

const CHUNK: usize = 24541;

#[test]
fn tip1143_t36_public_creation_transition_full_code_and_sparse_chunk_contract() {
    for multi in [false, true] {
        let mut runtime = if multi { vec![0x61, 0xbf, 0xba, 0x56] } else { Vec::new() };
        if multi {
            runtime.resize(2 * CHUNK, 0);
            runtime.push(0x5b);
        }
        runtime.extend([0x60, 42, 0x60, 0, 0x52, 0x60, 32, 0x60, 0, 0xf3]);
        let size = runtime.len();
        let mut input = vec![
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
        input.extend_from_slice(&runtime);
        let input = Bytes::from(input);
        let caller = Address::repeat_byte(0x55);
        let owner = caller.create(0);
        let version = Version::new(SpecId::PRAGUE).with_tip1143(true);
        validate_create_initcode(&version, TxKind::Create, &input).unwrap();
        let config = ExecutionConfig::for_spec_and_version(SpecId::PRAGUE, version);
        let mut creator = Evm::<'_, BaseEvmTypes>::new_with_execution_config(
            config.clone(),
            SpecId::PRAGUE,
            BlockEnvExt::default(),
            TxRegistry::new(),
            InMemoryDB::default(),
            Precompiles::base(SpecId::PRAGUE),
        );
        let mut gas = GasTracker::new(20_000_000);
        let frame = prepare_initial_frame(
            &mut creator,
            caller,
            0,
            TxKind::Create,
            &input,
            U256::ZERO,
            &mut gas,
        )
        .unwrap();
        let deployed = execute_initial_frame(
            &mut creator,
            &TxEnvExt::default(),
            frame,
            &mut gas,
            20_000_000,
            0,
        )
        .unwrap();
        assert!(deployed.stop.is_success());
        assert_eq!(deployed.created_address, Some(owner));
        let info = creator.state_mut().account(&owner).unwrap().get().unwrap().clone();
        assert_eq!(info.code_hash, keccak256(&runtime));
        assert_eq!(info.code.as_ref().unwrap().original_byte_slice(), runtime);
        if multi {
            let metadata = info.code_metadata().unwrap();
            assert_eq!(metadata.code_size() as usize, size);
            assert_eq!(
                metadata.chunk_hashes(),
                runtime.chunks(CHUNK).map(keccak256).collect::<Vec<_>>()
            );
        } else {
            assert!(info.code_metadata().is_none());
        }
        // Creation validates/hashes every original byte but must not construct
        // execution analysis eagerly, even though it retains the original output.
        assert!(creator.state_mut().account(&owner).unwrap().code_chunks().is_empty());
        // Execute before any provider has persisted the newly created code.
        // The middle payload of the three-chunk runtime is never requested.
        let immediate = creator.execute_system_call(SystemTx::new(owner, Bytes::new())).unwrap();
        assert_eq!(immediate.stop, InstrStop::Return);
        assert_eq!(immediate.output.len(), 32);
        assert_eq!(immediate.output[31], 42);
        {
            let account = creator.state_mut().account(&owner).unwrap();
            let mut requested = account.code_chunks().keys().copied().collect::<Vec<_>>();
            requested.sort_unstable();
            assert_eq!(requested, if multi { vec![0, 2] } else { vec![0] });
        }

        // Exercise the public transition source and both supported cache consumers.
        let mut pending = PendingState::default();
        pending.insert_account(owner, None, Some(info.clone()));
        for streamed in [false, true] {
            let mut provider = InMemoryDB::default();
            if streamed {
                provider.commit_source(&pending);
            } else {
                provider.commit_pending(&pending);
            }
            let restored = provider.account_info(&owner).unwrap();
            assert_eq!(restored.code_hash, info.code_hash);
            assert_eq!(restored.code_metadata(), info.code_metadata());
            assert_eq!((restored.nonce, restored.balance), (info.nonce, info.balance));
            let full = provider.get_code_by_hash(&info.code_hash).unwrap();
            assert_eq!(full.original_byte_slice(), runtime);
            assert_eq!(keccak256(full.original_byte_slice()), info.code_hash);
            for (index, expected) in runtime.chunks(CHUNK).enumerate() {
                let payload = provider
                    .get_code_chunk_by_hash(&info.code_hash, index as u32)
                    .unwrap()
                    .unwrap();
                assert_eq!(payload.original_bytes().as_ref(), expected);
            }
            assert!(
                provider
                    .get_code_chunk_by_hash(&info.code_hash, size.div_ceil(CHUNK) as u32)
                    .unwrap()
                    .is_none()
            );
            let mut fresh = Evm::<'_, BaseEvmTypes>::new_with_execution_config(
                config.clone(),
                SpecId::PRAGUE,
                BlockEnvExt::default(),
                TxRegistry::new(),
                provider,
                Precompiles::base(SpecId::PRAGUE),
            );
            let result = fresh.system_call(SystemTx::new(owner, Bytes::new())).unwrap().discard();
            assert_eq!(result.stop, InstrStop::Return);
            assert_eq!(result.output, immediate.output);
            assert_eq!(result.total_gas_spent, if multi { 2 * 28680 + 30 } else { 28680 + 18 });
            assert!(result.logs.is_empty());
        }
    }
}

#[test]
fn tip1143_t28_actual_detached_creation_handoff_and_enclosing_revert() {
    let factory = Address::repeat_byte(0x71);
    let child = factory.create(1);
    let mut runtime = vec![0x61, 0x5f, 0xdd, 0x56];
    runtime.resize(CHUNK, 0);
    runtime.extend([0x5b, 0x60, 42, 0x60, 0, 0x52, 0x60, 32, 0x60, 0, 0xf3]);
    let size = runtime.len();
    let mut input = vec![
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
    input.extend_from_slice(&runtime);
    for revert in [false, true] {
        // Calldata carries resident initcode; the factory itself fits a legacy row.
        // CREATE followed by CALL demonstrates execution before provider publication.
        let mut factory_code = vec![
            0x36, 0x60, 0, 0x60, 0, 0x37, 0x36, 0x60, 0, 0x60, 0, 0xf0, 0x50, 0x60, 32, 0x60, 0,
            0x60, 0, 0x60, 0, 0x60, 0, 0x73,
        ];
        factory_code.extend_from_slice(child.as_slice());
        factory_code.extend([
            0x62,
            3,
            0x0d,
            0x40,
            0xf1,
            0x60,
            32,
            0x52,
            0x60,
            64,
            0x60,
            0,
            if revert { 0xfd } else { 0xf3 },
        ]);
        let mut provider = InMemoryDB::default();
        provider.insert_account_info(
            &factory,
            AccountInfo {
                nonce: 1,
                code_hash: keccak256(&factory_code),
                code: Some(Bytecode::new_legacy(factory_code.into())),
                ..Default::default()
            },
        );
        let config = ExecutionConfig::for_spec_and_version(
            SpecId::PRAGUE,
            Version::new(SpecId::PRAGUE).with_tip1143(true),
        );
        let mut creator = Evm::<'_, BaseEvmTypes>::new_with_execution_config(
            config.clone(),
            SpecId::PRAGUE,
            BlockEnvExt::default(),
            TxRegistry::new(),
            provider,
            Precompiles::base(SpecId::PRAGUE),
        );
        let detached = creator
            .system_call(SystemTx::new(factory, input.clone().into()).with_gas_limit(20_000_000))
            .unwrap()
            .detach();
        assert_eq!(
            detached.result.stop,
            if revert { InstrStop::Revert } else { InstrStop::Return }
        );
        assert_eq!(detached.result.output.len(), 64);
        assert_eq!(U256::from_be_slice(&detached.result.output[..32]), U256::from(42));
        assert_eq!(U256::from_be_slice(&detached.result.output[32..]), U256::from(1));
        let mut restored = InMemoryDB::default();
        restored.commit_source(&detached.pending_state);
        if revert {
            assert!(detached.pending_state.account_info(&child).is_none());
            assert!(restored.account_info(&child).is_none());
            continue;
        }
        let info = detached.pending_state.account_info(&child).unwrap();
        assert_eq!(info.code_hash, keccak256(&runtime));
        assert_eq!(info.code.as_ref().unwrap().original_byte_slice(), runtime);
        let metadata = info.code_metadata().unwrap();
        assert_eq!(metadata.code_size() as usize, size);
        assert_eq!(
            metadata.chunk_hashes(),
            runtime.chunks(CHUNK).map(keccak256).collect::<Vec<_>>()
        );
        assert_eq!(
            restored.get_code_by_hash(&info.code_hash).unwrap().original_byte_slice(),
            runtime
        );
        let mut fresh = Evm::<'_, BaseEvmTypes>::new_with_execution_config(
            config,
            SpecId::PRAGUE,
            BlockEnvExt::default(),
            TxRegistry::new(),
            restored,
            Precompiles::base(SpecId::PRAGUE),
        );
        let result = fresh.system_call(SystemTx::new(child, Bytes::new())).unwrap().discard();
        assert_eq!(result.stop, InstrStop::Return);
        assert_eq!(result.output.as_ref(), &detached.result.output[..32]);
        assert_eq!(result.total_gas_spent, 57360 + 30);
    }
}
