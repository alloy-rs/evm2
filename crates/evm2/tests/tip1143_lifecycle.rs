//! TIP-1143 owner warmth and rollback through production state checkpoints.
//! The provider ledger counts backing reads independently of journal/cache observations.

use alloy_primitives::{Address, B256, Bytes, keccak256};
use evm2::{
    BaseEvmTypes, Evm, ExecutionConfig, Precompiles, SpecId, Version,
    bytecode::{Bytecode, CodeChunk, CodeMetadata},
    env::BlockEnvExt,
    ethereum::validate_sender,
    evm::{AccountInfo, Database, Db, SystemTx},
    interpreter::{InstrStop, Word},
    registry::{HandlerError, TxRegistry},
};
use std::{cell::RefCell, io, rc::Rc};

const CHUNK: usize = 24541;
const COLD: u64 = 28680;
const WARM: u64 = 1000;
const ORDINARY: u64 = 30;

#[derive(Clone, Debug)]
struct Provider {
    code: Bytes,
    reads: Rc<RefCell<Vec<(B256, u32)>>>,
}

impl Database for Provider {
    type Error = io::Error;

    fn get_account(&mut self, address: &Address) -> Result<Option<AccountInfo>, Self::Error> {
        if ![Address::repeat_byte(0x44), Address::repeat_byte(0x45)].contains(address) {
            return Ok(None);
        }
        Ok(Some(AccountInfo {
            nonce: 7,
            balance: Word::from(1234),
            code_hash: keccak256(&self.code),
            extension: ((self.code.len() > CHUNK).then(|| {
                CodeMetadata::new(
                    self.code.len() as u32,
                    self.code.chunks(CHUNK).map(keccak256).collect(),
                )
                .unwrap()
            }))
            .map(evm2::evm::AccountExtension::chunked)
            .unwrap_or_else(|| {
                Bytecode::new_eip7702_raw(self.code.clone())
                    .ok()
                    .and_then(|code| code.eip7702_address())
                    .map(evm2::evm::AccountExtension::delegated)
                    .unwrap_or_default()
            }),
            ..Default::default()
        }))
    }

    fn get_code_by_hash(&mut self, _: &B256) -> Result<Bytecode, Self::Error> {
        assert!(self.code.len() <= CHUNK);
        self.reads.borrow_mut().push((keccak256(&self.code), 0));
        Ok(Bytecode::new_eip7702_raw(self.code.clone())
            .unwrap_or_else(|_| Bytecode::new_legacy(self.code.clone())))
    }

    fn get_storage(&mut self, _: &Address, _: &Word) -> Result<Word, Self::Error> {
        Ok(Word::ZERO)
    }

    fn get_block_hash(&mut self, _: &Word) -> Result<B256, Self::Error> {
        Ok(B256::ZERO)
    }

    fn get_code_chunk_by_hash(
        &mut self,
        hash: &B256,
        index: u32,
    ) -> Result<Option<CodeChunk>, Self::Error> {
        assert_eq!(*hash, keccak256(&self.code));
        self.reads.borrow_mut().push((*hash, index));
        Ok(evm2::bytecode::code_chunk(&self.code, index))
    }
}

fn fixture() -> Provider {
    let mut code = vec![0x61, 0x5f, 0xdd, 0x56];
    code.resize(CHUNK, 0);
    code.extend([0x5b, 0x60, 42, 0x60, 0, 0x52, 0x60, 32, 0x60, 0, 0xf3]);
    Provider { code: code.into(), reads: Rc::default() }
}

fn evm(provider: Provider) -> Evm<'static, BaseEvmTypes> {
    Evm::new_with_execution_config(
        ExecutionConfig::for_spec_and_version(
            SpecId::PRAGUE,
            Version::new(SpecId::PRAGUE).with_tip1143(true),
        ),
        SpecId::PRAGUE,
        BlockEnvExt::default(),
        TxRegistry::new(),
        Db::new(provider),
        Precompiles::base(SpecId::PRAGUE),
    )
}

fn call(evm: &mut Evm<'_, BaseEvmTypes>, owner: Address, chunk_cost: u64) {
    // Handler-level execution intentionally preserves transaction state between calls.
    let result = evm.execute_system_call(SystemTx::new(owner, Bytes::new())).unwrap();
    assert_eq!(result.stop, InstrStop::Return);
    assert_eq!(result.total_gas_spent, ORDINARY + chunk_cost);
    assert_eq!(result.output.len(), 32);
    assert_eq!(result.output[31], 42);
    assert!(result.output[..31].iter().all(|byte| *byte == 0));
    assert!(result.logs.is_empty());
}

#[test]
fn tip1143_t24_equal_code_owners_have_independent_warmth_and_shared_bytes() {
    let provider = fixture();
    let reads = provider.reads.clone();
    let hash = keccak256(&provider.code);
    let mut execution = evm(provider);
    let a = Address::repeat_byte(0x44);
    let b = Address::repeat_byte(0x45);
    call(&mut execution, a, 2 * COLD);
    call(&mut execution, b, 2 * COLD);
    call(&mut execution, a, 2 * WARM);
    assert_eq!(*reads.borrow(), [(hash, 0), (hash, 1)]);
}

#[test]
fn tip1143_t25_checkpoint_reverts_new_warmth_and_retains_earlier_warmth() {
    let provider = fixture();
    let reads = provider.reads.clone();
    let hash = keccak256(&provider.code);
    let mut execution = evm(provider);
    let features = execution.version().features;
    let a = Address::repeat_byte(0x44);
    let b = Address::repeat_byte(0x45);
    call(&mut execution, a, 2 * COLD);
    let outer = execution.state_mut().checkpoint();
    call(&mut execution, b, 2 * COLD);
    let inner = execution.state_mut().checkpoint();
    call(&mut execution, a, 2 * WARM);
    call(&mut execution, b, 2 * WARM);
    execution.state_mut().rollback(inner, features);
    call(&mut execution, a, 2 * WARM);
    call(&mut execution, b, 2 * WARM);
    execution.state_mut().rollback(outer, features);
    call(&mut execution, a, 2 * WARM);
    call(&mut execution, b, 2 * COLD);
    assert_eq!(*reads.borrow(), [(hash, 0), (hash, 1)]);
}

#[test]
fn tip1143_t26_reset_retains_bytes_but_discards_logical_warmth() {
    let provider = fixture();
    let reads = provider.reads.clone();
    let hash = keccak256(&provider.code);
    let mut execution = evm(provider);
    let owner = Address::repeat_byte(0x44);
    for _ in 0..3 {
        call(&mut execution, owner, 2 * COLD);
        call(&mut execution, owner, 2 * WARM);
        execution.state_mut().clear_transaction_state();
    }
    assert_eq!(*reads.borrow(), [(hash, 0), (hash, 1)]);
}

#[test]
fn tip1143_t27_multi_to_single_or_empty_rollback_restores_identity_and_warmth() {
    for replacement in
        [Bytes::new(), Bytes::from_static(&[0x60, 99, 0x60, 0, 0x52, 0x60, 32, 0x60, 0, 0xf3])]
    {
        let provider = fixture();
        let original_hash = keccak256(&provider.code);
        let reads = provider.reads.clone();
        let mut execution = evm(provider);
        let owner = Address::repeat_byte(0x44);
        let features = execution.version().features;
        call(&mut execution, owner, 2 * COLD);
        let before = execution.state_mut().account(&owner).unwrap().get().unwrap().clone();
        let checkpoint = execution.state_mut().checkpoint();
        execution
            .state_mut()
            .account(&owner)
            .unwrap()
            .set_code_slow(Bytecode::new_legacy(replacement.clone()));
        let replaced = execution.state_mut().account(&owner).unwrap().get().unwrap().clone();
        assert_eq!(replaced.code_hash, keccak256(&replacement));
        assert!(replaced.code_metadata().is_none());
        assert_eq!((replaced.nonce, replaced.balance), (7, Word::from(1234)));
        let result = execution.execute_system_call(SystemTx::new(owner, Bytes::new())).unwrap();
        if replacement.is_empty() {
            assert_eq!(result.stop, InstrStop::Stop);
            assert_eq!(result.total_gas_spent, 0);
            assert!(result.output.is_empty());
        } else {
            assert_eq!(result.stop, InstrStop::Return);
            assert_eq!(result.total_gas_spent, COLD + 18);
            assert_eq!(result.output.len(), 32);
            assert_eq!(result.output[31], 99);
        }
        execution.state_mut().rollback(checkpoint, features);
        let restored = execution.state_mut().account(&owner).unwrap().get().unwrap().clone();
        assert_eq!(restored.code_hash, original_hash);
        assert_eq!(restored.code_metadata(), before.code_metadata());
        assert_eq!(restored.extension, before.extension);
        assert_eq!((restored.nonce, restored.balance), (before.nonce, before.balance));
        call(&mut execution, owner, 2 * WARM);
        assert_eq!(*reads.borrow(), [(original_hash, 0), (original_hash, 1)]);
    }
}

#[test]
fn tip1143_t27_single_and_empty_origins_restore_after_replacement() {
    let marker = Bytes::from_static(&[0x60, 42, 0x60, 0, 0x52, 0x60, 32, 0x60, 0, 0xf3]);
    let other = Bytes::from_static(&[0x60, 99, 0x60, 0, 0x52, 0x60, 32, 0x60, 0, 0xf3]);
    for original in [Bytes::new(), marker] {
        for replacement in [Bytes::new(), other.clone()] {
            let provider = Provider { code: original.clone(), reads: Rc::default() };
            let reads = provider.reads.clone();
            let mut execution = evm(provider);
            let owner = Address::repeat_byte(0x44);
            let features = execution.version().features;
            let first = execution.execute_system_call(SystemTx::new(owner, Bytes::new())).unwrap();
            assert_eq!(first.total_gas_spent, if original.is_empty() { 0 } else { COLD + 18 });
            assert_eq!(
                first.stop,
                if original.is_empty() { InstrStop::Stop } else { InstrStop::Return }
            );
            let original_info =
                execution.state_mut().account(&owner).unwrap().get().unwrap().clone();
            assert!(original_info.code_metadata().is_none());
            let checkpoint = execution.state_mut().checkpoint();
            execution
                .state_mut()
                .account(&owner)
                .unwrap()
                .set_code_slow(Bytecode::new_legacy(replacement.clone()));
            let changed =
                execution.execute_system_call(SystemTx::new(owner, Bytes::new())).unwrap();
            assert_eq!(changed.total_gas_spent, if replacement.is_empty() { 0 } else { COLD + 18 });
            if replacement.is_empty() {
                assert_eq!(changed.stop, InstrStop::Stop);
                assert!(changed.output.is_empty());
            } else {
                assert_eq!(changed.stop, InstrStop::Return);
                assert_eq!(changed.output.len(), 32);
                assert_eq!(changed.output[31], 99);
            }
            let changed_info =
                execution.state_mut().account(&owner).unwrap().get().unwrap().clone();
            assert_eq!(changed_info.code_hash, keccak256(&replacement));
            assert!(changed_info.code_metadata().is_none());
            assert_eq!((changed_info.nonce, changed_info.balance), (7, Word::from(1234)));
            execution.state_mut().rollback(checkpoint, features);
            let restored =
                execution.execute_system_call(SystemTx::new(owner, Bytes::new())).unwrap();
            assert_eq!(restored.stop, first.stop);
            assert_eq!(restored.output, first.output);
            assert_eq!(restored.total_gas_spent, if original.is_empty() { 0 } else { WARM + 18 });
            let restored_info =
                execution.state_mut().account(&owner).unwrap().get().unwrap().clone();
            assert_eq!(restored_info.code_hash, original_info.code_hash);
            assert_eq!(restored_info.code_metadata(), original_info.code_metadata());
            assert_eq!(restored_info.extension, original_info.extension);
            assert_eq!((restored_info.nonce, restored_info.balance), (7, Word::from(1234)));
            let expected =
                if original.is_empty() { vec![] } else { vec![(keccak256(&original), 0)] };
            assert_eq!(*reads.borrow(), expected);
        }
    }
}

#[test]
fn tip1143_t27_t38_every_origin_to_new_multi_code_restores_previous_execution() {
    let single = Bytes::from_static(&[0x60, 42, 0x60, 0, 0x52, 0x60, 32, 0x60, 0, 0xf3]);
    for original in [Bytes::new(), single, fixture().code] {
        let mut replacement = vec![0x61, 0xbf, 0xba, 0x56];
        replacement.resize(2 * CHUNK, 0);
        replacement.extend([0x5b, 0x60, 99, 0x60, 0, 0x52, 0x60, 32, 0x60, 0, 0xf3]);
        let provider = Provider { code: original.clone(), reads: Rc::default() };
        let reads = provider.reads.clone();
        let mut execution = evm(provider);
        let owner = Address::repeat_byte(0x44);
        let features = execution.version().features;
        let before = execution.execute_system_call(SystemTx::new(owner, Bytes::new())).unwrap();
        let info = execution.state_mut().account(&owner).unwrap().get().unwrap().clone();
        let checkpoint = execution.state_mut().checkpoint();
        execution
            .state_mut()
            .account(&owner)
            .unwrap()
            .set_chunked_code(Bytecode::new_legacy(Bytes::copy_from_slice(&replacement)))
            .unwrap();
        let changed = execution.state_mut().account(&owner).unwrap().get().unwrap().clone();
        assert_eq!(changed.code_hash, keccak256(&replacement));
        let metadata = changed.code_metadata().unwrap();
        assert_eq!(metadata.code_size() as usize, replacement.len());
        assert_eq!(
            metadata.chunk_hashes(),
            replacement.chunks(CHUNK).map(keccak256).collect::<Vec<_>>()
        );
        assert_eq!((changed.nonce, changed.balance), (info.nonce, info.balance));
        for tariff in [COLD, WARM] {
            let result = execution.execute_system_call(SystemTx::new(owner, Bytes::new())).unwrap();
            assert_eq!(result.stop, InstrStop::Return);
            assert_eq!(result.output.len(), 32);
            assert_eq!(result.output[31], 99);
            assert_eq!(result.total_gas_spent, 2 * tariff + ORDINARY);
        }
        execution.state_mut().rollback(checkpoint, features);
        let restored = execution.execute_system_call(SystemTx::new(owner, Bytes::new())).unwrap();
        assert_eq!(restored.stop, before.stop);
        assert_eq!(restored.output, before.output);
        assert_eq!(
            restored.total_gas_spent,
            match original.len() {
                0 => 0,
                1..=CHUNK => WARM + 18,
                _ => 2 * WARM + ORDINARY,
            }
        );
        let restored_info = execution.state_mut().account(&owner).unwrap().get().unwrap().clone();
        assert_eq!(restored_info.code_hash, info.code_hash);
        assert_eq!(restored_info.code_metadata(), info.code_metadata());
        assert_eq!(restored_info.extension, info.extension);
        // Replacement is resident newly supplied code, so no provider row may
        // be consulted under either the replacement hash or an old identity.
        assert_eq!(
            *reads.borrow(),
            (0..original.len().div_ceil(CHUNK))
                .map(|index| (keccak256(&original), index as u32))
                .collect::<Vec<_>>()
        );
    }
}

fn invoke(code: &mut Vec<u8>, owner: Address, opcode: u8) {
    // Empty input, 32-byte output, non-value call with ample child gas.
    code.extend([0x60, 32, 0x60, 0, 0x60, 0, 0x60, 0]);
    if matches!(opcode, 0xf1 | 0xf2) {
        code.extend([0x60, 0]);
    }
    code.push(0x73);
    code.extend_from_slice(owner.as_slice());
    code.extend([0x62, 3, 0x0d, 0x40, opcode]);
}

#[test]
fn tip1143_t24_delegate_callcode_and_staticcall_resolve_code_owner() {
    let parent = Address::repeat_byte(0x66);
    let owner = Address::repeat_byte(0x44);
    for opcode in [0xf1, 0xf2, 0xf4, 0xfa] {
        let mut provider = fixture();
        let mut child = vec![0x61, 0x5f, 0xdd, 0x56];
        child.resize(CHUNK, 0);
        // Store 42 at slot zero, then return ADDRESS to reveal execution context.
        child.extend([0x5b, 0x60, 42, 0x60, 0, 0x55, 0x30, 0x60, 0, 0x52, 0x60, 32, 0x60, 0, 0xf3]);
        provider.code = child.into();
        let hash = keccak256(&provider.code);
        let reads = provider.reads.clone();
        let mut execution = evm(provider);
        let mut parent_code = Vec::new();
        invoke(&mut parent_code, owner, opcode);
        parent_code.extend([0x60, 32, 0x52, 0x60, 64, 0x60, 0, 0xf3]);
        execution
            .state_mut()
            .account(&parent)
            .unwrap()
            .set_code_slow(Bytecode::new_legacy(parent_code.into()));
        let result = execution.execute_system_call(SystemTx::new(parent, Bytes::new())).unwrap();
        assert_eq!(result.stop, InstrStop::Return);
        assert_eq!(result.output.len(), 64);
        let successful = opcode != 0xfa;
        assert_eq!(Word::from_be_slice(&result.output[32..]), Word::from(u8::from(successful)));
        let context = if opcode == 0xf1 { owner } else { parent };
        assert_eq!(
            Word::from_be_slice(&result.output[..32]),
            if successful { Word::from_be_slice(context.as_slice()) } else { Word::ZERO }
        );
        for address in [parent, owner] {
            let actual = execution
                .state_mut()
                .account(&address)
                .unwrap()
                .storage()
                .into_slot(Word::ZERO)
                .unwrap()
                .current();
            assert_eq!(
                actual,
                if successful && address == context { Word::from(42) } else { Word::ZERO }
            );
        }
        let owner_account = execution.state_mut().account(&owner).unwrap();
        assert!(owner_account.code_chunks().get(&0).unwrap().is_warm);
        assert_eq!(owner_account.code_chunks().get(&1).is_some_and(|c| c.is_warm), successful);
        drop(owner_account);
        let parent_account = execution.state_mut().account(&parent).unwrap();
        assert_eq!(parent_account.code_chunks().len(), 1);
        assert_eq!(*reads.borrow(), [(hash, 0), (hash, 1)]);
        assert!(result.logs.is_empty());
    }
}

#[test]
fn tip1143_t25_real_child_failure_reverts_remote_but_not_precheckpoint_entry_warmth() {
    let parent = Address::repeat_byte(0x66);
    let owner = Address::repeat_byte(0x44);
    for terminator in [0xf3, 0xfd, 0xfe] {
        let mut provider = fixture();
        let mut child = vec![0x61, 0x5f, 0xdd, 0x56];
        child.resize(CHUNK, 0);
        child.extend([
            0x5b, 0x60, 42, 0x60, 0, 0x55, 0x60, 0, 0x60, 0, 0xa0, 0x60, 0, 0x60, 0, terminator,
        ]);
        provider.code = child.into();
        let hash = keccak256(&provider.code);
        let reads = provider.reads.clone();
        let mut execution = evm(provider);
        let mut parent_code = Vec::new();
        invoke(&mut parent_code, owner, 0xf1);
        parent_code.extend([0x60, 0, 0x52, 0x60, 32, 0x60, 0, 0xf3]);
        execution
            .state_mut()
            .account(&parent)
            .unwrap()
            .set_code_slow(Bytecode::new_legacy(parent_code.into()));
        for round in 0..2 {
            let result =
                execution.execute_system_call(SystemTx::new(parent, Bytes::new())).unwrap();
            assert_eq!(result.stop, InstrStop::Return);
            let success = terminator == 0xf3;
            assert_eq!(Word::from_be_slice(&result.output), Word::from(u8::from(success)));
            let account = execution.state_mut().account(&owner).unwrap();
            assert!(account.code_chunks().get(&0).unwrap().is_warm);
            assert_eq!(account.code_chunks().get(&1).is_some_and(|c| c.is_warm), success);
            drop(account);
            assert_eq!(
                execution
                    .state_mut()
                    .account(&owner)
                    .unwrap()
                    .storage()
                    .into_slot(Word::ZERO)
                    .unwrap()
                    .current(),
                if success { Word::from(42) } else { Word::ZERO }
            );
            assert_eq!(execution.state_mut().logs().len(), if success { round + 1 } else { 0 });
            assert_eq!(*reads.borrow(), [(hash, 0), (hash, 1)]);
        }
    }
}

#[test]
fn tip1143_t25_successful_child_inside_reverted_parent_loses_all_new_child_warmth() {
    let parent = Address::repeat_byte(0x66);
    let owner = Address::repeat_byte(0x44);
    for prewarm in [false, true] {
        let provider = fixture();
        let hash = keccak256(&provider.code);
        let reads = provider.reads.clone();
        let mut execution = evm(provider);
        if prewarm {
            call(&mut execution, owner, 2 * COLD);
        }
        let mut code = Vec::new();
        invoke(&mut code, owner, 0xf1);
        code.extend([0x50, 0x60, 32, 0x60, 0, 0xfd]);
        execution
            .state_mut()
            .account(&parent)
            .unwrap()
            .set_code_slow(Bytecode::new_legacy(code.into()));
        let result = execution.execute_system_call(SystemTx::new(parent, Bytes::new())).unwrap();
        assert_eq!(result.stop, InstrStop::Revert);
        assert_eq!(result.output.len(), 32);
        assert_eq!(result.output[31], 42);
        assert!(result.logs.is_empty());
        call(&mut execution, owner, if prewarm { 2 * WARM } else { 2 * COLD });
        assert_eq!(*reads.borrow(), [(hash, 0), (hash, 1)]);
    }
}

#[test]
fn tip1143_t31_sender_validation_uses_normalized_account_extensions() {
    let delegate = Address::repeat_byte(0x99);
    let marker = Bytecode::new_eip7702(delegate).original_byte_slice().to_vec();
    let mut short = marker.clone();
    short.pop();
    let mut long = marker.clone();
    long.push(0);
    let mut wrong_prefix = marker.clone();
    wrong_prefix[2] = 2;
    for (code, accepted, expected_reads) in [
        (Bytes::new(), true, 0),
        (Bytes::from_static(&[0]), false, 0),
        (Bytes::from(marker), true, 0),
        (Bytes::from(short), false, 0),
        (Bytes::from(long), false, 0),
        (Bytes::from(wrong_prefix), false, 0),
        (fixture().code, false, 0),
    ] {
        let provider = Provider { code, reads: Rc::default() };
        let reads = provider.reads.clone();
        let mut execution = evm(provider);
        let owner = Address::repeat_byte(0x44);
        let result = validate_sender(&mut execution, owner, 7, Word::ZERO);
        if accepted {
            let info = result.unwrap();
            assert_eq!(info.nonce, 7);
            assert_eq!(info.balance, Word::from(1234));
        } else {
            assert!(matches!(result, Err(HandlerError::RejectCallerWithCode)));
        }
        assert_eq!(reads.borrow().len(), expected_reads);
        // Metadata-based rejection must never analyze an unexecuted multi chunk.
        if expected_reads == 0 {
            assert!(execution.state_mut().account(&owner).unwrap().code_chunks().is_empty());
        }
    }
}

#[test]
fn tip1143_t24_t31_distinct_delegators_share_resolved_owner_chunk_warmth() {
    let provider = fixture();
    let hash = keccak256(&provider.code);
    let reads = provider.reads.clone();
    let mut execution = evm(provider);
    let owner = Address::repeat_byte(0x44);
    let first = Address::repeat_byte(0x66);
    let second = Address::repeat_byte(0x67);
    for delegator in [first, second] {
        execution
            .state_mut()
            .account(&delegator)
            .unwrap()
            .set_code_slow(Bytecode::new_eip7702(owner));
    }
    for (delegator, tariff) in [(first, COLD), (second, WARM), (first, WARM)] {
        let result = execution.execute_system_call(SystemTx::new(delegator, Bytes::new())).unwrap();
        assert_eq!(result.stop, InstrStop::Return);
        assert_eq!(result.output.len(), 32);
        assert_eq!(result.output[31], 42);
        // Prague initial delegation resolution has no EIP-2780 account tariff.
        // Marker recognition is validation, not execution of the marker account.
        assert_eq!(result.total_gas_spent, ORDINARY + 2 * tariff);
        assert!(result.logs.is_empty());
        let account = execution.state_mut().account(&owner).unwrap();
        assert_eq!(account.code_chunks().len(), 2);
        assert!(account.code_chunks().values().all(|chunk| chunk.is_warm));
        drop(account);
        assert!(!execution.state_mut().account(&delegator).unwrap().code_chunks().contains_key(&1));
        assert_eq!(*reads.borrow(), [(hash, 0), (hash, 1)]);
    }
    // The resolved owner is warm even when entered directly after delegation.
    call(&mut execution, owner, 2 * WARM);
}

#[test]
fn tip1143_t35_snapshot_restores_both_representations_and_checkpoint_warmth() {
    for original in
        [Bytes::from_static(&[0x60, 42, 0x60, 0, 0x52, 0x60, 32, 0x60, 0, 0xf3]), fixture().code]
    {
        let owner = Address::repeat_byte(0x44);
        let provider = Provider { code: original.clone(), reads: Rc::default() };
        let reads = provider.reads.clone();
        let mut execution = evm(provider.clone());
        let first = execution.execute_system_call(SystemTx::new(owner, Bytes::new())).unwrap();
        let features = execution.version().features;
        let info = execution.state_mut().account(&owner).unwrap().get().unwrap().clone();
        let checkpoint = execution.state_mut().checkpoint();
        let snapshot = execution.state_mut().snapshot();
        execution
            .state_mut()
            .account(&owner)
            .unwrap()
            .set_code_slow(Bytecode::new_legacy(Bytes::new()));
        assert_eq!(
            execution
                .execute_system_call(SystemTx::new(owner, Bytes::new()))
                .unwrap()
                .total_gas_spent,
            0
        );
        *execution.state_mut() = snapshot.into_state(Db::new(provider));
        let restored = execution.state_mut().account(&owner).unwrap().get().unwrap().clone();
        assert_eq!(restored, info);
        execution.state_mut().rollback(checkpoint, features);
        let warm = execution.execute_system_call(SystemTx::new(owner, Bytes::new())).unwrap();
        assert_eq!(warm.stop, first.stop);
        assert_eq!(warm.output, first.output);
        let count = if original.len() > CHUNK { 2 } else { 1 };
        assert_eq!(warm.total_gas_spent, first.total_gas_spent - count * (COLD - WARM));
        execution.state_mut().clear_transaction_state();
        let cold = execution.execute_system_call(SystemTx::new(owner, Bytes::new())).unwrap();
        assert_eq!(cold.output, first.output);
        assert_eq!(cold.total_gas_spent, first.total_gas_spent);
        assert_eq!(
            *reads.borrow(),
            (0..count as u32).map(|index| (keccak256(&original), index)).collect::<Vec<_>>()
        );
    }
}

/// TIP1143-T24: all non-value call variants charge the resolved owner once per request.
#[test]
fn tip1143_t24_repeated_call_variants_have_exact_owner_tariffs() {
    let parent = Address::repeat_byte(0x66);
    let owner = Address::repeat_byte(0x44);
    for opcode in [0xf1, 0xf2, 0xf4, 0xfa] {
        let provider = fixture();
        let hash = keccak256(&provider.code);
        let reads = provider.reads.clone();
        let mut execution = evm(provider);
        // Normalize EIP-2929 warmth without loading or warming code chunks.
        execution.state_mut().account(&owner).unwrap().warm();
        assert!(execution.state_mut().account(&owner).unwrap().code_chunks().is_empty());
        let mut parent_code = Vec::new();
        for _ in 0..2 {
            invoke(&mut parent_code, owner, opcode);
            parent_code.push(0x50);
        }
        parent_code.extend([0x60, 32, 0x60, 0, 0xf3]);
        execution
            .state_mut()
            .account(&parent)
            .unwrap()
            .set_code_slow(Bytecode::new_legacy(parent_code.into()));
        let result = execution.execute_system_call(SystemTx::new(parent, Bytes::new())).unwrap();
        assert_eq!(result.stop, InstrStop::Return);
        assert_eq!(Word::from_be_slice(&result.output), Word::from(42));
        // Six/seven PUSHes, warm account CALL cost, child opcodes and POP per call;
        // one memory word expansion, then two PUSHes and RETURN in the parent.
        let pushes = if matches!(opcode, 0xf1 | 0xf2) { 21 } else { 18 };
        let ordinary = 2 * (pushes + 100 + ORDINARY + 2) + 3 + 6;
        assert_eq!(result.total_gas_spent, ordinary + 3 * COLD + 2 * WARM, "opcode={opcode:#x}");
        assert_eq!(*reads.borrow(), [(hash, 0), (hash, 1)]);
        assert!(
            execution
                .state_mut()
                .account(&owner)
                .unwrap()
                .code_chunks()
                .values()
                .all(|chunk| chunk.is_warm)
        );
        assert_eq!(execution.state_mut().account(&parent).unwrap().code_chunks().len(), 1);
        assert!(result.logs.is_empty());
    }
}

/// T31: sender recognition traverses persisted code conversion, not set_code_slow.
#[test]
fn tip1143_t31_persisted_kind_controls_sender_validation_and_delegation() {
    let owner = Address::repeat_byte(0x44);
    let delegate = Address::repeat_byte(0x77);
    let marker = Bytecode::new_eip7702(delegate);
    for delegation in [false, true] {
        let code =
            if delegation { marker.clone() } else { Bytecode::new_legacy(marker.original_bytes()) };
        let mut stored = evm2::evm::InMemoryDB::default();
        stored.insert_account_info(
            &owner,
            AccountInfo { nonce: 7, ..AccountInfo::default().with_code(code) },
        );
        stored.insert_account_info(
            &delegate,
            AccountInfo::default().with_code(Bytecode::new_legacy(Bytes::from_static(&[
                0x60, 42, 0x60, 0, 0x52, 0x60, 32, 0x60, 0, 0xf3,
            ]))),
        );
        let mut execution = Evm::<BaseEvmTypes>::new_with_execution_config(
            ExecutionConfig::for_spec_and_version(
                SpecId::PRAGUE,
                Version::new(SpecId::PRAGUE).with_tip1143(true),
            ),
            SpecId::PRAGUE,
            BlockEnvExt::default(),
            TxRegistry::new(),
            evm2::evm::CacheDB::new(Db::new(stored)),
            Precompiles::base(SpecId::PRAGUE),
        );
        let validation = validate_sender(&mut execution, owner, 7, Word::ZERO);
        if delegation {
            assert_eq!(validation.unwrap().nonce, 7);
        } else {
            assert!(matches!(validation, Err(HandlerError::RejectCallerWithCode)));
        }
        let result = execution.execute_system_call(SystemTx::new(owner, Bytes::new())).unwrap();
        if delegation {
            assert_eq!(result.stop, InstrStop::Return);
            assert_eq!(Word::from_be_slice(&result.output), Word::from(42));
        } else {
            assert_eq!(result.stop, InstrStop::OpcodeNotFound);
            assert!(result.output.is_empty());
        }
    }
}
