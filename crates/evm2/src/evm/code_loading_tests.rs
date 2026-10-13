//! End-to-end checks of code opcode payload reads and chunk gas reservation.

use super::*;
use crate::{
    BaseEvmConfigSelector, BaseEvmTypes, Precompiles, SpecId, Version,
    bytecode::{
        Bytecode, CODE_CHUNK_SIZE, COLD_CODE_CHUNK_GAS, LEGACY_CODE_CHUNK_SIZE,
        WARM_CODE_CHUNK_GAS, code_metadata,
    },
    env::{BlockEnvExt, TxEnvExt},
    ethereum::{ethereum_tx_registry, prepare_initial_frame},
    interpreter::{MessageExt, MessageResult, instructions::encode_rjump_offset, op},
    test_utils::{push, push_address, push_all},
};
use alloc::{vec, vec::Vec};
use alloy_primitives::{Bytes, TxKind, U256, hex, keccak256};

const OWNER: Address = Address::with_last_byte(0x81);
const CALLER: Address = Address::with_last_byte(0x82);

#[derive(Default)]
struct CodeReads {
    db: InMemoryDB,
    reads: Vec<CodeRead>,
    chunk_hashes: Vec<B256>,
    chunk_error: Option<DatabaseError>,
}

impl DynDatabase for CodeReads {
    fn get_account(&mut self, address: &Address) -> DbResult<Option<AccountInfo>> {
        DynDatabase::get_account(&mut self.db, address)
    }

    fn get_code_by_hash(&mut self, hash: &B256) -> DbResult<Bytecode> {
        if self.chunk_hashes.contains(hash) {
            self.reads.push(CodeRead::Chunk(*hash));
            if let Some(error) = &self.chunk_error {
                return Err(error.clone());
            }
        } else {
            self.reads.push(CodeRead::Legacy(*hash));
        }
        DynDatabase::get_code_by_hash(&mut self.db, hash)
    }

    fn get_storage(&mut self, address: &Address, key: &Word) -> DbResult<Word> {
        DynDatabase::get_storage(&mut self.db, address, key)
    }

    fn get_block_hash(&mut self, number: &Word) -> DbResult<B256> {
        DynDatabase::get_block_hash(&mut self.db, number)
    }
}

fn test_evm(code: &[u8], chunked: bool) -> Evm<'static, BaseEvmTypes> {
    let mut db = CodeReads::default();
    let mut account = AccountInfo::default().with_code(Bytecode::new_legacy(code.to_vec().into()));
    if chunked {
        account.chunks = code_metadata(code).unwrap();
        db.chunk_hashes =
            account.chunks.as_ref().map_or_else(Vec::new, |m| m.chunk_hashes().to_vec());
    }
    db.db.insert_account_info(&OWNER, account);
    Evm::new_with_execution_config(
        ExecutionConfig::for_base_spec::<BaseEvmConfigSelector>(SpecId::OSAKA)
            .with_version(Version::base(SpecId::OSAKA).with_tip1143(true).with_rjump(true)),
        SpecId::OSAKA,
        BlockEnvExt::default(),
        ethereum_tx_registry(SpecId::OSAKA),
        db,
        Precompiles::base(SpecId::OSAKA),
    )
}

fn execute(evm: &mut Evm<'_, BaseEvmTypes>, code: Vec<u8>, gas_limit: u64) -> MessageResult {
    let mut message = MessageExt {
        destination: CALLER,
        code_address: CALLER,
        code: Bytecode::new_legacy(code.into()).into(),
        gas_limit,
        ..Default::default()
    };
    Host::execute_message(evm, &TxEnvExt::default(), &mut message).unwrap()
}

fn reads<'a>(evm: &'a Evm<'static, BaseEvmTypes>) -> &'a [CodeRead] {
    &evm.database().downcast_ref::<CodeReads>().unwrap().reads
}

fn extcopy(code: &mut Vec<u8>, owner: Address, offset: Word, len: usize) {
    push(code, len);
    push(code, offset);
    code.push(op::PUSH0);
    push_address(code, &owner);
    code.push(op::EXTCODECOPY);
}

fn return_memory(code: &mut Vec<u8>, len: usize) {
    push(code, len);
    code.extend([op::PUSH0, op::RETURN]);
}

#[test]
fn extcodecopy_reads_only_intersecting_payloads() {
    let mut bytes = vec![op::STOP; 2 * CODE_CHUNK_SIZE + 7];
    // A PUSH immediate straddles the boundary. Reading its suffix must not fetch its prefix.
    bytes[CODE_CHUNK_SIZE - 2] = op::PUSH32;
    bytes[CODE_CHUNK_SIZE - 1..CODE_CHUNK_SIZE + 31].fill(op::JUMPDEST);
    bytes[2 * CODE_CHUNK_SIZE..2 * CODE_CHUNK_SIZE + 7].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7]);
    for (offset, len, indices) in [
        (CODE_CHUNK_SIZE - 3, 40, vec![0, 1]),
        (CODE_CHUNK_SIZE, 32, vec![1]),
        (2 * CODE_CHUNK_SIZE - 2, 20, vec![1, 2]),
        (0, bytes.len() + 32, vec![0, 1, 2]),
        (bytes.len(), 10, vec![]),
        (usize::MAX, 10, vec![]),
        (0, 0, vec![]),
    ] {
        let mut evm = test_evm(&bytes, true);
        let mut code = Vec::new();
        extcopy(&mut code, OWNER, Word::from(offset), len);
        return_memory(&mut code, len);
        let result = execute(&mut evm, code, 1_000_000);
        assert_eq!(result.stop, InstrStop::Return);
        let mut expected = vec![0; len];
        let data = bytes.get(offset..).unwrap_or_default();
        let copied = data.len().min(len);
        expected[..copied].copy_from_slice(&data[..copied]);
        assert_eq!(result.output.as_ref(), expected);
        assert_eq!(reads(&evm), indices.iter().map(|&i| chunk_read(&bytes, i)).collect::<Vec<_>>());
        assert!(evm.state.account(&OWNER).unwrap().is_warm());
        for i in 0..3 {
            assert_eq!(evm.state.code_chunk_is_warm(&OWNER, i), indices.contains(&i));
        }
    }
}

#[test]
fn extcodecopy_zero_length_ignores_unrepresentable_offsets() {
    let bytes = vec![op::STOP; LEGACY_CODE_CHUNK_SIZE + 1];
    let mut evm = test_evm(&bytes, true);
    let mut code = Vec::new();
    push_all(&mut code, [Word::ZERO, Word::MAX, Word::MAX]);
    push_address(&mut code, &OWNER);
    code.push(op::EXTCODECOPY);
    let result = execute(&mut evm, code, 10_000);
    assert_eq!(result.stop, InstrStop::Stop);
    assert!(reads(&evm).is_empty());
    assert!(evm.state.account(&OWNER).unwrap().is_warm());
}

#[test]
fn extcodecopy_skips_unaffordable_cold_chunk() {
    let bytes = vec![op::STOP; LEGACY_CODE_CHUNK_SIZE + 1];
    let mut evm = test_evm(&bytes, true);
    let mut code = Vec::new();
    extcopy(&mut code, OWNER, Word::from(CODE_CHUNK_SIZE - 1), 2);
    // The first chunk is affordable; skip the second and roll back warmth on halt.
    let result = execute(&mut evm, code, COLD_CODE_CHUNK_GAS + 10_000);
    assert_eq!(result.stop, InstrStop::OutOfGas);
    assert_eq!(reads(&evm), [chunk_read(&bytes, 0)]);
    assert!(!evm.state.code_chunk_is_warm(&OWNER, 0));
    assert!(!evm.state.code_chunk_is_warm(&OWNER, 1));
}

#[test]
fn external_code_metadata_does_not_load_chunk_payloads() {
    let bytes = vec![op::STOP; LEGACY_CODE_CHUNK_SIZE + 1];
    let mut evm = test_evm(&bytes, true);
    let mut code = Vec::new();
    push_address(&mut code, &OWNER);
    code.extend([op::EXTCODESIZE, op::PUSH0, op::MSTORE]);
    push_address(&mut code, &OWNER);
    code.extend([op::EXTCODEHASH, op::PUSH1, 32, op::MSTORE]);
    return_memory(&mut code, 64);
    let result = execute(&mut evm, code, 10_000);
    assert_eq!(result.stop, InstrStop::Return);
    assert_eq!(&result.output[..32], Word::from(bytes.len()).to_be_bytes::<32>());
    assert_eq!(&result.output[32..], keccak256(&bytes).as_slice());
    assert!(reads(&evm).is_empty());
}

#[test]
fn legacy_external_code_uses_fixed_limit_after_size_is_known() {
    for size in [3, LEGACY_CODE_CHUNK_SIZE] {
        let bytes = vec![op::JUMPDEST; size];
        let mut evm = test_evm(&bytes, false);
        let mut code = Vec::new();
        push_address(&mut code, &OWNER);
        code.extend([op::EXTCODESIZE, op::POP]);
        extcopy(&mut code, OWNER, Word::from(LEGACY_CODE_CHUNK_SIZE - 1), 2);
        return_memory(&mut code, 2);
        let result = execute(&mut evm, code, 100_000);
        assert_eq!(result.stop, InstrStop::Return);
        assert_eq!(result.output.as_ref(), [if size == 3 { 0 } else { op::JUMPDEST }, 0]);
        assert_eq!(reads(&evm), [CodeRead::Legacy(keccak256(&bytes))]);
        assert!(evm.state.code_chunk_is_warm(&OWNER, 0));
    }
}

#[test]
fn repeated_extcodecopy_charges_warm_chunks_and_revert_restores_coldness() {
    let bytes = vec![op::STOP; LEGACY_CODE_CHUNK_SIZE + 1];
    let mut evm = test_evm(&bytes, true);
    let mut code = Vec::new();
    extcopy(&mut code, OWNER, Word::from(CODE_CHUNK_SIZE - 1), 2);
    let cold = execute(&mut evm, code.clone(), 100_000);
    let warm = execute(&mut evm, code.clone(), 100_000);
    assert_eq!(cold.stop, InstrStop::Stop);
    assert_eq!(warm.stop, InstrStop::Stop);
    assert_eq!(
        warm.gas.remaining() - cold.gas.remaining(),
        2 * (COLD_CODE_CHUNK_GAS - WARM_CODE_CHUNK_GAS)
            + evm.version().gas_params.cold_account_additional_cost()
    );
    assert_eq!(reads(&evm).len(), 2);

    let mut evm = test_evm(&bytes, true);
    let mut reverting = code.clone();
    reverting.extend([op::PUSH0, op::PUSH0, op::REVERT]);
    assert_eq!(execute(&mut evm, reverting, 100_000).stop, InstrStop::Revert);
    assert!(!evm.state.code_chunk_is_warm(&OWNER, 0));
    assert!(!evm.state.code_chunk_is_warm(&OWNER, 1));
    let after_revert = execute(&mut evm, code, 100_000);
    assert_eq!(after_revert.gas.remaining(), cold.gas.remaining());
    // The bytes stay cached, but they must still be charged as cold after rollback.
    assert_eq!(reads(&evm).len(), 2);
}

#[test]
fn delegation_external_code_reads_inline_marker_only() {
    let bytes = vec![op::STOP; LEGACY_CODE_CHUNK_SIZE + 1];
    let mut evm = test_evm(&bytes, true);
    let delegated = Address::with_last_byte(0x83);
    let marker = Bytecode::new_eip7702(OWNER);
    evm.overlay_db_mut()
        .insert_account_info(&delegated, AccountInfo::default().with_code(marker.clone()));
    let mut code = Vec::new();
    extcopy(&mut code, delegated, Word::ZERO, 32);
    push_address(&mut code, &delegated);
    code.extend([op::EXTCODESIZE, op::PUSH1, 32, op::MSTORE]);
    push_address(&mut code, &delegated);
    code.extend([op::EXTCODEHASH, op::PUSH1, 64, op::MSTORE]);
    return_memory(&mut code, 96);
    let result = execute(&mut evm, code, 100_000);
    assert_eq!(result.stop, InstrStop::Return);
    assert_eq!(&result.output[..23], marker.original_byte_slice());
    assert_eq!(&result.output[23..32], [0; 9]);
    assert_eq!(&result.output[32..64], Word::from(23).to_be_bytes::<32>());
    assert_eq!(&result.output[64..], marker.hash_slow().as_slice());
    assert!(reads(&evm).is_empty());
}

fn call(code: &mut Vec<u8>, opcode: u8, target: Address, output_len: usize) {
    push_all(code, [output_len, 0, 0, 0]);
    if opcode == op::CALL || opcode == op::CALLCODE {
        code.push(op::PUSH0);
    }
    push_address(code, &target);
    code.extend([op::GAS, opcode, op::POP]);
}

#[test]
fn call_family_codecopy_uses_code_owner_and_global_offsets() {
    let target = 2 * CODE_CHUNK_SIZE;
    let mut bytes = vec![op::STOP; target + 64];
    let mut entry = Vec::new();
    push(&mut entry, target);
    entry.push(op::JUMP);
    bytes[..entry.len()].copy_from_slice(&entry);
    bytes[CODE_CHUNK_SIZE..CODE_CHUNK_SIZE + 4].copy_from_slice(&[1, 2, 3, 4]);
    let mut tail = vec![op::JUMPDEST, op::PC, op::PUSH0, op::MSTORE];
    tail.extend([op::CODESIZE, op::PUSH1, 32, op::MSTORE]);
    push_all(&mut tail, [4, CODE_CHUNK_SIZE, 64]);
    tail.push(op::CODECOPY);
    return_memory(&mut tail, 68);
    bytes[target..target + tail.len()].copy_from_slice(&tail);
    for opcode in [op::CALL, op::CALLCODE, op::DELEGATECALL, op::STATICCALL] {
        for delegate in [false, true] {
            let mut evm = test_evm(&bytes, true);
            let destination = if delegate { Address::with_last_byte(0x83) } else { OWNER };
            if delegate {
                evm.overlay_db_mut().insert_account_info(
                    &destination,
                    AccountInfo::default().with_code(Bytecode::new_eip7702(OWNER)),
                );
            }
            let mut code = Vec::new();
            call(&mut code, opcode, destination, 68);
            return_memory(&mut code, 68);
            let result = execute(&mut evm, code, 1_000_000);
            assert_eq!(result.stop, InstrStop::Return);
            assert_eq!(&result.output[..32], Word::from(target + 1).to_be_bytes::<32>());
            assert_eq!(&result.output[32..64], Word::from(bytes.len()).to_be_bytes::<32>());
            assert_eq!(&result.output[64..], [1, 2, 3, 4]);
            assert_eq!(reads(&evm), [0, 2, 1].map(|index| chunk_read(&bytes, index)));
        }
    }
}

#[test]
fn initcode_extcodecopy_reads_external_account_source() {
    let bytes = vec![op::JUMPDEST; LEGACY_CODE_CHUNK_SIZE + 1];
    let mut evm = test_evm(&bytes, true);
    let mut initcode = Vec::new();
    extcopy(&mut initcode, OWNER, Word::from(CODE_CHUNK_SIZE), 16);
    return_memory(&mut initcode, 16);
    initcode.resize(CODE_CHUNK_SIZE + 100, op::INVALID);
    let mut message = MessageExt {
        destination: CALLER,
        code_address: CALLER,
        code: Bytecode::new_legacy(Bytes::from(initcode)).into(),
        gas_limit: 100_000,
        ..Default::default()
    };
    let result = Host::execute_message(&mut evm, &TxEnvExt::default(), &mut message).unwrap();
    assert_eq!(result.stop, InstrStop::Return);
    assert_eq!(result.output.as_ref(), [op::JUMPDEST; 16]);
    assert_eq!(reads(&evm), [chunk_read(&bytes, 1)]);
}

fn execute_account(
    evm: &mut Evm<'_, BaseEvmTypes>,
    gas_limit: u64,
) -> Result<MessageResult, ExecutionError> {
    let entry = Host::load_code_chunk(evm, &OWNER, 0, false).unwrap().unwrap().chunk;
    let mut message = MessageExt {
        destination: OWNER,
        code_address: OWNER,
        code: CodeSource::Account(entry),
        gas_limit,
        ..Default::default()
    };
    Host::execute_message(evm, &TxEnvExt::default(), &mut message)
}

#[test]
fn account_jumps_load_only_taken_target_chunks() {
    for opcode in [op::JUMP, op::JUMPI, op::RJUMP, op::RJUMPI] {
        for taken in [false, true] {
            if !taken && (opcode == op::JUMP || opcode == op::RJUMP) {
                continue;
            }
            // Start close enough to the boundary for a relative jump to reach chunk 1.
            let start = CODE_CHUNK_SIZE - 100;
            let target = CODE_CHUNK_SIZE + 1;
            let mut bytes = vec![op::STOP; (target + 1).max(LEGACY_CODE_CHUNK_SIZE + 1)];
            let mut entry = Vec::new();
            push(&mut entry, start);
            entry.push(op::JUMP);
            bytes[..entry.len()].copy_from_slice(&entry);
            let mut branch = vec![op::JUMPDEST];
            if opcode == op::JUMPI || opcode == op::RJUMPI {
                push(&mut branch, u64::from(taken));
            }
            if opcode == op::JUMP || opcode == op::JUMPI {
                push(&mut branch, target);
                branch.push(opcode);
            } else {
                let displacement = (target - (start + branch.len() + 3)) as isize;
                branch.push(opcode);
                branch.extend(encode_rjump_offset(displacement).unwrap());
            }
            bytes[start..start + branch.len()].copy_from_slice(&branch);
            bytes[target] = op::JUMPDEST;
            let mut evm = test_evm(&bytes, true);
            let result = execute_account(&mut evm, 100_000).unwrap();
            assert_eq!(result.stop, InstrStop::Stop);
            let indices = if taken { vec![0, 1] } else { vec![0] };
            assert_eq!(
                reads(&evm),
                indices.into_iter().map(|i| chunk_read(&bytes, i)).collect::<Vec<_>>()
            );
        }
    }
}

#[test]
fn account_fallthrough_skips_continued_push_data() {
    let start = CODE_CHUNK_SIZE - 2;
    let mut bytes = vec![op::STOP; CODE_CHUNK_SIZE + 64];
    let mut entry = Vec::new();
    push(&mut entry, start);
    entry.push(op::JUMP);
    bytes[..entry.len()].copy_from_slice(&entry);
    bytes[start] = op::JUMPDEST;
    bytes[start + 1] = op::PUSH32;
    bytes[CODE_CHUNK_SIZE..CODE_CHUNK_SIZE + 32].fill(op::INVALID);
    let mut tail = vec![op::PUSH0, op::MSTORE, op::PC, op::PUSH1, 32, op::MSTORE];
    return_memory(&mut tail, 64);
    bytes[CODE_CHUNK_SIZE + 32..CODE_CHUNK_SIZE + 32 + tail.len()].copy_from_slice(&tail);
    let mut evm = test_evm(&bytes, true);
    let result = execute_account(&mut evm, 100_000).unwrap();
    assert_eq!(result.stop, InstrStop::Return);
    assert_eq!(&result.output[..32], [op::INVALID; 32]);
    assert_eq!(&result.output[32..], Word::from(CODE_CHUNK_SIZE + 34).to_be_bytes::<32>());
    assert_eq!(reads(&evm), [0, 1].map(|i| chunk_read(&bytes, i)));
}

#[test]
fn account_codecopy_skips_unaffordable_cold_chunk() {
    let mut bytes = vec![op::STOP; CODE_CHUNK_SIZE * 2 + 1];
    let mut entry = Vec::new();
    push_all(&mut entry, [2, CODE_CHUNK_SIZE * 2 - 1, 0]);
    entry.push(op::CODECOPY);
    bytes[..entry.len()].copy_from_slice(&entry);
    let mut evm = test_evm(&bytes, true);
    let result = execute_account(&mut evm, COLD_CODE_CHUNK_GAS + 10_000).unwrap();
    assert_eq!(result.stop, InstrStop::OutOfGas);
    assert_eq!(reads(&evm), [chunk_read(&bytes, 0), chunk_read(&bytes, 1)]);
}

#[test]
fn extcodecopy_missing_or_malformed_chunk_is_fatal() {
    let bytes = vec![op::STOP; LEGACY_CODE_CHUNK_SIZE + 1];
    for malformed in [false, true] {
        let mut evm = test_evm(&bytes, true);
        let chunk_hash = code_metadata(&bytes).unwrap().unwrap().chunk_hashes()[1];
        let db = evm.database_mut().downcast_mut::<CodeReads>().unwrap();
        if malformed {
            db.db
                .cache
                .contracts
                .insert(chunk_hash, Bytecode::new_legacy(vec![op::STOP; 7].into()));
        } else {
            db.db.cache.contracts.remove(&chunk_hash);
        }
        let mut code = Vec::new();
        extcopy(&mut code, OWNER, Word::from(CODE_CHUNK_SIZE), 7);
        let mut message = MessageExt {
            code: Bytecode::new_legacy(code.into()).into(),
            gas_limit: 100_000,
            ..Default::default()
        };
        let error =
            Host::execute_message(&mut evm, &TxEnvExt::default(), &mut message).unwrap_err();
        let ExecutionError::Database(error) = error else { panic!("expected database failure") };
        assert!(error.is_fatal());
        assert_eq!(
            error.downcast_ref::<crate::CodeChunkLoadError>(),
            Some(&if malformed {
                crate::CodeChunkLoadError::Invalid(chunk_hash)
            } else {
                crate::CodeChunkLoadError::Missing(chunk_hash)
            })
        );
        assert_eq!(reads(&evm), [chunk_read(&bytes, 1)]);
        assert!(evm.state.account(&OWNER).unwrap().code_chunks().is_empty());
    }
}

#[derive(Debug, thiserror::Error)]
#[error("chunk provider failure")]
struct ChunkProviderFailure;

#[test]
fn extcodecopy_provider_failure_preserves_original_error_and_is_fatal() {
    let bytes = vec![op::STOP; LEGACY_CODE_CHUNK_SIZE + 1];
    for fatal in [false, true] {
        let mut evm = test_evm(&bytes, true);
        let db = evm.database_mut().downcast_mut::<CodeReads>().unwrap();
        db.chunk_error = Some(DatabaseError::new(ChunkProviderFailure, fatal));
        let mut code = Vec::new();
        extcopy(&mut code, OWNER, Word::from(CODE_CHUNK_SIZE), 7);
        let mut message = MessageExt {
            code: Bytecode::new_legacy(code.into()).into(),
            gas_limit: 100_000,
            ..Default::default()
        };
        let error =
            Host::execute_message(&mut evm, &TxEnvExt::default(), &mut message).unwrap_err();
        let ExecutionError::Database(error) = error else { panic!("expected database failure") };
        assert!(error.is_fatal());
        assert!(error.downcast_ref::<ChunkProviderFailure>().is_some());
        assert_eq!(reads(&evm), [chunk_read(&bytes, 1)]);
    }
}

#[test]
fn invalid_inline_delegation_is_fatal() {
    for target in [Address::ZERO, OWNER] {
        let mut account = AccountInfo::default().with_code(Bytecode::new_eip7702(target));
        if target == OWNER {
            account.code_hash = B256::ZERO;
        }
        let error = account.inline_delegation_code().unwrap_err();
        assert!(error.is_fatal());
        assert_eq!(
            error.downcast_ref::<crate::CodeChunkLoadError>(),
            Some(&crate::CodeChunkLoadError::InvalidDelegation(account.code_hash))
        );
    }
}

/// The backing store sees content hashes, never account addresses or logical indices.
#[derive(Debug, PartialEq, Eq)]
enum CodeRead {
    Legacy(B256),
    Chunk(B256),
}

fn chunk_read(code: &[u8], index: u32) -> CodeRead {
    CodeRead::Chunk(code_metadata(code).unwrap().unwrap().chunk_hashes()[index as usize])
}

#[test]
fn identical_chunk_hashes_share_bytes_but_not_warmth() {
    let mut bytes = vec![op::STOP; 2 * CODE_CHUNK_SIZE + 1];
    assert_eq!(chunk_read(&bytes, 0), chunk_read(&bytes, 1));
    let mut evm = test_evm(&bytes, true);
    let mut code = Vec::new();
    extcopy(&mut code, OWNER, Word::ZERO, 1);
    extcopy(&mut code, OWNER, Word::from(CODE_CHUNK_SIZE), 1);
    let shared = execute(&mut evm, code.clone(), 100_000);
    assert_eq!(shared.stop, InstrStop::Stop);
    assert_eq!(reads(&evm), [chunk_read(&bytes, 0)]);
    assert!(evm.state.code_chunk_is_warm(&OWNER, 0));
    assert!(evm.state.code_chunk_is_warm(&OWNER, 1));

    let other = Address::with_last_byte(0x84);
    let account = evm
        .database()
        .downcast_ref::<CodeReads>()
        .unwrap()
        .db
        .account_info(&OWNER)
        .unwrap()
        .clone();
    evm.overlay_db_mut().insert_account_info(&other, account);
    assert!(!evm.state.code_chunk_is_warm(&other, 0));
    let mut other_copy = Vec::new();
    extcopy(&mut other_copy, other, Word::ZERO, 1);
    let other_result = execute(&mut evm, other_copy, 100_000);
    assert_eq!(other_result.stop, InstrStop::Stop);
    assert_eq!(reads(&evm), [chunk_read(&bytes, 0)]);
    assert!(evm.state.code_chunk_is_warm(&other, 0));
    assert!(!evm.state.code_chunk_is_warm(&other, 1));
    let first = evm.state.load_code_chunk(&OWNER, 0, false).unwrap().unwrap().get().bytecode();
    for (address, index) in [(OWNER, 1), (other, 0)] {
        let loaded =
            evm.state.load_code_chunk(&address, index, false).unwrap().unwrap().get().bytecode();
        assert!(core::ptr::eq(first.bytes_slice().as_ptr(), loaded.bytes_slice().as_ptr()));
        assert!(core::ptr::eq(
            first.legacy_jump_table().unwrap(),
            loaded.legacy_jump_table().unwrap(),
        ));
    }
    let mut cold_copy = Vec::new();
    extcopy(&mut cold_copy, OWNER, Word::ZERO, 1);
    let cold_result = execute(&mut test_evm(&bytes, true), cold_copy, 100_000);
    assert_eq!(other_result.gas.remaining(), cold_result.gas.remaining());

    bytes[CODE_CHUNK_SIZE] = op::JUMPDEST;
    let mut separate = test_evm(&bytes, true);
    let distinct = execute(&mut separate, code, 100_000);
    assert_eq!(reads(&separate), [chunk_read(&bytes, 0), chunk_read(&bytes, 1)]);
    assert_eq!(shared.gas.remaining(), distinct.gas.remaining());
}

#[test]
fn chunk_skip_cold_preserves_resident_coldness_and_journal() {
    let bytes = vec![op::STOP; LEGACY_CODE_CHUNK_SIZE + 1];
    for resident in [false, true] {
        let mut evm = test_evm(&bytes, true);
        if resident {
            evm.state.load_code_chunk(&OWNER, 0, false).unwrap().unwrap();
        }
        let checkpoint = evm.state.checkpoint();
        let journal_len = evm.state.journal().len();
        assert!(matches!(
            evm.state.load_code_chunk(&OWNER, 0, true),
            Err(LoadError::ColdLoadSkipped)
        ));
        assert!(matches!(
            Host::load_code_chunk(&mut evm, &OWNER, 0, true),
            Err(HostError::Halt(InstrStop::OutOfGas))
        ));
        assert_eq!(reads(&evm).len(), usize::from(resident));
        assert_eq!(evm.state.journal().len(), journal_len);
        assert!(!evm.state.code_chunk_is_warm(&OWNER, 0));
        assert_eq!(evm.state.account(&OWNER).unwrap().code_chunks().len(), usize::from(resident));

        let cold = Host::load_code_chunk(&mut evm, &OWNER, 0, false).unwrap().unwrap();
        assert!(cold.is_cold);
        let warm = Host::load_code_chunk(&mut evm, &OWNER, 0, true).unwrap().unwrap();
        assert!(!warm.is_cold);
        assert_eq!(reads(&evm), [chunk_read(&bytes, 0)]);
        assert_eq!(evm.state.journal().len(), journal_len + 1);
        evm.state.rollback(checkpoint, EvmFeatures::TIP1143);
        assert!(matches!(
            Host::load_code_chunk(&mut evm, &OWNER, 0, true),
            Err(HostError::Halt(InstrStop::OutOfGas))
        ));
        assert_eq!(evm.state.journal().len(), journal_len);
        assert_eq!(reads(&evm), [chunk_read(&bytes, 0)]);
    }
}

#[test]
fn chunk_skip_cold_honors_warm_and_cold_gas_boundaries() {
    let bytes = vec![op::STOP; LEGACY_CODE_CHUNK_SIZE + 1];
    let mut code = Vec::new();
    extcopy(&mut code, OWNER, Word::ZERO, 1);
    for warm in [false, true] {
        let mut reference = test_evm(&bytes, true);
        Host::load_account(&mut reference, &OWNER, false, false).unwrap();
        if warm {
            Host::load_code_chunk(&mut reference, &OWNER, 0, false).unwrap();
        }
        let result = execute(&mut reference, code.clone(), 100_000);
        assert_eq!(result.stop, InstrStop::Stop);
        let required = 100_000 - result.gas.remaining();
        for enough in [false, true] {
            let mut evm = test_evm(&bytes, true);
            Host::load_account(&mut evm, &OWNER, false, false).unwrap();
            if warm {
                Host::load_code_chunk(&mut evm, &OWNER, 0, false).unwrap();
            }
            let result = execute(&mut evm, code.clone(), required - u64::from(!enough));
            assert_eq!(result.stop, if enough { InstrStop::Stop } else { InstrStop::OutOfGas });
            assert_eq!(reads(&evm).len(), usize::from(warm || enough));
            assert_eq!(evm.state.code_chunk_is_warm(&OWNER, 0), warm || enough);
        }
    }
}

#[test]
fn initial_chunk_skips_cold_read_and_accepts_warm_budget() {
    let bytes = vec![op::STOP; LEGACY_CODE_CHUNK_SIZE + 1];
    for warm in [false, true] {
        for enough in [false, true] {
            let mut evm = test_evm(&bytes, true);
            if warm {
                Host::load_code_chunk(&mut evm, &OWNER, 0, false).unwrap();
            }
            let charge = if warm { WARM_CODE_CHUNK_GAS } else { COLD_CODE_CHUNK_GAS };
            let mut gas = GasTracker::new(charge - u64::from(!enough));
            let frame = prepare_initial_frame(
                &mut evm,
                CALLER,
                0,
                TxKind::Call(OWNER),
                &Bytes::new(),
                U256::ZERO,
                &mut gas,
            )
            .unwrap();
            assert_eq!(frame.is_some(), enough);
            assert_eq!(reads(&evm).len(), usize::from(warm || enough));
            assert_eq!(evm.state.code_chunk_is_warm(&OWNER, 0), warm || enough);
            if enough {
                assert_eq!(gas.remaining(), 0);
            }
        }
    }
}

#[test]
fn account_code_size_distinguishes_empty_unknown_and_chunked_without_loading() {
    let mut evm = test_evm(&[op::STOP], false);
    let funded = Address::with_last_byte(0x85);
    evm.overlay_db_mut()
        .insert_account_info(&funded, AccountInfo { balance: Word::from(1), ..Default::default() });
    for address in [OWNER, funded, Address::with_last_byte(0x86)] {
        let metadata = Host::load_account(&mut evm, &address, false, false).unwrap();
        assert_eq!(metadata.code_size, if address == OWNER { None } else { Some(0) });
        assert_eq!(metadata.is_empty, address != OWNER && address != funded);
        assert!(metadata.code.is_empty());
        assert!(reads(&evm).is_empty());
    }
    let loaded = Host::load_account(&mut evm, &OWNER, true, false).unwrap();
    assert_eq!(loaded.code_size, Some(1));
    assert_eq!(loaded.code.original_byte_slice(), &[op::STOP]);

    let bytes = vec![op::STOP; 2 * CODE_CHUNK_SIZE + 7];
    let mut evm = test_evm(&bytes, true);
    let metadata = Host::load_account(&mut evm, &OWNER, false, false).unwrap();
    assert!(metadata.is_chunked);
    assert_eq!(metadata.code_size, Some(bytes.len() as u32));
    assert!(metadata.code.is_empty());
    assert!(reads(&evm).is_empty());
}

#[test]
fn precompile_calls_skip_chunk_loading_and_tariffs() {
    let sha256 = Address::with_last_byte(2);
    let expected = hex!("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    for opcode in [op::CALL, op::CALLCODE, op::DELEGATECALL, op::STATICCALL] {
        let mut evm = test_evm(&[op::STOP], false);
        let mut code = Vec::new();
        call(&mut code, opcode, sha256, 32);
        return_memory(&mut code, 32);
        let result = execute(&mut evm, code, 10_000);
        assert_eq!(result.stop, InstrStop::Return);
        assert_eq!(result.output.as_ref(), expected);
        assert!(reads(&evm).is_empty());
        assert!(!evm.state.code_chunk_is_warm(&sha256, 0));
    }

    let mut evm = test_evm(&[op::STOP], false);
    let mut gas = GasTracker::new(10_000);
    let mut frame = prepare_initial_frame(
        &mut evm,
        CALLER,
        0,
        TxKind::Call(sha256),
        &Bytes::new(),
        U256::ZERO,
        &mut gas,
    )
    .unwrap()
    .unwrap();
    assert_eq!(gas.remaining(), 10_000);
    let result = Host::execute_message(&mut evm, &TxEnvExt::default(), &mut frame.message).unwrap();
    assert!(result.is_success());
    assert_eq!(result.output.as_ref(), expected);
    assert_eq!(result.gas.remaining(), 9_940);
    assert!(reads(&evm).is_empty());
}
