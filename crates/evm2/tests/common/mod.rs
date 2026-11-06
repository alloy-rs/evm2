#![allow(dead_code)]

use alloy_consensus::{TxEip8141, transaction::Recovered};
use alloy_eips::{
    eip8141::{Frame, FrameLimits, FrameMode, NONCE_MANAGER},
    eip8272::{
        RECENT_ROOT_ADDRESS, RECENT_ROOT_CODE, RECENT_ROOT_ENTRY_DOMAIN, RECENT_ROOT_STORAGE_DOMAIN,
    },
};
use alloy_primitives::{Address, B256, Bytes, U256, keccak256};
use evm2::{
    BaseEvmTypes, Evm, ExecutionConfig, InterpreterRunner, Precompiles, SpecId, Version,
    bytecode::Bytecode,
    env::BlockEnvExt,
    ethereum::{TxEnvelope, ethereum_tx_registry},
    evm::{AccountInfo, Database, InMemoryDB},
    interpreter::{InstrStop, Interpreter},
    registry::HandlerError,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

pub(crate) const SENDER: Address = Address::with_last_byte(0xa0);
pub(crate) const BODY: Address = Address::with_last_byte(0xb0);
pub(crate) const APPROVE_BOTH: &[u8] = &[0x60, 3, 0x5f, 0x5f, 0xaa];

pub(crate) fn frame(mode: FrameMode, target: Address) -> Frame {
    Frame {
        mode,
        target: target.into(),
        flags: if mode == FrameMode::Verify { 3 } else { 0 },
        limits: FrameLimits { execution: 100_000, state: 300_000 },
        ..Default::default()
    }
}

pub(crate) fn payload(frames: Vec<Frame>) -> TxEip8141 {
    TxEip8141 {
        chain_id: 1,
        sender: SENDER,
        nonce_keys: Some(vec![]),
        frames,
        ..Default::default()
    }
}

pub(crate) const fn recovered(tx: TxEip8141) -> Recovered<TxEnvelope> {
    Recovered::new_unchecked(TxEnvelope::Eip8141(tx), Address::ZERO)
}

pub(crate) fn account(code: &[u8]) -> AccountInfo {
    AccountInfo::default()
        .with_balance(U256::from(1_000_000_000))
        .with_code(Bytecode::new_legacy(Bytes::copy_from_slice(code)))
}

pub(crate) fn database<'a>(accounts: impl IntoIterator<Item = (Address, &'a [u8])>) -> InMemoryDB {
    let mut db = InMemoryDB::default();
    for (address, code) in accounts {
        db.insert_account_info(&address, account(code));
    }
    db
}

/// Builds funded test accounts with the nonce manager already present.
pub(crate) fn database_with_nonce_manager<'a>(
    accounts: impl IntoIterator<Item = (Address, &'a [u8])>,
) -> InMemoryDB {
    let mut db = database(accounts);
    db.insert_account_info(&NONCE_MANAGER, AccountInfo::default().with_nonce(1));
    db
}

pub(crate) fn evm(db: InMemoryDB) -> Evm<'static, BaseEvmTypes> {
    Evm::new(
        SpecId::BOGOTA,
        BlockEnvExt::default(),
        ethereum_tx_registry(SpecId::BOGOTA),
        db,
        Precompiles::base(SpecId::BOGOTA),
    )
}

pub(crate) fn delegating_sender(target: Address, parent_exit: &[u8]) -> Vec<u8> {
    let mut code = vec![0x5f, 0x35, 0x15, 0x60, 0, 0x57];
    code.extend_from_slice(&[0x5f, 0x5f, 0x5f, 0x5f, 0x73]);
    code.extend_from_slice(target.as_slice());
    code.extend_from_slice(&[0x5a, 0xf4, 0x50]);
    code.extend_from_slice(parent_exit);
    code[4] = code.len().try_into().unwrap();
    code.push(0x5b);
    code.extend_from_slice(APPROVE_BOTH);
    code
}

pub(crate) fn set_version(evm: &mut Evm<'_, BaseEvmTypes>, version: Version) {
    evm.set_execution_config(
        ExecutionConfig::for_spec_and_version(SpecId::BOGOTA, version),
        SpecId::BOGOTA,
        ethereum_tx_registry(SpecId::BOGOTA),
        Precompiles::base(SpecId::BOGOTA),
    );
}

pub(crate) fn stored(evm: &mut Evm<'_, BaseEvmTypes>, address: Address, key: U256) -> U256 {
    evm.state_mut().storage(&address).unwrap().into_slot(key).unwrap().current()
}

pub(crate) fn install_recent_root(evm: &mut Evm<'_, BaseEvmTypes>) {
    evm.overlay_db_mut().insert_account_info(
        &RECENT_ROOT_ADDRESS,
        AccountInfo::default().with_code(Bytecode::new_legacy(RECENT_ROOT_CODE.clone())),
    );
}

pub(crate) fn recent_tuple(source: [u8; 32], slot: u64, root: [u8; 32]) -> Vec<u8> {
    let mut data = source.to_vec();
    data.extend_from_slice(&slot.to_be_bytes());
    data.extend_from_slice(&root);
    data
}

pub(crate) fn recent_slot(source: [u8; 32], slot: u64) -> U256 {
    let mut key = RECENT_ROOT_STORAGE_DOMAIN.as_slice().to_vec();
    key.extend_from_slice(&source);
    key.extend_from_slice(&(slot % 8192).to_be_bytes());
    U256::from_be_bytes(keccak256(key).0)
}

pub(crate) fn recent_entry(tuple: &[u8]) -> U256 {
    let mut entry = RECENT_ROOT_ENTRY_DOMAIN.as_slice().to_vec();
    entry.extend_from_slice(tuple);
    U256::from_be_bytes(keccak256(entry).0)
}

#[derive(Debug)]
pub(crate) struct CountingRunner(pub(crate) Arc<AtomicUsize>);

impl InterpreterRunner<BaseEvmTypes> for CountingRunner {
    fn run<'frame, 'host>(
        &self,
        _: &mut Interpreter<'frame, 'host, BaseEvmTypes>,
        _: &mut Evm<'host, BaseEvmTypes>,
    ) -> Option<InstrStop> {
        self.0.fetch_add(1, Ordering::Relaxed);
        None
    }
}

pub(crate) struct FailingDatabase(pub(crate) Arc<AtomicUsize>, pub(crate) &'static str);

impl FailingDatabase {
    fn fail<T>(&self) -> Result<T, std::io::Error> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Err(std::io::Error::other(self.1))
    }
}

impl Database for FailingDatabase {
    type Error = std::io::Error;

    fn get_account(&mut self, _: &Address) -> Result<Option<AccountInfo>, Self::Error> {
        self.fail()
    }

    fn get_code_by_hash(&mut self, _: &B256) -> Result<Bytecode, Self::Error> {
        self.fail()
    }

    fn get_storage(&mut self, _: &Address, _: &U256) -> Result<U256, Self::Error> {
        self.fail()
    }

    fn get_block_hash(&mut self, _: &U256) -> Result<B256, Self::Error> {
        self.fail()
    }
}

/// Checks that preparation, prefix validation, and execution report the same rejection.
pub(crate) fn assert_validation_error(
    evm: &mut Evm<'_, BaseEvmTypes>,
    tx: &Recovered<TxEnvelope>,
    expected: HandlerError,
) {
    assert_eq!(evm.validate_tx(tx).unwrap_err(), expected, "preparation");
    assert_eq!(evm.validate_frame_prefix(tx, 1).unwrap_err(), expected, "prefix");
    assert_eq!(evm.transact(tx).unwrap_err(), expected, "execution");
}
