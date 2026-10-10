//! TIP-1143 public metadata and synchronous adapter acceptance.
//!
//! Proposed exports: `CodeChunk::new(Bytes)` / `bytes()` and
//! `CodeMetadata::new(u32, Vec<B256>)` / `code_size()` / `chunk_hashes()`.
//! Metadata construction validates size/count, not provider authentication.

use alloy_primitives::{Address, B256, Bytes, keccak256};
use evm2::{
    bytecode::{Bytecode, CodeChunk, CodeMetadata},
    evm::{AccountInfo, CacheDB, Database, Db, DbStats, DynDatabase, EmptyDB, InMemoryDB},
    interpreter::Word,
};
use std::{cell::RefCell, collections::BTreeMap, io, rc::Rc};

#[derive(Clone, Debug, Default)]
struct Provider {
    rows: BTreeMap<(B256, u32), Bytes>,
    reads: Rc<RefCell<Vec<(B256, u32)>>>,
}

impl Database for Provider {
    type Error = io::Error;

    fn get_account(&mut self, _: &Address) -> Result<Option<AccountInfo>, Self::Error> {
        Ok(None)
    }

    fn get_code_by_hash(&mut self, _: &B256) -> Result<Bytecode, Self::Error> {
        panic!("chunk adapter must not reconstruct complete code")
    }

    fn get_storage(&mut self, _: &Address, _: &Word) -> Result<Word, Self::Error> {
        Ok(Word::ZERO)
    }

    fn get_block_hash(&mut self, _: &Word) -> Result<B256, Self::Error> {
        Ok(B256::ZERO)
    }

    fn get_code_chunk_by_hash(
        &mut self,
        code_hash: &B256,
        index: u32,
    ) -> Result<Option<CodeChunk>, Self::Error> {
        self.reads.borrow_mut().push((*code_hash, index));
        if index == u32::MAX {
            return Err(io::Error::other("TIP1143 provider sentinel"));
        }
        Ok(self.rows.get(&(*code_hash, index)).cloned().map(CodeChunk::new))
    }
}

fn provider() -> (Provider, B256) {
    let hash = keccak256(b"complete code identity");
    let mut provider = Provider::default();
    provider.rows.insert((hash, 0), Bytes::from_static(&[0x60, 0x2a, 0]));
    (provider, hash)
}

#[test]
fn tip1143_t08_metadata_boundaries_and_order() {
    for size in [24577_u32, 958465, 981600] {
        let count = (size as usize).div_ceil(24540);
        let hashes = (0..count).map(|i| keccak256([i as u8])).collect::<Vec<_>>();
        let metadata = CodeMetadata::new(size, hashes.clone()).unwrap();
        assert_eq!(metadata.code_size(), size);
        assert_eq!(metadata.chunk_hashes(), hashes.as_slice());
        assert_eq!(metadata.clone(), metadata);
        for invalid_count in [0, 1, count - 1, count + 1, 41] {
            assert!(
                CodeMetadata::new(size, vec![B256::ZERO; invalid_count]).is_err(),
                "size={size}, count={invalid_count}"
            );
        }
    }
    for size in [0, 24539, 24540, 981601, u32::MAX] {
        assert!(CodeMetadata::new(size, vec![B256::ZERO; 2]).is_err(), "size={size}");
    }
}

#[test]
fn tip1143_t10_typed_and_mutable_reference_forwarding() {
    let (mut provider, hash) = provider();
    let chunk = Database::get_code_chunk_by_hash(&mut provider, &hash, 0).unwrap().unwrap();
    assert_eq!(chunk.bytes(), &[0x60, 0x2a, 0]);
    let mut borrowed = &mut provider;
    assert!(Database::get_code_chunk_by_hash(&mut borrowed, &hash, 1).unwrap().is_none());
    let error = Database::get_code_chunk_by_hash(&mut borrowed, &hash, u32::MAX).unwrap_err();
    assert_eq!(error.to_string(), "TIP1143 provider sentinel");
    assert_eq!(*provider.reads.borrow(), [(hash, 0), (hash, 1), (hash, u32::MAX)]);
}

#[test]
fn tip1143_t10_erased_counted_and_empty_forwarding() {
    let (provider, hash) = provider();
    let ledger = provider.reads.clone();
    let mut counted = DbStats::new(Db::new(provider));
    let erased: &mut dyn DynDatabase = &mut counted;
    assert_eq!(erased.get_code_chunk_by_hash(&hash, 0).unwrap().unwrap().bytes(), &[0x60, 0x2a, 0]);
    assert!(erased.get_code_chunk_by_hash(&hash, 1).unwrap().is_none());
    assert_eq!(
        erased.get_code_chunk_by_hash(&hash, u32::MAX).unwrap_err().to_string(),
        "TIP1143 provider sentinel"
    );
    assert_eq!(counted.counts().get_code_chunk_by_hash, 3);
    assert_eq!(*ledger.borrow(), [(hash, 0), (hash, 1), (hash, u32::MAX)]);
    let mut empty = EmptyDB::default();
    for index in [0, 1, u32::MAX] {
        assert!(Database::get_code_chunk_by_hash(&mut empty, &hash, index).unwrap().is_none());
        assert!(DynDatabase::get_code_chunk_by_hash(&mut empty, &hash, index).unwrap().is_none());
    }
}

#[test]
fn tip1143_t10_t38_cache_uses_hash_and_index() {
    let (mut provider, first) = provider();
    let second = keccak256(b"different complete code");
    provider.rows.insert((first, 1), Bytes::from_static(&[0x5b, 0]));
    provider.rows.insert((second, 0), Bytes::from_static(&[0x60, 0x7f, 0]));
    provider.rows.insert((second, 1), Bytes::from_static(&[0x5b, 0]));
    let ledger = provider.reads.clone();
    let mut cache = CacheDB::new(DbStats::new(Db::new(provider)));
    let rows = [
        (first, 0, vec![0x60, 0x2a, 0]),
        (first, 1, vec![0x5b, 0]),
        (second, 0, vec![0x60, 0x7f, 0]),
        (second, 1, vec![0x5b, 0]),
    ];
    for _ in 0..3 {
        for (hash, index, expected) in &rows {
            let chunk =
                DynDatabase::get_code_chunk_by_hash(&mut cache, hash, *index).unwrap().unwrap();
            assert_eq!(chunk.bytes(), expected.as_slice());
        }
    }
    assert_eq!(*ledger.borrow(), [(first, 0), (first, 1), (second, 0), (second, 1)]);
}

#[cfg(feature = "serde")]
#[test]
fn tip1143_t08_metadata_serde_revalidates_ingress() {
    let valid = CodeMetadata::new(24577, vec![B256::ZERO, B256::repeat_byte(1)]).unwrap();
    let encoded = serde_json::to_value(&valid).unwrap();
    assert_eq!(serde_json::from_value::<CodeMetadata>(encoded.clone()).unwrap(), valid);
    for size in [0, 24540, 981601, u32::MAX] {
        let mut malformed = encoded.clone();
        malformed["code_size"] = serde_json::json!(size);
        assert!(serde_json::from_value::<CodeMetadata>(malformed).is_err());
    }
    let mut malformed = encoded;
    malformed["chunk_hashes"] = serde_json::json!([B256::ZERO]);
    assert!(serde_json::from_value::<CodeMetadata>(malformed).is_err());
}

#[cfg(feature = "async")]
mod asynchronous {
    use super::*;
    use evm2::{
        BaseEvmTypes, Evm, ExecutionConfig, Precompiles, SpecId, Version,
        env::BlockEnvExt,
        evm::{
            SystemTx,
            r#async::{AsyncDatabase, AsyncDb, AsyncError},
        },
        interpreter::InstrStop,
        registry::{HandlerError, TxRegistry},
    };
    use std::{
        future::{Future, poll_fn},
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        task::{Context, Poll, Wake, Waker},
    };

    const CHUNK: usize = 24540;

    #[derive(Clone, Copy, Debug)]
    enum Response {
        Valid,
        Missing,
        Short,
        Empty,
        Long,
        Oversized,
        Error,
        TrustedMismatch,
    }

    #[derive(Clone, Debug)]
    struct SuspendingProvider {
        code: Bytes,
        fail_index: u32,
        response: Response,
        reads: Arc<Mutex<Vec<u32>>>,
        polls: Arc<AtomicUsize>,
    }

    impl SuspendingProvider {
        fn account(&self, address: Address) -> Option<AccountInfo> {
            (address == Address::repeat_byte(0x44)).then(|| AccountInfo {
                nonce: 1,
                code_hash: keccak256(&self.code),
                extension: ((self.code.len() > CHUNK).then(|| {
                    CodeMetadata::new(
                        self.code.len() as u32,
                        evm2::bytecode::code_metadata(&self.code)
                            .unwrap()
                            .unwrap()
                            .chunk_hashes()
                            .to_vec(),
                    )
                    .unwrap()
                }))
                .map(evm2::evm::AccountExtension::chunked)
                .unwrap_or_default(),
                ..Default::default()
            })
        }

        fn read(&self, hash: B256, index: u32) -> Result<Option<CodeChunk>, io::Error> {
            assert_eq!(hash, keccak256(&self.code));
            self.reads.lock().unwrap().push(index);
            let Some(payload) = self.code.chunks(CHUNK).nth(index as usize) else {
                return Ok(None);
            };
            let mut bytes = payload.to_vec();
            if index == self.fail_index {
                match self.response {
                    Response::Valid => {}
                    Response::TrustedMismatch => bytes[2] = 99,
                    Response::Missing => return Ok(None),
                    Response::Short => {
                        bytes.pop();
                    }
                    Response::Empty => bytes.clear(),
                    Response::Long => bytes.push(0),
                    Response::Oversized => bytes.resize(24577, 0),
                    Response::Error => return Err(io::Error::other("TIP1143 async sentinel")),
                }
            }
            let prepared = evm2::bytecode::code_chunk(&self.code, index).unwrap();
            if bytes.len() == payload.len()
                && let Some(layout) = prepared.prepared()
            {
                return Ok(Some(
                    CodeChunk::from_prepared(
                        {
                            let mut stored = prepared.bytes().to_vec();
                            stored[..bytes.len()].copy_from_slice(&bytes);
                            stored.into()
                        },
                        layout.code_size(),
                        layout.index(),
                    )
                    .unwrap(),
                ));
            }
            Ok(Some(CodeChunk::new(bytes.into())))
        }
    }

    impl AsyncDatabase for SuspendingProvider {
        type Error = io::Error;

        async fn get_account(
            &mut self,
            address: Address,
        ) -> Result<Option<AccountInfo>, Self::Error> {
            Ok(self.account(address))
        }

        async fn get_code_by_hash(&mut self, _: B256) -> Result<Bytecode, Self::Error> {
            assert!(self.code.len() <= 24576);
            let chunk = self
                .read(keccak256(&self.code), 0)?
                .ok_or_else(|| io::Error::other("missing legacy payload"))?;
            Ok(chunk.bytecode())
        }

        async fn get_storage(&mut self, _: Address, _: Word) -> Result<Word, Self::Error> {
            Ok(Word::ZERO)
        }

        async fn get_block_hash(&mut self, _: Word) -> Result<B256, Self::Error> {
            Ok(B256::ZERO)
        }

        async fn get_code_chunk_by_hash(
            &mut self,
            hash: B256,
            index: u32,
        ) -> Result<Option<CodeChunk>, Self::Error> {
            let mut suspended = false;
            poll_fn(|cx| {
                self.polls.fetch_add(1, Ordering::SeqCst);
                if suspended {
                    Poll::Ready(())
                } else {
                    suspended = true;
                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
            })
            .await;
            self.read(hash, index)
        }
    }

    impl Database for SuspendingProvider {
        type Error = io::Error;

        fn get_account(&mut self, address: &Address) -> Result<Option<AccountInfo>, Self::Error> {
            Ok(self.account(*address))
        }

        fn get_code_by_hash(&mut self, _: &B256) -> Result<Bytecode, Self::Error> {
            assert!(self.code.len() <= 24576);
            let chunk = self
                .read(keccak256(&self.code), 0)?
                .ok_or_else(|| io::Error::other("missing legacy payload"))?;
            Ok(chunk.bytecode())
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
            self.read(*hash, index)
        }
    }

    #[derive(Default)]
    struct WakeCounter(AtomicUsize);

    impl Wake for WakeCounter {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn drive<F: Future>(future: F) -> (F::Output, usize) {
        let counter = Arc::new(WakeCounter::default());
        let waker = Waker::from(counter.clone());
        let mut context = Context::from_waker(&waker);
        let mut future = std::pin::pin!(future);
        for pending in 0..100 {
            match future.as_mut().poll(&mut context) {
                Poll::Ready(result) => {
                    assert_eq!(counter.0.load(Ordering::SeqCst), pending);
                    return (result, pending);
                }
                Poll::Pending => assert_eq!(counter.0.load(Ordering::SeqCst), pending + 1),
            }
        }
        panic!("async execution failed to resume after 100 wakeups")
    }

    fn evm<'a>(db: impl DynDatabase + 'a) -> Evm<'a, BaseEvmTypes> {
        Evm::new_with_execution_config(
            ExecutionConfig::for_spec_and_version(
                SpecId::PRAGUE,
                Version::new(SpecId::PRAGUE).with_tip1143(true),
            ),
            SpecId::PRAGUE,
            BlockEnvExt::default(),
            TxRegistry::new(),
            db,
            Precompiles::base(SpecId::PRAGUE),
        )
    }

    fn fixture(copy: bool, response: Response, fail_index: u32) -> SuspendingProvider {
        let mut code = if copy {
            // Copy two bytes spanning chunks one/two, then return both bytes.
            vec![
                0x60,
                2,
                0x61,
                ((2 * CHUNK - 1) >> 8) as u8,
                (2 * CHUNK - 1) as u8,
                0x60,
                0,
                0x39,
                0x60,
                2,
                0x60,
                0,
                0xf3,
            ]
        } else {
            vec![0x61, ((2 * CHUNK) >> 8) as u8, (2 * CHUNK) as u8, 0x56]
        };
        code.resize(2 * CHUNK, 0);
        code.extend([0x5b, 0x60, 42, 0x60, 0, 0x52, 0x60, 32, 0x60, 0, 0xf3]);
        SuspendingProvider {
            code: code.into(),
            fail_index,
            response,
            reads: Arc::default(),
            polls: Arc::default(),
        }
    }

    #[test]
    fn tip1143_t11_pending_entry_jump_and_later_copy_reads_resume_once() {
        for copy in [false, true] {
            let provider = fixture(copy, Response::Valid, 0);
            let reads = provider.reads.clone();
            let polls = provider.polls.clone();
            let sync_provider = SuspendingProvider {
                reads: Arc::default(),
                polls: Arc::default(),
                ..provider.clone()
            };
            let mut synchronous = evm(Db::new(sync_provider));
            let tx = SystemTx::new(Address::repeat_byte(0x44), Bytes::new());
            let expected = synchronous.system_call(tx.clone()).unwrap().discard();
            let mut asynchronous = evm(AsyncDb::new(provider));
            let (result, pending) = drive(asynchronous.system_call_async(tx));
            let actual = result.unwrap().discard();
            assert_eq!(actual, expected);
            assert_eq!(actual.stop, InstrStop::Return);
            let indices = if copy { vec![0, 1, 2] } else { vec![0, 2] };
            assert_eq!(*reads.lock().unwrap(), indices);
            assert_eq!(pending, indices.len());
            assert_eq!(polls.load(Ordering::SeqCst), 2 * indices.len());
            if copy {
                assert_eq!(actual.output.as_ref(), &[0, 0x5b]);
                // Ordinary cost 24 plus one entry and two cold copy requests.
                assert_eq!(actual.total_gas_spent, 3 * 28680 + 24);
            } else {
                assert_eq!(actual.output.len(), 32);
                assert_eq!(actual.output[31], 42);
                assert_eq!(actual.total_gas_spent, 2 * 28680 + 30);
            }
            assert!(actual.logs.is_empty());
        }
    }

    #[test]
    fn tip1143_t11_t29_async_required_data_failures_are_node_errors() {
        for copy in [false, true] {
            for index in [0, 2] {
                for response in [
                    Response::Missing,
                    Response::Short,
                    Response::Empty,
                    Response::Long,
                    Response::Error,
                ] {
                    let provider = fixture(copy, response, index);
                    let reads = provider.reads.clone();
                    let polls = provider.polls.clone();
                    let mut execution = evm(AsyncDb::new(provider));
                    let (result, pending) = drive(execution.system_call_async(SystemTx::new(
                        Address::repeat_byte(0x44),
                        Bytes::new(),
                    )));
                    assert!(
                        matches!(result, Err(AsyncError::Inner(HandlerError::Database(_)))),
                        "copy={copy}, index={index}, response={response:?}"
                    );
                    let expected = if index == 0 {
                        vec![0]
                    } else if copy {
                        vec![0, 1, 2]
                    } else {
                        vec![0, 2]
                    };
                    assert_eq!(*reads.lock().unwrap(), expected);
                    assert_eq!(pending, expected.len());
                    assert_eq!(polls.load(Ordering::SeqCst), 2 * expected.len());
                }
            }
        }
    }

    #[test]
    fn tip1143_t29_sync_cached_counted_and_erased_failures_are_node_errors() {
        for route in 0..3 {
            for copy in [false, true] {
                for index in [0, 2] {
                    for response in [
                        Response::Missing,
                        Response::Short,
                        Response::Empty,
                        Response::Long,
                        Response::Error,
                    ] {
                        let provider = fixture(copy, response, index);
                        let reads = provider.reads.clone();
                        let mut execution = match route {
                            0 => evm(Db::new(provider)),
                            1 => evm(CacheDB::new(Db::new(provider))),
                            2 => evm(DbStats::new(Db::new(provider))),
                            _ => unreachable!(),
                        };
                        let result = execution
                            .system_call(SystemTx::new(Address::repeat_byte(0x44), Bytes::new()));
                        assert!(
                            matches!(result, Err(HandlerError::Database(_))),
                            "route={route}, copy={copy}, index={index}, response={response:?}"
                        );
                        let expected = if index == 0 {
                            vec![0]
                        } else if copy {
                            vec![0, 1, 2]
                        } else {
                            vec![0, 2]
                        };
                        assert_eq!(*reads.lock().unwrap(), expected);
                    }
                }
            }
        }
    }

    #[test]
    fn tip1143_t30_execution_trusts_exact_length_authenticated_provider_bytes() {
        // Intentionally violate ingestion's hash invariant in an isolated provider.
        // Metadata still commits to PUSH1 42; execution receives PUSH1 99.
        // This tests the execution trust boundary, not permission to ingest corruption.
        for asynchronous in [false, true] {
            let provider = fixture(false, Response::TrustedMismatch, 2);
            let reads = provider.reads.clone();
            let tx = SystemTx::new(Address::repeat_byte(0x44), Bytes::new());
            let result = if asynchronous {
                let mut execution = evm(AsyncDb::new(provider));
                let (result, pending) = drive(execution.system_call_async(tx));
                assert_eq!(pending, 2);
                result.unwrap().discard()
            } else {
                evm(Db::new(provider)).system_call(tx).unwrap().discard()
            };
            assert_eq!(result.stop, InstrStop::Return);
            assert_eq!(result.output.len(), 32);
            assert!(result.output[..31].iter().all(|byte| *byte == 0));
            assert_eq!(result.output[31], 99);
            assert_eq!(result.total_gas_spent, 2 * 28680 + 30);
            assert_eq!(*reads.lock().unwrap(), [0, 2]);
            assert!(result.logs.is_empty());
        }
    }

    #[derive(Debug)]
    struct StoredAsyncProvider {
        db: InMemoryDB,
        reads: Arc<Mutex<Vec<(B256, u32)>>>,
    }

    impl AsyncDatabase for StoredAsyncProvider {
        type Error = <InMemoryDB as Database>::Error;

        async fn get_account(
            &mut self,
            address: Address,
        ) -> Result<Option<AccountInfo>, Self::Error> {
            Database::get_account(&mut self.db, &address)
        }

        async fn get_code_by_hash(&mut self, hash: B256) -> Result<Bytecode, Self::Error> {
            self.reads.lock().unwrap().push((hash, 0));
            Database::get_code_by_hash(&mut self.db, &hash)
        }

        async fn get_storage(&mut self, address: Address, key: Word) -> Result<Word, Self::Error> {
            Database::get_storage(&mut self.db, &address, &key)
        }

        async fn get_block_hash(&mut self, number: Word) -> Result<B256, Self::Error> {
            Database::get_block_hash(&mut self.db, &number)
        }

        async fn get_code_chunk_by_hash(
            &mut self,
            hash: B256,
            index: u32,
        ) -> Result<Option<CodeChunk>, Self::Error> {
            let mut yielded = false;
            poll_fn(|cx| {
                if yielded {
                    Poll::Ready(())
                } else {
                    yielded = true;
                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
            })
            .await;
            self.reads.lock().unwrap().push((hash, index));
            Database::get_code_chunk_by_hash(&mut self.db, &hash, index)
        }
    }

    #[test]
    fn tip1143_t10_t31_async_persisted_kinds_survive_suspension_and_execution() {
        let owner = Address::repeat_byte(0x44);
        let delegate = Address::repeat_byte(0x77);
        let marker = Bytecode::new_eip7702(delegate);
        for delegation in [false, true] {
            let code = if delegation {
                marker.clone()
            } else {
                Bytecode::new_legacy(marker.original_bytes())
            };
            let mut db = InMemoryDB::default();
            db.insert_account_info(&owner, AccountInfo::default().with_code(code.clone()));
            db.insert_account_info(
                &delegate,
                AccountInfo::default().with_code(Bytecode::new_legacy(Bytes::from_static(&[
                    0x60, 42, 0x60, 0, 0x52, 0x60, 32, 0x60, 0, 0xf3,
                ]))),
            );
            let reads = Arc::default();
            let mut provider = StoredAsyncProvider { db, reads };
            let (chunk, pending) =
                drive(AsyncDatabase::get_code_chunk_by_hash(&mut provider, code.hash_slow(), 0));
            assert_eq!(pending, 1);
            let chunk = chunk.unwrap().unwrap();
            assert_eq!(chunk.bytecode().kind(), code.kind());
            assert_eq!(chunk.bytecode(), code);
            let reads = provider.reads.clone();
            reads.lock().unwrap().clear();
            let mut execution = evm(AsyncDb::new(provider));
            let (result, _) =
                drive(execution.system_call_async(SystemTx::new(owner, Bytes::new())));
            let result = result.unwrap().discard();
            assert_eq!(
                result.stop,
                if delegation { InstrStop::Return } else { InstrStop::OpcodeNotFound }
            );
            if delegation {
                assert_eq!(Word::from_be_slice(&result.output), Word::from(42));
            }
            let reads = reads.lock().unwrap();
            assert_eq!(reads.len(), 1);
            assert_eq!(
                reads[0].0,
                if delegation {
                    Bytecode::new_legacy(Bytes::from_static(&[
                        0x60, 42, 0x60, 0, 0x52, 0x60, 32, 0x60, 0, 0xf3,
                    ]))
                    .hash_slow()
                } else {
                    marker.hash_slow()
                }
            );
            assert!(reads.iter().all(|(_, index)| *index == 0));
        }
    }

    #[test]
    fn tip1143_t30_reached_marker_prefix_is_an_ordinary_invalid_legacy_opcode() {
        for asynchronous in [false, true] {
            let mut provider = fixture(false, Response::Valid, 2);
            let mut bytes = vec![0; CHUNK + 23];
            bytes[..3].copy_from_slice(&[0xef, 1, 0]);
            provider.code = bytes.into();
            let reads = provider.reads.clone();
            let tx = SystemTx::new(Address::repeat_byte(0x44), Bytes::new());
            let result = if asynchronous {
                let mut execution = evm(AsyncDb::new(provider));
                let (result, pending) = drive(execution.system_call_async(tx));
                assert_eq!(pending, 1);
                result.unwrap().discard()
            } else {
                evm(Db::new(provider)).system_call(tx).unwrap().discard()
            };
            assert_eq!(result.stop, InstrStop::OpcodeNotFound);
            assert!(result.output.is_empty());
            assert!(result.logs.is_empty());
            assert_eq!(*reads.lock().unwrap(), [0]);
        }
    }

    /// T30: multi payloads with delegation-like prefixes are always legacy.
    #[test]
    fn tip1143_t30_marker_shaped_remote_payloads_execute_as_legacy() {
        for length in [23, 24] {
            for skip_marker in [false, true] {
                for asynchronous in [false, true] {
                    let mut provider = fixture(false, Response::Valid, 2);
                    let mut code = vec![
                        0x61,
                        ((CHUNK + if skip_marker { 3 } else { 0 }) >> 8) as u8,
                        (CHUNK + if skip_marker { 3 } else { 0 }) as u8,
                        0x56,
                    ];
                    code.resize(CHUNK, 0);
                    code.extend([
                        0xef, 1, 0, 0x5b, 0x60, 42, 0x60, 0, 0x52, 0x60, 32, 0x60, 0, 0xf3,
                    ]);
                    code.resize(CHUNK + length, 0);
                    provider.code = code.into();
                    let reads = provider.reads.clone();
                    let tx = SystemTx::new(Address::repeat_byte(0x44), Bytes::new());
                    let result = if asynchronous {
                        let mut execution = evm(AsyncDb::new(provider));
                        let (result, pending) = drive(execution.system_call_async(tx));
                        assert_eq!(pending, 2);
                        result.unwrap().discard()
                    } else {
                        evm(Db::new(provider)).system_call(tx).unwrap().discard()
                    };
                    // Entering the prefix is an invalid jump destination; reading
                    // it for jump analysis must still never parse a designator.
                    assert_eq!(
                        result.stop,
                        if skip_marker { InstrStop::Return } else { InstrStop::InvalidJump }
                    );
                    if skip_marker {
                        assert_eq!(Word::from_be_slice(&result.output), Word::from(42));
                        assert_eq!(result.total_gas_spent, 2 * 28680 + 30);
                    }
                    assert_eq!(*reads.lock().unwrap(), [0, 1]);
                    assert!(result.logs.is_empty());
                }
            }
        }
    }

    #[test]
    fn tip1143_t40_repaired_provider_retries_with_retained_production_cache() {
        for copy in [false, true] {
            for index in [0, 2] {
                for response in [
                    Response::Missing,
                    Response::Short,
                    Response::Empty,
                    Response::Long,
                    Response::Error,
                ] {
                    let provider = fixture(copy, response, index);
                    let reads = provider.reads.clone();
                    let mut cache = CacheDB::new(Db::new(provider));
                    let tx = SystemTx::new(Address::repeat_byte(0x44), Bytes::new());
                    {
                        let mut execution = evm(&mut cache);
                        let result = execution.system_call(tx.clone());
                        assert!(
                            matches!(result, Err(HandlerError::Database(_))),
                            "copy={copy}, index={index}, response={response:?}"
                        );
                    }
                    let failed_reads = reads.lock().unwrap().clone();
                    assert_eq!(failed_reads.last(), Some(&index));
                    // Repair only the backing provider. Keep the actual CacheDB, including
                    // any raw payload it retained, and discard the fatally aborted Evm.
                    cache.db.inner_mut().response = Response::Valid;
                    let before_retry = reads.lock().unwrap().len();
                    {
                        let mut execution = evm(&mut cache);
                        let result = execution.system_call(tx.clone()).unwrap().discard();
                        assert_eq!(result.stop, InstrStop::Return);
                        assert!(result.logs.is_empty());
                        if copy {
                            assert_eq!(result.output.as_ref(), &[0, 0x5b]);
                            assert_eq!(result.total_gas_spent, 3 * 28680 + 24);
                        } else {
                            assert_eq!(result.output.len(), 32);
                            assert_eq!(result.output[31], 42);
                            assert_eq!(result.total_gas_spent, 2 * 28680 + 30);
                        }
                    }
                    let after_retry = reads.lock().unwrap().clone();
                    assert_eq!(
                        after_retry[before_retry..].iter().filter(|i| **i == index).count(),
                        1,
                        "the malformed/missing/error response must not satisfy a repaired read"
                    );
                    // A third fresh Evm remains logically cold while using validated bytes.
                    let result = evm(&mut cache).system_call(tx).unwrap().discard();
                    assert_eq!(result.stop, InstrStop::Return);
                    assert_eq!(
                        result.total_gas_spent,
                        if copy { 3 * 28680 + 24 } else { 2 * 28680 + 30 }
                    );
                    assert_eq!(*reads.lock().unwrap(), after_retry);
                }
            }
        }
    }

    #[test]
    fn tip1143_t40_contextual_rejection_evicts_both_retained_cache_layers() {
        for response in [Response::Short, Response::Long, Response::Missing, Response::Error] {
            let provider = fixture(false, response, 2);
            let reads = provider.reads.clone();
            let mut cache = CacheDB::new(CacheDB::new(Db::new(provider)));
            let tx = SystemTx::new(Address::repeat_byte(0x44), Bytes::new());
            {
                let mut execution = evm(&mut cache);
                assert!(matches!(
                    execution.system_call(tx.clone()),
                    Err(HandlerError::Database(_))
                ));
            }
            assert_eq!(*reads.lock().unwrap(), [0, 2]);
            cache.db.db.inner_mut().response = Response::Valid;
            let result = evm(&mut cache).system_call(tx.clone()).unwrap().discard();
            assert_eq!(result.stop, InstrStop::Return);
            assert_eq!(Word::from_be_slice(&result.output), Word::from(42));
            assert_eq!(result.total_gas_spent, 2 * 28680 + 30);
            // Valid zero survives; both caches discard the rejected remote key.
            assert_eq!(*reads.lock().unwrap(), [0, 2, 2]);
            let repeated = evm(&mut cache).system_call(tx).unwrap().discard();
            assert_eq!(repeated, result);
            assert_eq!(*reads.lock().unwrap(), [0, 2, 2]);
        }
    }

    /// Requested chunks and provider reads stay sparse across transaction resets.
    #[test]
    fn tip1143_t12_sparse_analysis_and_cached_reads() {
        let mut code = vec![0x60, 0, 0x60, 0, 0xa0]; // One explicit empty LOG0.
        let target = 39 * CHUNK;
        code.extend([0x62, (target >> 16) as u8, (target >> 8) as u8, target as u8, 0x56]);
        code.resize(target, 0);
        code.push(0x5b); // One original byte, followed only by synthetic STOP.
        let provider = SuspendingProvider {
            code: code.into(),
            fail_index: 0,
            response: Response::Valid,
            reads: Arc::default(),
            polls: Arc::default(),
        };
        let reads = provider.reads.clone();
        let mut execution = evm(Db::new(provider));
        let owner = Address::repeat_byte(0x44);
        {
            let account = execution.state_mut().account(&owner).unwrap();
            assert_eq!(account.code_size(), Some((target + 1) as u32));
            assert!(account.code_chunks().is_empty());
        }
        assert!(reads.lock().unwrap().is_empty());
        for (round, tariff) in [28680, 1000, 28680].into_iter().enumerate() {
            let result = execution.execute_system_call(SystemTx::new(owner, Bytes::new())).unwrap();
            assert_eq!(result.stop, InstrStop::Stop);
            assert_eq!(result.total_gas_spent, 2 * tariff + 396);
            assert!(result.output.is_empty());
            let logs = execution.state_mut().logs();
            assert_eq!(logs.len(), if round == 1 { 2 } else { 1 });
            for log in logs {
                assert_eq!(log.address, owner);
                assert!(log.data.topics().is_empty());
                assert!(log.data.data.is_empty());
            }
            let account = execution.state_mut().account(&owner).unwrap();
            let mut indices = account.code_chunks().keys().copied().collect::<Vec<_>>();
            indices.sort_unstable();
            assert_eq!(indices, [0, 39]);
            assert!(account.code_chunks().values().all(|chunk| chunk.is_warm));
            assert_eq!(*reads.lock().unwrap(), [0, 39]);
            drop(account);
            if round == 1 {
                execution.state_mut().clear_transaction_state();
                assert!(execution.state_mut().account(&owner).unwrap().code_chunks().is_empty());
            }
        }
    }

    /// Failures carry structured context through DatabaseError type erasure.
    /// CodeChunkError exports code_hash, index, expected_length and actual_length;
    /// missing/provider failures have no actual length. Error::source retains a
    /// reported provider error. System calls have no transaction hash to invent.
    #[test]
    fn tip1143_t29_t33_required_read_diagnostics_preserve_lengths_and_provider_source() {
        for index in [0, 2] {
            for response in [
                Response::Missing,
                Response::Short,
                Response::Empty,
                Response::Long,
                Response::Error,
            ] {
                let provider = fixture(false, response, index);
                let hash = keccak256(&provider.code);
                let expected = if index == 0 { CHUNK } else { 11 };
                let reads = provider.reads.clone();
                let mut execution = evm(Db::new(provider));
                let result = execution
                    .execute_system_call(SystemTx::new(Address::repeat_byte(0x44), Bytes::new()));
                let Err(HandlerError::Database(error)) = result else {
                    panic!("required chunk failure must be a node error");
                };
                let diagnostic = error
                    .downcast_ref::<evm2::CodeChunkError>()
                    .expect("structured chunk context survives erasure");
                assert_eq!(diagnostic.code_hash, hash);
                assert_eq!(diagnostic.index, index);
                assert_eq!(diagnostic.expected_length, Some(expected));
                assert_eq!(
                    diagnostic.actual_length,
                    match response {
                        Response::Short => Some(expected - 1),
                        Response::Empty => Some(0),
                        Response::Long => Some(expected + 1),
                        _ => None,
                    }
                );
                if matches!(response, Response::Error) {
                    let source = std::error::Error::source(diagnostic).unwrap();
                    assert_eq!(source.to_string(), "TIP1143 async sentinel");
                }
                assert_eq!(*reads.lock().unwrap(), if index == 0 { vec![0] } else { vec![0, 2] });
                assert!(execution.state_mut().logs().is_empty());
            }
        }
    }

    #[test]
    fn tip1143_t11_async_raw_contract_preserves_none_and_errors_after_suspension() {
        for response in [Response::Valid, Response::Missing, Response::Error] {
            let mut provider = fixture(false, response, 2);
            let hash = keccak256(&provider.code);
            for index in [0, 2, 3, u32::MAX] {
                let (result, pending) =
                    drive(AsyncDatabase::get_code_chunk_by_hash(&mut provider, hash, index));
                assert_eq!(pending, 1);
                if index >= 3 || (index == 2 && matches!(response, Response::Missing)) {
                    assert!(result.unwrap().is_none());
                } else if index == 2 && matches!(response, Response::Error) {
                    assert_eq!(result.unwrap_err().to_string(), "TIP1143 async sentinel");
                } else {
                    let expected = provider.code.chunks(CHUNK).nth(index as usize).unwrap();
                    let chunk = result.unwrap().unwrap();
                    assert_eq!(&chunk.bytes()[..chunk.payload_len()], expected);
                }
            }
            assert_eq!(*provider.reads.lock().unwrap(), [0, 2, 3, u32::MAX]);
            assert_eq!(provider.polls.load(Ordering::SeqCst), 8);
        }
    }

    #[derive(Debug)]
    struct NestedProvider {
        parent: SuspendingProvider,
        child: SuspendingProvider,
    }

    impl AsyncDatabase for NestedProvider {
        type Error = io::Error;

        async fn get_account(
            &mut self,
            address: Address,
        ) -> Result<Option<AccountInfo>, Self::Error> {
            Ok(if address == Address::repeat_byte(0x66) {
                self.parent.account(Address::repeat_byte(0x44))
            } else {
                self.child.account(address)
            })
        }

        async fn get_code_by_hash(&mut self, _: B256) -> Result<Bytecode, Self::Error> {
            panic!("nested async execution reconstructed code")
        }

        async fn get_storage(&mut self, _: Address, _: Word) -> Result<Word, Self::Error> {
            Ok(Word::ZERO)
        }

        async fn get_block_hash(&mut self, _: Word) -> Result<B256, Self::Error> {
            Ok(B256::ZERO)
        }

        async fn get_code_chunk_by_hash(
            &mut self,
            hash: B256,
            index: u32,
        ) -> Result<Option<CodeChunk>, Self::Error> {
            if hash == keccak256(&self.parent.code) {
                AsyncDatabase::get_code_chunk_by_hash(&mut self.parent, hash, index).await
            } else {
                AsyncDatabase::get_code_chunk_by_hash(&mut self.child, hash, index).await
            }
        }
    }

    #[test]
    fn tip1143_t11_t39_suspended_remote_child_preserves_parent_frame_and_pool_reuse() {
        for revert in [false, true] {
            let mut child = fixture(false, Response::Valid, 0);
            if revert {
                let mut code = child.code.to_vec();
                *code.last_mut().unwrap() = 0xfd;
                child.code = code.into();
            }
            let mut parent_code = vec![0x61, (CHUNK >> 8) as u8, CHUNK as u8, 0x56];
            parent_code.resize(CHUNK, 0);
            parent_code.extend([0x5b, 0x60, 0xa1, 0x60, 0, 0x52, 0x60, 0x51]);
            parent_code.extend([0x60, 32, 0x60, 32, 0x60, 0, 0x60, 0, 0x60, 0, 0x73]);
            parent_code.extend_from_slice(Address::repeat_byte(0x44).as_slice());
            parent_code.extend([0x62, 3, 0x0d, 0x40, 0xf1, 0x60, 64, 0x52, 0x60, 96, 0x52]);
            let resumed_pc = parent_code.len();
            parent_code
                .extend([0x58, 0x60, 128, 0x52, 0x3d, 0x60, 160, 0x52, 0x60, 192, 0x60, 0, 0xf3]);
            let parent = SuspendingProvider {
                code: parent_code.into(),
                fail_index: 0,
                response: Response::Valid,
                reads: Arc::default(),
                polls: Arc::default(),
            };
            let parent_reads = parent.reads.clone();
            let child_reads = child.reads.clone();
            let parent_polls = parent.polls.clone();
            let child_polls = child.polls.clone();
            let mut execution = evm(AsyncDb::new(NestedProvider { parent, child }));
            let mut previous = None;
            for round in 0..2 {
                let (result, pending) = drive(
                    execution
                        .system_call_async(SystemTx::new(Address::repeat_byte(0x66), Bytes::new())),
                );
                let result = result.unwrap().discard();
                assert_eq!(pending, if round == 0 { 4 } else { 0 });
                assert_eq!(result.stop, InstrStop::Return);
                let expected = [
                    Word::from(0xa1),
                    Word::from(42),
                    Word::from(u8::from(!revert)),
                    Word::from(0x51),
                    Word::from(resumed_pc),
                    Word::from(32),
                ];
                assert_eq!(result.output.len(), 192);
                for (actual, expected) in result.output.as_chunks::<32>().0.iter().zip(expected) {
                    assert_eq!(Word::from_be_slice(actual), expected);
                }
                assert!(result.logs.is_empty());
                if let Some(prior) = &previous {
                    assert_eq!(&result, prior);
                }
                previous = Some(result);
                assert_eq!(*parent_reads.lock().unwrap(), [0, 1]);
                assert_eq!(*child_reads.lock().unwrap(), [0, 2]);
                assert_eq!(parent_polls.load(Ordering::SeqCst), 4);
                assert_eq!(child_polls.load(Ordering::SeqCst), 4);
            }
        }
    }

    /// Chunk handles check bounds and skipped loads before fetching payloads.
    /// Warming is explicit; the interpreter caller reserves gas first.
    #[test]
    fn tip1143_t09_state_bounds_and_skipped_loads_precede_provider_calls() {
        for size in [0, 1, 24576, 24577, 981600] {
            let provider = SuspendingProvider {
                code: Bytes::from(vec![0; size]),
                fail_index: 0,
                response: Response::Valid,
                reads: Arc::default(),
                polls: Arc::default(),
            };
            let reads = provider.reads.clone();
            let mut execution = evm(Db::new(provider));
            let owner = Address::repeat_byte(0x44);
            let expected_size = if size == 0 || size > CHUNK { Some(size as u32) } else { None };
            assert_eq!(execution.state_mut().account(&owner).unwrap().code_size(), expected_size);
            let count = size.div_ceil(CHUNK) as u32;
            for index in [count, count + 1, u32::MAX] {
                for skip in [false, true] {
                    assert!(
                        execution
                            .state_mut()
                            .load_code_chunk(&owner, index, skip)
                            .unwrap()
                            .is_none()
                    );
                    assert!(reads.lock().unwrap().is_empty());
                    assert!(
                        execution.state_mut().account(&owner).unwrap().code_chunks().is_empty()
                    );
                }
            }
            if size == 0 {
                continue;
            }
            assert!(matches!(
                execution.state_mut().load_code_chunk(&owner, 0, true),
                Err(evm2::LoadError::ColdLoadSkipped)
            ));
            assert!(reads.lock().unwrap().is_empty());
            let checkpoint = execution.state_mut().checkpoint();
            let first = {
                let mut chunk =
                    execution.state_mut().load_code_chunk(&owner, 0, false).unwrap().unwrap();
                assert!(!chunk.is_warm());
                assert!(chunk.warm());
                chunk.get().clone()
            };
            assert_eq!(first.payload_len(), size.min(CHUNK));
            assert_eq!(
                execution.state_mut().account(&owner).unwrap().code_size(),
                Some(size as u32)
            );
            {
                let mut second =
                    execution.state_mut().load_code_chunk(&owner, 0, true).unwrap().unwrap();
                assert!(!second.warm());
                assert_eq!(first.bytes(), second.get().bytes());
            }
            assert_eq!(*reads.lock().unwrap(), [0]);
            let features = execution.version().features;
            execution.state_mut().rollback(checkpoint, features);
            // Bytes can remain cached, but skip-cold still refuses logical access.
            assert!(matches!(
                execution.state_mut().load_code_chunk(&owner, 0, true),
                Err(evm2::LoadError::ColdLoadSkipped)
            ));
            let mut reloaded =
                execution.state_mut().load_code_chunk(&owner, 0, false).unwrap().unwrap();
            assert!(reloaded.warm());
            assert_eq!(*reads.lock().unwrap(), [0]);
        }
    }

    #[derive(Clone, Debug)]
    struct NestedFailureProvider {
        inner: SuspendingProvider,
        parent: Bytes,
    }

    impl Database for NestedFailureProvider {
        type Error = io::Error;

        fn get_account(&mut self, address: &Address) -> Result<Option<AccountInfo>, Self::Error> {
            if *address == Address::repeat_byte(0x66) {
                Ok(Some(AccountInfo {
                    nonce: 1,
                    code_hash: keccak256(&self.parent),
                    ..Default::default()
                }))
            } else {
                Ok(self.inner.account(*address))
            }
        }

        fn get_code_by_hash(&mut self, _: &B256) -> Result<Bytecode, Self::Error> {
            Ok(Bytecode::new_legacy(self.parent.clone()))
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
            if *hash == keccak256(&self.parent) {
                assert_eq!(index, 0);
                Ok(Some(CodeChunk::new(self.parent.clone())))
            } else {
                self.inner.read(*hash, index)
            }
        }
    }

    #[test]
    fn tip1143_t29_nested_required_read_failure_cannot_be_caught_as_call_failure() {
        for index in [0, 2] {
            for response in [
                Response::Valid,
                Response::Missing,
                Response::Empty,
                Response::Short,
                Response::Long,
                Response::Error,
            ] {
                // CALL's ordinary success/failure bit is discarded. If the child
                // incorrectly turns a node error into an EVM halt, this parent
                // would return 99 and make the transaction appear successful.
                let mut parent = vec![0x60, 0, 0x60, 0, 0x60, 0, 0x60, 0, 0x60, 0, 0x73];
                parent.extend(Address::repeat_byte(0x44).as_slice());
                parent.extend([
                    0x62, 0x0f, 0x42, 0x40, 0xf1, 0x50, 0x60, 99, 0x60, 0, 0x52, 0x60, 32, 0x60, 0,
                    0xf3,
                ]);
                let provider = NestedFailureProvider {
                    inner: fixture(false, response, index),
                    parent: parent.into(),
                };
                let reads = provider.inner.reads.clone();
                let mut execution = evm(CacheDB::new(Db::new(provider)));
                let result =
                    execution.system_call(SystemTx::new(Address::repeat_byte(0x66), Bytes::new()));
                if matches!(response, Response::Valid) {
                    let result = result.unwrap().discard();
                    assert_eq!(result.stop, InstrStop::Return);
                    assert_eq!(Word::from_be_slice(&result.output), Word::from(99));
                    assert_eq!(*reads.lock().unwrap(), [0, 2]);
                } else {
                    assert!(
                        matches!(result, Err(HandlerError::Database(_))),
                        "index={index}, response={response:?}"
                    );
                    assert_eq!(
                        *reads.lock().unwrap(),
                        if index == 0 { vec![0] } else { vec![0, 2] }
                    );
                }
            }
        }
    }

    #[derive(Clone, Debug)]
    struct FullRecordProvider {
        inner: SuspendingProvider,
        full_reads: Arc<AtomicUsize>,
    }

    impl Database for FullRecordProvider {
        type Error = io::Error;

        fn get_account(&mut self, address: &Address) -> Result<Option<AccountInfo>, Self::Error> {
            Ok(self.inner.account(*address))
        }

        fn get_code_by_hash(&mut self, hash: &B256) -> Result<Bytecode, Self::Error> {
            assert_eq!(*hash, keccak256(&self.inner.code));
            self.full_reads.fetch_add(1, Ordering::SeqCst);
            Ok(Bytecode::new_legacy(self.inner.code.clone()))
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
            self.inner.read(*hash, index)
        }
    }

    #[test]
    fn tip1143_t29_missing_multi_zero_cannot_use_available_full_record() {
        for cached in [false, true] {
            let mut provider = FullRecordProvider {
                inner: fixture(false, Response::Missing, 0),
                full_reads: Arc::default(),
            };
            let hash = keccak256(&provider.inner.code);
            // Positive control: the original full record exists and is usable
            // by non-consensus consumers, despite the missing indexed payload.
            let original = Database::get_code_by_hash(&mut provider, &hash).unwrap();
            assert_eq!(original.original_byte_slice(), provider.inner.code.as_ref());
            let full_reads = provider.full_reads.clone();
            let reads = provider.inner.reads.clone();
            let mut execution = if cached {
                let mut database = CacheDB::new(Db::new(provider));
                assert_eq!(DynDatabase::get_code_by_hash(&mut database, &hash).unwrap(), original);
                evm(database)
            } else {
                evm(Db::new(provider))
            };
            let before = full_reads.load(Ordering::SeqCst);
            let result =
                execution.system_call(SystemTx::new(Address::repeat_byte(0x44), Bytes::new()));
            assert!(matches!(result, Err(HandlerError::Database(_))));
            assert_eq!(full_reads.load(Ordering::SeqCst), before);
            assert_eq!(*reads.lock().unwrap(), [0]);
        }
    }

    #[test]
    fn tip1143_t29_legacy_unknown_length_enforces_nonempty_bounded_payload() {
        for (size, response, actual) in [
            (1, Response::Missing, None),
            (1, Response::Empty, Some(0)),
            (1, Response::Short, Some(0)),
            (17, Response::Oversized, Some(24577)),
            (17, Response::Error, None),
        ] {
            for asynchronous in [false, true] {
                let provider = SuspendingProvider {
                    code: Bytes::from(vec![0; size]),
                    fail_index: 0,
                    response,
                    reads: Arc::default(),
                    polls: Arc::default(),
                };
                let hash = keccak256(&provider.code);
                let reads = provider.reads.clone();
                let tx = SystemTx::new(Address::repeat_byte(0x44), Bytes::new());
                let result = if asynchronous {
                    let mut execution = evm(AsyncDb::new(provider));
                    let (result, pending) = drive(execution.system_call_async(tx));
                    assert_eq!(pending, 0);
                    result.map(|result| result.discard()).map_err(|error| match error {
                        AsyncError::Inner(error) => error,
                        other => panic!("unexpected fiber failure: {other:?}"),
                    })
                } else {
                    evm(CacheDB::new(Db::new(provider)))
                        .system_call(tx)
                        .map(|result| result.discard())
                };
                let Err(HandlerError::Database(error)) = result else {
                    panic!(
                        "legacy malformed row became an EVM result: size={size}, response={response:?}"
                    );
                };
                let diagnostic = error.downcast_ref::<evm2::CodeChunkError>().unwrap();
                assert_eq!(diagnostic.code_hash, hash);
                assert_eq!(diagnostic.index, 0);
                // Legacy metadata cannot promise an exact size before this read.
                assert_eq!(diagnostic.expected_length, None);
                assert_eq!(diagnostic.actual_length, actual);
                assert_eq!(*reads.lock().unwrap(), [0]);
            }
        }
    }

    #[test]
    fn tip1143_t11_t22_async_reservation_precedes_first_pending_payload() {
        for operation in [0, 1, 2] {
            let mut provider = fixture(operation == 2, Response::Valid, 0);
            let mut code = provider.code.to_vec();
            let required_budget = match operation {
                0 => {
                    code[0] = 0; // Entry followed immediately by STOP.
                    28680
                }
                1 => {
                    code.truncate(2 * CHUNK + 1); // Remote JUMPDEST then synthetic STOP.
                    2 * 28680 + 11 // PUSH2 + JUMP, before destination JUMPDEST.
                }
                2 => {
                    code[8] = 0; // STOP immediately after the boundary-spanning copy.
                    3 * 28680 + 18
                }
                _ => unreachable!(),
            };
            provider.code = code.into();
            for budget in
                [required_budget - 1, required_budget, required_budget + 1, required_budget + 4]
            {
                let fresh = SuspendingProvider {
                    reads: Arc::default(),
                    polls: Arc::default(),
                    ..provider.clone()
                };
                let reads = fresh.reads.clone();
                let polls = fresh.polls.clone();
                let mut execution = evm(AsyncDb::new(fresh));
                let (result, pending) = drive(execution.system_call_async(
                    SystemTx::new(Address::repeat_byte(0x44), Bytes::new()).with_gas_limit(budget),
                ));
                let result = result.unwrap().discard();
                let expected = if budget < required_budget {
                    if operation == 0 { vec![] } else { vec![0] }
                } else {
                    match operation {
                        0 => vec![0],
                        1 => vec![0, 2],
                        2 => vec![0, 1, 2],
                        _ => unreachable!(),
                    }
                };
                assert_eq!(*reads.lock().unwrap(), expected);
                assert_eq!(pending, expected.len());
                assert_eq!(polls.load(Ordering::SeqCst), 2 * expected.len());
                if budget < required_budget || (operation == 1 && budget < required_budget + 4) {
                    assert_eq!(result.stop, InstrStop::OutOfGas);
                    assert_eq!(result.total_gas_spent, budget);
                } else {
                    assert_eq!(result.stop, InstrStop::Stop);
                    assert_eq!(
                        result.total_gas_spent,
                        required_budget + 4 * u64::from(operation == 1)
                    );
                }
                assert!(result.logs.is_empty());
            }
        }
    }
}

#[test]
fn tip1143_t09_t35_original_inmemory_rows_need_no_chunk_index_or_rewrite() {
    let owner = Address::repeat_byte(0x44);
    for size in [0, 1, 24575, 24576] {
        let original = Bytes::from(vec![0x5b; size]);
        let hash = keccak256(&original);
        let mut database = InMemoryDB::default();
        database.insert_account_info(
            &owner,
            AccountInfo {
                nonce: 7,
                balance: Word::from(123),
                code_hash: hash,
                code: Some(Bytecode::new_legacy(original.clone())),
                ..Default::default()
            },
        );
        let before = database.account_info(&owner).unwrap().clone();
        assert!(before.code_metadata().is_none());
        for index in [1, 40, u32::MAX] {
            assert!(
                DynDatabase::get_code_chunk_by_hash(&mut database, &hash, index).unwrap().is_none()
            );
        }
        let zero = DynDatabase::get_code_chunk_by_hash(&mut database, &hash, 0).unwrap();
        if size == 0 {
            assert!(zero.is_none());
        } else if size <= 24540 {
            assert_eq!(zero.unwrap().bytes(), original.as_ref());
        } else {
            assert!(zero.is_none()); // Legacy record is accessed through the explicit full-record API below.
        }
        assert_eq!(database.account_info(&owner).unwrap(), &before);
        assert_eq!(
            DynDatabase::get_code_by_hash(&mut database, &hash).unwrap().original_byte_slice(),
            original.as_ref()
        );
    }
}

/// Counted-outside-cache counts logical adapter calls, including cache hits.
#[test]
fn tip1143_t10_counted_cache_composition_preserves_results_and_attempts() {
    let (provider, hash) = provider();
    let reads = provider.reads.clone();
    let mut database = DbStats::new(CacheDB::new(Db::new(provider)));
    for round in 0..3 {
        assert_eq!(
            DynDatabase::get_code_chunk_by_hash(&mut database, &hash, 0).unwrap().unwrap().bytes(),
            &[0x60, 0x2a, 0]
        );
        assert!(DynDatabase::get_code_chunk_by_hash(&mut database, &hash, 1).unwrap().is_none());
        assert_eq!(
            DynDatabase::get_code_chunk_by_hash(&mut database, &hash, u32::MAX)
                .unwrap_err()
                .to_string(),
            "TIP1143 provider sentinel"
        );
        assert_eq!(database.counts().get_code_chunk_by_hash, 3 * (round + 1));
    }
    // Absence/errors must remain retryable; only successful payloads are cached.
    assert_eq!(
        *reads.borrow(),
        [
            (hash, 0),
            (hash, 1),
            (hash, u32::MAX),
            (hash, 1),
            (hash, u32::MAX),
            (hash, 1),
            (hash, u32::MAX),
        ]
    );
}

#[cfg(feature = "serde")]
#[test]
fn tip1143_t08_t35_account_serialization_preserves_optional_identity_and_extension() {
    for size in [0, 17, 24577, 981600] {
        let bytes = vec![0; size];
        let metadata = (size > 24540).then(|| {
            CodeMetadata::new(size as u32, bytes.chunks(24540).map(keccak256).collect()).unwrap()
        });
        let account = AccountInfo {
            nonce: 19,
            balance: Word::from(987654),
            code_hash: keccak256(&bytes),
            extension: (metadata).map(evm2::evm::AccountExtension::chunked).unwrap_or_default(),
            ..Default::default()
        };
        let json = serde_json::to_value(&account).unwrap();
        let restored = serde_json::from_value::<AccountInfo>(json.clone()).unwrap();
        assert_eq!(restored, account);
        assert_eq!(restored.code_metadata(), account.code_metadata());
        let packed = rmp_serde::to_vec_named(&account).unwrap();
        let restored = rmp_serde::from_slice::<AccountInfo>(&packed).unwrap();
        assert_eq!(restored, account);
        assert_eq!(restored.code_metadata(), account.code_metadata());
        if size > 24540 {
            for count in [0, 1, 41] {
                let mut malformed = json.clone();
                let mut extension = account.extension.encode()[..5].to_vec();
                extension.resize(5 + count * 32, 0);
                malformed["extension"] = serde_json::to_value(Bytes::from(extension)).unwrap();
                assert!(
                    serde_json::from_value::<AccountInfo>(malformed).is_err(),
                    "size={size}, count={count}"
                );
            }
        } else {
            assert!(restored.code_metadata().is_none());
        }
    }
}

/// TIP1143-T38: equal payload bytes at different indices remain different cache keys.
#[test]
fn tip1143_t38_identical_payloads_at_distinct_indices_do_not_alias_adapter_keys() {
    let (mut provider, hash) = provider();
    let identical = Bytes::from_static(&[0x5b, 0]);
    provider.rows.insert((hash, 1), identical.clone());
    provider.rows.insert((hash, 2), identical.clone());
    let reads = provider.reads.clone();
    let mut cache = CacheDB::new(Db::new(provider));
    for index in [2, 1, 2, 1] {
        assert_eq!(
            DynDatabase::get_code_chunk_by_hash(&mut cache, &hash, index).unwrap().unwrap().bytes(),
            &identical
        );
    }
    assert_eq!(*reads.borrow(), [(hash, 2), (hash, 1)]);
}

/// T10: identical persisted bytes can carry distinct known execution kinds.
#[test]
fn tip1143_t10_persisted_kinds_survive_typed_erased_counted_and_cached_reads() {
    let marker = Bytecode::new_eip7702(Address::repeat_byte(0x77));
    for code in [marker.clone(), Bytecode::new_legacy(marker.original_bytes())] {
        let hash = code.hash_slow();
        let mut stored = InMemoryDB::default();
        stored.insert_account_info(
            &Address::repeat_byte(0x44),
            AccountInfo::default().with_code(code.clone()),
        );
        let typed = Database::get_code_chunk_by_hash(&mut stored, &hash, 0).unwrap().unwrap();
        assert_eq!(typed.bytecode().kind(), code.kind());
        assert_eq!(typed.bytecode(), code);
        let mut cache = CacheDB::new(DbStats::new(Db::new(stored)));
        for _ in 0..2 {
            let erased: &mut dyn DynDatabase = &mut cache;
            let chunk = erased.get_code_chunk_by_hash(&hash, 0).unwrap().unwrap();
            assert_eq!(chunk.bytes(), &marker.original_bytes());
            assert_eq!(chunk.bytecode().kind(), code.kind());
            assert_eq!(chunk.bytecode(), code);
            assert_eq!(chunk.bytecode().kind(), code.kind());
        }
        assert_eq!(cache.db.counts().get_code_chunk_by_hash, 1);
    }
}
