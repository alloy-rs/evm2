//! Benchmarks evm2's parallel block execution on workloads shaped like Tempo's txgen presets.
//!
//! ```sh
//! cargo run -p evm2-cli --features jit --example parallel_bench -- \
//!     --workloads public-mix,tip20 --threads 1,2,4,8 --work 0,64 --jit both
//! ```
//!
//! Prints one CSV row per measurement. Every transaction calls a token-style contract that moves
//! one unit from the caller to a recipient and then hashes `work` times, standing in for the rest
//! of a TIP-20 transfer. A sender's transactions chain through its nonce, as they chain through
//! its fee-token balance on Tempo.

use alloy_consensus::{
    EthereumTxEnvelope, SignableTransaction, TxEip4844, TxLegacy, constants::ETH_TO_WEI,
    transaction::Recovered,
};
use alloy_eips::{eip2935::HISTORY_STORAGE_CODE, eip4788::BEACON_ROOTS_CODE};
use alloy_primitives::{Address, B256, Bytes, Signature, TxKind, U256};
use evm2::{
    BaseEvmTypes, Evm, Precompiles, SpecId,
    block::{
        BlockExecutor,
        ethereum::{
            EthBlockExecutionCtx, EthBlockExecutor, EthBlockRules, EthReceiptBuilder,
            ParallelExecutionStats, ParallelHints, StreamedTx,
        },
    },
    bytecode::Bytecode,
    env::BlockEnvExt,
    ethereum::ethereum_tx_registry,
    evm::{
        AccountInfo, BEACON_ROOTS_ADDRESS, Cache, DbResult, DynDatabase, DynDatabaseRef, EmptyDB,
        HISTORY_STORAGE_ADDRESS, InMemoryDB,
    },
    interpreter::Word,
};
use std::{
    num::NonZeroUsize,
    time::{Duration, Instant},
};

#[cfg(feature = "jit")]
use evm2_jit_runtime::{
    OptimizationLevel,
    evm2_evm::JitInterpreterRunner,
    runtime::{JitBackend, RuntimeConfig, RuntimeTuning},
};

type Tx = Recovered<EthereumTxEnvelope<TxEip4844>>;

const SPEC: SpecId = SpecId::PRAGUE;
const BENEFICIARY: Address = Address::repeat_byte(0xbe);
const TOKEN: Address = Address::repeat_byte(0x7a);
const HOT_TOKEN: Address = Address::repeat_byte(0x7b);
const HOT_VAULT: Address = Address::repeat_byte(0xa1);
const HOT_PORTAL: Address = Address::repeat_byte(0xa2);
const HOT_SUPPLY: Address = Address::repeat_byte(0xa3);
const DEAD: Address =
    Address::new([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xde, 0xad]);

/// The pre-block state as a shared database for the workers.
struct Snapshot(Cache);

impl DynDatabaseRef for Snapshot {
    fn get_account(&self, address: &Address) -> DbResult<Option<AccountInfo>> {
        Ok(self.0.accounts.get(address).cloned().flatten())
    }

    fn get_code_by_hash(&self, code_hash: &B256) -> DbResult<Bytecode> {
        Ok(self.0.contracts.get(code_hash).cloned().unwrap_or_default())
    }

    fn get_storage(&self, address: &Address, key: &Word) -> DbResult<Word> {
        Ok(self
            .0
            .storage
            .get(address)
            .and_then(|storage| storage.slots.get(key))
            .copied()
            .unwrap_or_default())
    }

    fn get_block_hash(&self, number: &Word) -> DbResult<B256> {
        DynDatabase::get_block_hash(&mut EmptyDB::default(), number)
    }
}

/// Moves one unit from the caller to the address in calldata word 0, then hashes calldata word 1
/// times. With `hot_fee`, every call also increments one shared slot, as Tempo's fee accumulator
/// and expiring nonce ring pointer do today.
fn token_code(hot_fee: bool) -> Bytes {
    let mut code = vec![
        0x33, 0x54, 0x60, 0x01, 0x90, 0x03, 0x33, 0x55, // balances[caller] -= 1
        0x60, 0x00, 0x35, 0x80, 0x54, 0x60, 0x01, 0x01, 0x90, 0x55, // balances[to] += 1
    ];
    if hot_fee {
        code.extend([0x61, 0x0f, 0xee, 0x54, 0x60, 0x01, 0x01, 0x61, 0x0f, 0xee, 0x55]);
    }
    code.extend([0x60, 0x20, 0x35]);
    let start = code.len() as u8;
    code.extend([
        0x5b, 0x80, 0x15, 0x60, 0x00, 0x57, 0x60, 0x20, 0x60, 0x00, 0x20, 0x60, 0x00, 0x52, 0x60,
        0x01, 0x90, 0x03, 0x60, start, 0x56,
    ]);
    let end = code.len() as u8;
    code[start as usize + 4] = end;
    code.extend([0x5b, 0x00]);
    code.into()
}

fn user(index: u64) -> Address {
    let mut bytes = [0u8; 20];
    bytes[0] = 0x55;
    bytes[12..].copy_from_slice(&index.to_be_bytes());
    Address::from(bytes)
}

fn database(users: u64) -> InMemoryDB {
    let mut database = InMemoryDB::default();
    for index in 0..users {
        let info = AccountInfo { balance: U256::from(ETH_TO_WEI), ..AccountInfo::default() };
        database.insert_account_info(&user(index), info);
    }
    for (address, code) in [
        (HISTORY_STORAGE_ADDRESS, HISTORY_STORAGE_CODE.clone()),
        (BEACON_ROOTS_ADDRESS, BEACON_ROOTS_CODE.clone()),
        (TOKEN, token_code(false)),
        (HOT_TOKEN, token_code(true)),
    ] {
        database.insert_account_info(
            &address,
            AccountInfo::default().with_code(Bytecode::new_legacy(code)),
        );
    }
    database
}

/// Deterministic xorshift generator.
struct Rng(u64);

impl Rng {
    const fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    const fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// A workload: its users and how each transaction picks its sender, recipient and token.
#[derive(Clone, Copy, Debug)]
enum Workload {
    /// `tip20.yml` with random recipients: transfers from random users to fresh addresses.
    Tip20,
    /// `tip20/recipient-users.yml`: transfers between users.
    Tip20Users,
    /// `tempo-mainnet-spam.yaml`: 100 spammers burning to one address.
    SpamDead,
    /// `public-mix.yml`: transfers, MPP opens, mints, zone and vault operations.
    PublicMix,
    /// `public-mix.yml` with the shared fee and nonce-ring slot Tempo writes today.
    PublicMixToday,
    /// `dex.yml`-like: every swap on one pool.
    DexHot,
}

impl Workload {
    fn parse(name: &str) -> Self {
        match name {
            "tip20" => Self::Tip20,
            "tip20-users" => Self::Tip20Users,
            "spam-dead" => Self::SpamDead,
            "public-mix" => Self::PublicMix,
            "public-mix-today" => Self::PublicMixToday,
            "dex-hot" => Self::DexHot,
            _ => panic!("unknown workload {name}"),
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Tip20 => "tip20",
            Self::Tip20Users => "tip20-users",
            Self::SpamDead => "spam-dead",
            Self::PublicMix => "public-mix",
            Self::PublicMixToday => "public-mix-today",
            Self::DexHot => "dex-hot",
        }
    }

    const fn users(self) -> u64 {
        match self {
            Self::SpamDead => 100,
            _ => 1_000,
        }
    }

    fn block(self, len: usize, work: u64) -> Vec<Tx> {
        let mut rng = Rng(0x5eed);
        let users = self.users();
        let mut nonces = vec![0u64; users as usize];
        (0..len)
            .map(|_| {
                let fresh = |rng: &mut Rng| Address::from_word(B256::from(U256::from(rng.next())));
                let roll = rng.below(100);
                let mut sender = rng.below(users);
                let (token, to) = match self {
                    Self::Tip20 => (TOKEN, fresh(&mut rng)),
                    Self::Tip20Users => (TOKEN, user(rng.below(users))),
                    Self::SpamDead => (TOKEN, DEAD),
                    Self::DexHot => (TOKEN, HOT_VAULT),
                    Self::PublicMix | Self::PublicMixToday => {
                        let token =
                            if matches!(self, Self::PublicMixToday) { HOT_TOKEN } else { TOKEN };
                        let to = match roll {
                            0 => {
                                sender = 0;
                                HOT_SUPPLY
                            }
                            1..=9 => HOT_PORTAL,
                            10..=19 => HOT_VAULT,
                            _ => fresh(&mut rng),
                        };
                        (token, to)
                    }
                };
                let nonce = nonces[sender as usize];
                nonces[sender as usize] += 1;
                let mut input = to.into_word().to_vec();
                input.extend_from_slice(&U256::from(work).to_be_bytes::<32>());
                let tx = TxLegacy {
                    nonce,
                    gas_price: 1,
                    gas_limit: 1_000_000,
                    to: TxKind::Call(token),
                    input: input.into(),
                    ..TxLegacy::default()
                };
                let signature = Signature::new(U256::ONE, U256::ONE, false);
                Recovered::new_unchecked(
                    EthereumTxEnvelope::Legacy(tx.into_signed(signature)),
                    user(sender),
                )
            })
            .collect()
    }
}

/// The interpreter, or the JIT shared by every EVM.
#[derive(Clone)]
struct Engine {
    #[cfg(feature = "jit")]
    jit: Option<JitBackend>,
}

impl Engine {
    const fn name(&self) -> &'static str {
        #[cfg(feature = "jit")]
        if self.jit.is_some() {
            return "jit";
        }
        "interpreter"
    }

    fn evm<'x>(&self, database: impl DynDatabase + 'x) -> Evm<'x, BaseEvmTypes> {
        let block = BlockEnvExt {
            number: U256::ONE,
            timestamp: U256::from(12),
            beneficiary: BENEFICIARY,
            gas_limit: U256::from(u64::MAX / 2),
            ..BlockEnvExt::default()
        };
        #[allow(unused_mut)]
        let mut evm =
            Evm::new(SPEC, block, ethereum_tx_registry(SPEC), database, Precompiles::base(SPEC));
        #[cfg(feature = "jit")]
        if let Some(jit) = &self.jit {
            evm.set_interpreter_runner(JitInterpreterRunner::new(jit.clone()));
        }
        evm
    }

    fn executor(&self, database: InMemoryDB) -> EthBlockExecutor<'static> {
        let ctx = EthBlockExecutionCtx {
            parent_hash: B256::repeat_byte(0x11),
            parent_beacon_block_root: Some(B256::repeat_byte(0x22)),
            ..EthBlockExecutionCtx::default()
        };
        let mut executor = EthBlockExecutor::new(
            self.evm(database),
            ctx,
            EthBlockRules::default(),
            EthReceiptBuilder::default(),
        );
        // A state hook, as a state-root task would install, so commits stream.
        executor.set_state_hook(drop);
        executor.apply_pre_execution_changes().unwrap();
        executor
    }
}

#[cfg(feature = "jit")]
fn jit_backend() -> JitBackend {
    let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
    JitBackend::new(RuntimeConfig {
        enabled: true,
        blocking: true,
        tuning: RuntimeTuning {
            jit_hot_threshold: 0,
            jit_worker_count: cpus,
            jit_opt_level: OptimizationLevel::Default,
            ..Default::default()
        },
        ..Default::default()
    })
    .expect("JIT backend")
}

fn engines(jit: &str) -> Vec<Engine> {
    let interpreter = Engine {
        #[cfg(feature = "jit")]
        jit: None,
    };
    match jit {
        "off" => vec![interpreter],
        #[cfg(feature = "jit")]
        "on" => vec![Engine { jit: Some(jit_backend()) }],
        #[cfg(feature = "jit")]
        "both" => vec![interpreter, Engine { jit: Some(jit_backend()) }],
        _ => panic!("--jit {jit} needs the jit feature (on, both) or is unknown"),
    }
}

fn sequential(engine: &Engine, database: &InMemoryDB, txs: &[Tx]) -> Duration {
    let mut executor = engine.executor(database.clone());
    let start = Instant::now();
    for tx in txs {
        executor.execute_transaction(tx).unwrap();
    }
    executor.finish().unwrap();
    start.elapsed()
}

fn parallel(
    engine: &Engine,
    database: &InMemoryDB,
    txs: &[Tx],
    threads: usize,
    hints: Option<&ParallelHints>,
) -> (Duration, ParallelExecutionStats, ParallelHints) {
    let snapshot = Snapshot(database.cache.clone());
    let mut executor = engine.executor(database.clone());
    let start = Instant::now();
    let output = executor
        .execute_transactions_parallel(
            txs,
            &snapshot,
            |database| engine.evm(database),
            NonZeroUsize::new(threads).unwrap(),
            hints,
            |_, _| {},
        )
        .unwrap();
    executor.finish().unwrap();
    (start.elapsed(), output.stats, output.dependencies)
}

/// Streams `txs` in at the rate sequential execution consumes them and returns how long after the
/// last arrival the block is committed.
fn stream_tail(
    engine: &Engine,
    database: &InMemoryDB,
    txs: &[Tx],
    threads: usize,
    gap: Duration,
    hints: Option<&ParallelHints>,
) -> (Duration, ParallelExecutionStats) {
    let snapshot = Snapshot(database.cache.clone());
    let mut executor = engine.executor(database.clone());
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        let producer = scope.spawn(|| {
            let begin = Instant::now();
            for (index, tx) in txs.iter().enumerate() {
                while begin.elapsed() < gap * index as u32 {
                    std::hint::spin_loop();
                }
                let dependencies =
                    hints.map(|h| h.dependencies()[index].clone()).unwrap_or_default();
                sender.send(StreamedTx::with_dependencies(tx, dependencies)).unwrap();
            }
            drop(sender);
            Instant::now()
        });
        let output = executor
            .execute_transaction_stream(
                receiver,
                &snapshot,
                |database| engine.evm(database),
                NonZeroUsize::new(threads).unwrap(),
                |_, _| {},
            )
            .unwrap();
        let done = Instant::now();
        let last = producer.join().unwrap();
        (done.saturating_duration_since(last), output.stats)
    })
}

fn list<T: std::str::FromStr>(value: &str) -> Vec<T>
where
    T::Err: std::fmt::Debug,
{
    value.split(',').map(|item| item.parse().unwrap()).collect()
}

fn main() {
    let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
    let mut workloads = vec![
        Workload::Tip20,
        Workload::Tip20Users,
        Workload::SpamDead,
        Workload::PublicMix,
        Workload::PublicMixToday,
        Workload::DexHot,
    ];
    let mut threads = vec![1, 2, 4, 8, 16, 32]
        .into_iter()
        .filter(|&t| t < cpus)
        .chain([cpus - 1])
        .collect::<Vec<_>>();
    let mut works = vec![0u64, 64, 256];
    let mut txs = 4_300usize;
    let mut runs = 5usize;
    let mut jit = "off".to_string();
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let value = args.next().expect("flag value");
        match flag.as_str() {
            "--workloads" => workloads = value.split(',').map(Workload::parse).collect(),
            "--threads" => threads = list(&value),
            "--work" => works = list(&value),
            "--txs" => txs = value.parse().unwrap(),
            "--runs" => runs = value.parse().unwrap(),
            "--jit" => jit = value,
            _ => panic!("unknown flag {flag}"),
        }
    }
    threads.dedup();
    println!("engine,workload,work,txs,cpus,mode,threads,ms,speedup,reexecuted,critical_path");
    for engine in engines(&jit) {
        for &workload in &workloads {
            for &work in &works {
                let block = workload.block(txs, work);
                let database = database(workload.users());
                // Warm up, compiling the contracts under the JIT.
                sequential(&engine, &database, &block);
                let exact =
                    parallel(&engine, &database, &block, threads[threads.len() - 1].max(2), None).2;
                let critical_path = exact.critical_path();
                let sequential_time =
                    (0..runs).map(|_| sequential(&engine, &database, &block)).min().unwrap();
                let row = |mode: &str, threads: usize, time: Duration, reexecuted: usize| {
                    println!(
                        "{},{},{work},{txs},{cpus},{mode},{threads},{:.3},{:.2},{reexecuted},{critical_path}",
                        engine.name(),
                        workload.name(),
                        time.as_secs_f64() * 1e3,
                        sequential_time.as_secs_f64() / time.as_secs_f64(),
                    );
                };
                row("sequential", 1, sequential_time, 0);
                let gap = sequential_time / txs as u32;
                for &threads in &threads {
                    for (mode, hints) in [("batch", None), ("batch-hints", Some(&exact))] {
                        let (time, stats) = (0..runs)
                            .map(|_| {
                                let (time, stats, _) =
                                    parallel(&engine, &database, &block, threads, hints);
                                (time, stats)
                            })
                            .min_by_key(|(time, _)| *time)
                            .unwrap();
                        row(mode, threads, time, stats.reexecuted);
                    }
                    // Tail after the last arrival; speedup is sequential time over that tail.
                    let (tail, stats) = stream_tail(&engine, &database, &block, threads, gap, None);
                    row("stream-tail", threads, tail, stats.reexecuted);
                }
            }
        }
    }
}
