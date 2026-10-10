//! Delegation is determined entirely by account extensions, without marker reads.

use alloy_primitives::{Address, B256, Bytes, U256};
use evm2::{
    BaseEvmTypes, Evm, ExecutionConfig, Precompiles, SpecId, Version,
    bytecode::{Bytecode, CodeChunk},
    env::BlockEnvExt,
    ethereum::validate_sender,
    evm::{AccountInfo, Database, Db, SystemTx},
    interpreter::{InstrStop, Word},
    registry::{HandlerError, TxRegistry},
};
use std::{cell::RefCell, collections::BTreeMap, io, rc::Rc};

#[derive(Clone, Default)]
struct Provider {
    accounts: BTreeMap<Address, AccountInfo>,
    records: BTreeMap<B256, Bytecode>,
    payloads: Rc<RefCell<Vec<B256>>>,
}

impl Provider {
    fn insert(&mut self, owner: Address, code: Bytecode, inline: bool) {
        let hash = code.hash_slow();
        self.accounts.insert(
            owner,
            AccountInfo {
                nonce: 1,
                code_hash: hash,
                code: None,
                extension: code
                    .eip7702_address()
                    .map(evm2::evm::AccountExtension::delegated)
                    .unwrap_or_default(),
                ..Default::default()
            },
        );
        if !inline {
            self.records.insert(hash, code);
        }
    }
}

impl Database for Provider {
    type Error = io::Error;
    fn get_account(&mut self, address: &Address) -> Result<Option<AccountInfo>, Self::Error> {
        Ok(self.accounts.get(address).cloned())
    }
    fn get_code_by_hash(&mut self, hash: &B256) -> Result<Bytecode, Self::Error> {
        self.payloads.borrow_mut().push(*hash);
        self.records.get(hash).cloned().ok_or_else(|| io::Error::other("missing payload"))
    }
    fn get_code_chunk_by_hash(
        &mut self,
        _: &B256,
        _: u32,
    ) -> Result<Option<CodeChunk>, Self::Error> {
        panic!("these accounts use explicit legacy records or inline metadata")
    }
    fn get_storage(&mut self, _: &Address, _: &Word) -> Result<Word, Self::Error> {
        Ok(Word::ZERO)
    }
    fn get_block_hash(&mut self, _: &Word) -> Result<B256, Self::Error> {
        Ok(B256::ZERO)
    }
}

fn execution(provider: Provider) -> Evm<'static, BaseEvmTypes> {
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

#[test]
fn ordinary_sender_is_rejected_from_account_metadata_without_payload_reads() {
    let owner = Address::repeat_byte(0x44);
    let code = Bytecode::new_legacy(Bytes::from_static(&[0]));
    let mut provider = Provider::default();
    provider.insert(owner, code, false);
    let payloads = provider.payloads.clone();
    let mut evm = execution(provider);
    for _ in 0..2 {
        assert!(matches!(
            validate_sender(&mut evm, owner, 1, U256::ZERO),
            Err(HandlerError::RejectCallerWithCode)
        ));
    }
    assert!(payloads.borrow().is_empty());
    assert!(!evm.state_mut().code_chunk_is_warm(&owner, 0));
}

#[test]
fn empty_extension_never_uses_a_bytecode_kind_as_delegation_metadata() {
    let owner = Address::repeat_byte(0x44);
    let target = Address::repeat_byte(0x55);
    let marker = Bytecode::new_eip7702(target);
    let hash = marker.hash_slow();
    let mut provider = Provider::default();
    provider.insert(owner, marker.clone(), false);
    // Deliberately inconsistent cached code kind: the empty extension is authoritative.
    let info = provider.accounts.get_mut(&owner).unwrap();
    info.extension = evm2::evm::AccountExtension::new();
    info.code = Some(marker);
    let payloads = provider.payloads.clone();
    let mut evm = execution(provider);
    assert!(matches!(
        validate_sender(&mut evm, owner, 1, U256::ZERO),
        Err(HandlerError::RejectCallerWithCode)
    ));
    assert!(payloads.borrow().is_empty());
    let result = evm.execute_system_call(SystemTx::new(owner, Bytes::new())).unwrap();
    assert_eq!(result.stop, InstrStop::OpcodeNotFound);
    assert_eq!(evm.state_mut().account(&owner).unwrap().code_hash(), hash);
    assert!(!evm.state_mut().code_chunk_is_warm(&target, 0));
}

#[test]
fn delegation_ignores_marker_residency_and_never_reads_it() {
    let owner = Address::repeat_byte(0x44);
    let target = Address::repeat_byte(0x55);
    let marker = Bytecode::new_eip7702(target);
    let target_code = Bytecode::new_legacy(Bytes::from_static(&[
        0x60, 42, 0x60, 0, 0x52, 0x60, 32, 0x60, 0, 0xf3,
    ]));
    let mut results = Vec::new();
    for inline in [false, true] {
        let mut provider = Provider::default();
        provider.insert(owner, marker.clone(), inline);
        provider.insert(target, target_code.clone(), false);
        let payloads = provider.payloads.clone();
        let mut evm = execution(provider);
        let result = evm.execute_system_call(SystemTx::new(owner, Bytes::new())).unwrap();
        assert_eq!(result.stop, InstrStop::Return);
        assert_eq!(Word::from_be_slice(&result.output), Word::from(42));
        assert!(!evm.state_mut().code_chunk_is_warm(&owner, 0));
        assert!(evm.state_mut().code_chunk_is_warm(&target, 0));
        assert_eq!(*payloads.borrow(), vec![target_code.hash_slow()]);
        results.push(result.total_gas_spent);
    }
    assert_eq!(results, [28_680 + 18, 28_680 + 18]);
}

#[test]
fn account_delegation_follows_exactly_one_hop() {
    let owner = Address::repeat_byte(0x44);
    let target = Address::repeat_byte(0x55);
    let final_target = Address::repeat_byte(0x66);
    let marker = Bytecode::new_eip7702(target);
    let second_marker = Bytecode::new_eip7702(final_target);
    let mut provider = Provider::default();
    provider.insert(owner, marker, false);
    provider.insert(target, second_marker, false);
    provider.insert(final_target, Bytecode::new_legacy(Bytes::from_static(&[0])), false);
    let payloads = provider.payloads.clone();
    let mut evm = execution(provider);
    let result = evm.execute_system_call(SystemTx::new(owner, Bytes::new())).unwrap();
    assert_eq!(result.stop, InstrStop::OpcodeNotFound);
    assert!(payloads.borrow().is_empty());
}

#[test]
fn extcode_operations_synthesize_marker_without_marker_reads_or_chunk_gas() {
    let owner = Address::repeat_byte(0x44);
    let caller = Address::repeat_byte(0x77);
    let marker = Bytecode::new_eip7702(Address::repeat_byte(0x55));
    let mut driver = vec![0x73];
    driver.extend_from_slice(owner.as_slice());
    driver.extend([0x3b, 0x5f, 0x52, 0x60, 23, 0x5f, 0x60, 32, 0x73]);
    driver.extend_from_slice(owner.as_slice());
    driver.extend([0x3c, 0x60, 55, 0x5f, 0xf3]);
    let driver = Bytecode::new_legacy(driver.into());
    let mut gas = Vec::new();
    for inline in [false, true] {
        let mut provider = Provider::default();
        provider.insert(owner, marker.clone(), inline);
        provider.insert(caller, driver.clone(), false);
        let payloads = provider.payloads.clone();
        let mut evm = execution(provider);
        let result = evm.execute_system_call(SystemTx::new(caller, Bytes::new())).unwrap();
        assert_eq!(result.stop, InstrStop::Return);
        assert_eq!(Word::from_be_slice(&result.output[..32]), Word::from(23));
        assert_eq!(&result.output[32..], marker.original_byte_slice());
        assert!(!evm.state_mut().code_chunk_is_warm(&owner, 0));
        assert_eq!(*payloads.borrow(), vec![driver.hash_slow()]);
        gas.push(result.total_gas_spent);
    }
    assert_eq!(gas[0], gas[1]);
}
