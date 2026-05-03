//! Parallel Ethereum state test runner for evm2.

use alloy_primitives::{Address, B256, Bytes, U256};
use evm2::{
    Evm, EvmVersion,
    bytecode::Bytecode,
    env::BlockEnv,
    evm::{
        AccountInfo, InMemoryDB, logs_hash,
        transaction::{Error as EvmError, Transaction},
    },
    interpreter::SpecId,
    registry::TxRegistry,
};
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use k256::ecdsa::SigningKey;
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    env, fs,
    io::{self, IsTerminal},
    path::{Path, PathBuf},
    process,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use thiserror::Error;
use walkdir::WalkDir;

fn main() {
    let args = match Args::parse() {
        Ok(args) => args,
        Err(err) => {
            eprintln!("{err}");
            process::exit(2);
        }
    };

    let files = match find_json_tests(&args.paths) {
        Ok(files) => files,
        Err(err) => {
            eprintln!("{err}");
            process::exit(2);
        }
    };

    let result = run(files, args.jobs, args.keep_going);
    if let Err(err) = result {
        eprintln!("{err}");
        process::exit(1);
    }
}

#[derive(Clone, Debug)]
struct Args {
    paths: Vec<PathBuf>,
    jobs: usize,
    keep_going: bool,
}

impl Args {
    fn parse() -> Result<Self, String> {
        let mut paths = Vec::new();
        let mut jobs = thread::available_parallelism().map_or(1, |jobs| jobs.get()).min(28);
        let mut keep_going = false;
        let mut args = env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "-j" | "--jobs" => {
                    let Some(value) = args.next() else {
                        return Err("-j requires a value".to_string());
                    };
                    jobs = value.parse().map_err(|_| format!("invalid job count: {value}"))?;
                }
                _ if arg.starts_with("-j") && arg.len() > 2 => {
                    let value = &arg[2..];
                    jobs = value.parse().map_err(|_| format!("invalid job count: {value}"))?;
                }
                _ if arg.starts_with("--jobs=") => {
                    let value = &arg["--jobs=".len()..];
                    jobs = value.parse().map_err(|_| format!("invalid job count: {value}"))?;
                }
                "--keep-going" => keep_going = true,
                "-h" | "--help" => {
                    println!("usage: evm2-statetest [-j N] [--keep-going] <file-or-dir>...");
                    process::exit(0);
                }
                _ if arg.starts_with('-') => return Err(format!("unknown option: {arg}")),
                _ => paths.push(PathBuf::from(arg)),
            }
        }
        if paths.is_empty() {
            paths.push(PathBuf::from(
                "/home/doni/github/danipopes/revm/test-fixtures/legacytests/Constantinople/GeneralStateTests",
            ));
        }
        Ok(Self { paths, jobs: jobs.max(1), keep_going })
    }
}

fn find_json_tests(paths: &[PathBuf]) -> Result<Vec<PathBuf>, TestError> {
    let mut files = Vec::new();
    for path in paths {
        if path.is_file() {
            if path.extension().is_some_and(|ext| ext == "json") {
                files.push(path.clone());
            }
            continue;
        }
        if !path.exists() {
            return Err(TestError::InvalidPath(path.clone()));
        }
        for entry in WalkDir::new(path).follow_links(false) {
            let entry = entry.map_err(|err| TestError::Walk(path.clone(), err))?;
            if entry.file_type().is_file()
                && entry.path().extension().is_some_and(|ext| ext == "json")
            {
                files.push(entry.path().to_path_buf());
            }
        }
    }
    files.sort();
    if files.is_empty() {
        return Err(TestError::NoJsonFiles);
    }
    Ok(files)
}

fn run(files: Vec<PathBuf>, jobs: usize, keep_going: bool) -> Result<(), TestError> {
    let jobs = jobs.min(files.len()).max(1);
    let state = RunnerState::new(files);
    let mut handles = Vec::with_capacity(jobs);
    for i in 0..jobs {
        let state = state.clone();
        let handle = thread::Builder::new()
            .name(format!("statetest-{i}"))
            .spawn(move || worker(state, keep_going))
            .map_err(TestError::ThreadSpawn)?;
        handles.push(handle);
    }

    let mut first_error = None;
    for handle in handles {
        match handle.join() {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                first_error.get_or_insert(err);
            }
            Err(_) => {
                first_error.get_or_insert(TestError::Panic);
            }
        };
    }
    state.bar.finish_and_clear();

    let failed = state.failed.load(Ordering::Relaxed);
    let passed = state.passed.load(Ordering::Relaxed);
    let elapsed = state.elapsed.lock().unwrap().as_secs_f64();
    println!(
        "Finished {passed} passed, {failed} failed, {} files in {elapsed:.3}s CPU time",
        state.total
    );

    if let Some(err) = first_error {
        return Err(err);
    }
    if failed != 0 {
        return Err(TestError::Failures(failed));
    }
    Ok(())
}

fn worker(state: RunnerState, keep_going: bool) -> Result<(), TestError> {
    loop {
        if state.stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let Some(path) = state.next() else {
            return Ok(());
        };
        let start = Instant::now();
        let result = execute_file(&path);
        *state.elapsed.lock().unwrap() += start.elapsed();
        state.bar.inc(1);

        match result {
            Ok(count) => {
                state.passed.fetch_add(count, Ordering::Relaxed);
            }
            Err(err) => {
                state.failed.fetch_add(1, Ordering::Relaxed);
                if !keep_going {
                    state.stop.store(true, Ordering::Relaxed);
                    return Err(err);
                }
                eprintln!("{err}");
            }
        }
    }
}

#[derive(Clone)]
struct RunnerState {
    queue: Arc<Mutex<(usize, Vec<PathBuf>)>>,
    bar: ProgressBar,
    total: usize,
    passed: Arc<AtomicUsize>,
    failed: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    elapsed: Arc<Mutex<Duration>>,
}

impl RunnerState {
    fn new(files: Vec<PathBuf>) -> Self {
        let total = files.len();
        let bar = ProgressBar::with_draw_target(
            Some(total as u64),
            if io::stderr().is_terminal() {
                ProgressDrawTarget::stderr_with_hz(2)
            } else {
                ProgressDrawTarget::hidden()
            },
        );
        bar.set_style(
            ProgressStyle::with_template(
                "[{elapsed_precise}] {wide_bar} {pos}/{len} ({per_sec}, eta {eta})",
            )
            .unwrap()
            .progress_chars("=>-"),
        );
        Self {
            queue: Arc::new(Mutex::new((0, files))),
            bar,
            total,
            passed: Arc::new(AtomicUsize::new(0)),
            failed: Arc::new(AtomicUsize::new(0)),
            stop: Arc::new(AtomicBool::new(false)),
            elapsed: Arc::new(Mutex::new(Duration::ZERO)),
        }
    }

    fn next(&self) -> Option<PathBuf> {
        let (next, queue) = &mut *self.queue.lock().unwrap();
        let path = queue.get(*next).cloned();
        *next += usize::from(path.is_some());
        path
    }
}

fn execute_file(path: &Path) -> Result<usize, TestError> {
    let input = fs::read_to_string(path).map_err(|err| TestError::Read(path.to_path_buf(), err))?;
    let suite: BTreeMap<String, RawUnit> =
        serde_json::from_str(&input).map_err(|err| TestError::Json(path.to_path_buf(), err))?;
    let mut passed = 0;
    for (name, unit) in suite {
        passed += execute_unit(path, &name, unit)?;
    }
    Ok(passed)
}

fn execute_unit(path: &Path, name: &str, unit: RawUnit) -> Result<usize, TestError> {
    let mut passed = 0;
    let state =
        parse_state(&unit.pre).map_err(|err| TestError::Case(path.into(), name.into(), err))?;
    let block =
        parse_block(&unit.env).map_err(|err| TestError::Case(path.into(), name.into(), err))?;
    for (spec_name, posts) in &unit.post {
        let Some(spec) = spec_from_name(spec_name) else {
            continue;
        };
        for post in posts {
            let tx = parse_tx(&unit.transaction, &post.indexes)
                .map_err(|err| TestError::Case(path.into(), name.into(), err))?;
            let result = execute_spec(spec, block, state.clone(), &tx);
            let expected_exception = post.expect_exception.as_ref();
            match (expected_exception, result) {
                (Some(_), Err(_)) => {
                    passed += 1;
                }
                (Some(expected), Ok(_)) => {
                    return Err(TestError::Case(
                        path.into(),
                        name.into(),
                        CaseError::ExpectedException(expected.clone()),
                    ));
                }
                (None, Err(err)) => {
                    return Err(TestError::Case(path.into(), name.into(), CaseError::Evm(err)));
                }
                (None, Ok(outcome)) => {
                    if let Some(expected) = unit.out.as_ref()
                        && expected != &outcome.output
                    {
                        return Err(TestError::Case(
                            path.into(),
                            name.into(),
                            CaseError::Output { got: outcome.output, expected: expected.clone() },
                        ));
                    }
                    if outcome.logs_root != post.logs {
                        return Err(TestError::Case(
                            path.into(),
                            name.into(),
                            CaseError::Logs { got: outcome.logs_root, expected: post.logs },
                        ));
                    }
                    if outcome.state_root != post.hash {
                        return Err(TestError::Case(
                            path.into(),
                            name.into(),
                            CaseError::StateRoot { got: outcome.state_root, expected: post.hash },
                        ));
                    }
                    passed += 1;
                }
            }
        }
    }
    Ok(passed)
}

fn execute_spec(
    spec: SpecId,
    block: BlockEnv,
    database: InMemoryDB,
    tx: &Transaction,
) -> Result<SpecOutcome, EvmError> {
    macro_rules! run {
        ($spec:ident) => {{
            let mut evm = Evm::<EvmVersion<(), { SpecId::$spec as u8 }>>::with_database(
                block,
                TxRegistry::new(),
                database,
            );
            let result = evm.execute(tx)?;
            Ok(SpecOutcome {
                state_root: evm.state().state_root(),
                logs_root: logs_hash(evm.logs()),
                output: result.output,
            })
        }};
    }
    match spec {
        SpecId::FRONTIER => run!(FRONTIER),
        SpecId::FRONTIER_THAWING => run!(FRONTIER_THAWING),
        SpecId::HOMESTEAD => run!(HOMESTEAD),
        SpecId::DAO_FORK => run!(DAO_FORK),
        SpecId::TANGERINE => run!(TANGERINE),
        SpecId::SPURIOUS_DRAGON => run!(SPURIOUS_DRAGON),
        SpecId::BYZANTIUM => run!(BYZANTIUM),
        SpecId::CONSTANTINOPLE => run!(CONSTANTINOPLE),
        SpecId::PETERSBURG => run!(PETERSBURG),
        SpecId::ISTANBUL => run!(ISTANBUL),
        SpecId::MUIR_GLACIER => run!(MUIR_GLACIER),
        SpecId::BERLIN => run!(BERLIN),
        SpecId::LONDON => run!(LONDON),
        SpecId::ARROW_GLACIER => run!(ARROW_GLACIER),
        SpecId::GRAY_GLACIER => run!(GRAY_GLACIER),
        SpecId::MERGE => run!(MERGE),
        SpecId::SHANGHAI => run!(SHANGHAI),
        SpecId::CANCUN => run!(CANCUN),
        SpecId::PRAGUE => run!(PRAGUE),
        SpecId::OSAKA => run!(OSAKA),
        SpecId::AMSTERDAM => run!(AMSTERDAM),
        _ => unreachable!("unknown statetest spec: {spec:?}"),
    }
}

#[derive(Clone, Debug)]
struct SpecOutcome {
    state_root: B256,
    logs_root: B256,
    output: Bytes,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawUnit {
    env: RawEnv,
    pre: BTreeMap<String, RawAccount>,
    transaction: RawTransaction,
    post: BTreeMap<String, Vec<RawPost>>,
    #[serde(default)]
    out: Option<Bytes>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawEnv {
    current_coinbase: Address,
    current_difficulty: U256,
    current_gas_limit: U256,
    current_number: U256,
    current_timestamp: U256,
    #[serde(default)]
    current_base_fee: Option<U256>,
    #[serde(default)]
    current_random: Option<U256>,
}

#[derive(Debug, Deserialize)]
struct RawAccount {
    balance: U256,
    code: Bytes,
    nonce: U256,
    storage: BTreeMap<String, U256>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawTransaction {
    data: Vec<Bytes>,
    gas_limit: Vec<U256>,
    gas_price: U256,
    nonce: U256,
    #[serde(default)]
    sender: Option<Address>,
    #[serde(default)]
    secret_key: Option<B256>,
    to: Option<Value>,
    value: Vec<U256>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPost {
    hash: B256,
    indexes: Indexes,
    logs: B256,
    #[serde(default)]
    expect_exception: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Indexes {
    data: usize,
    gas: usize,
    value: usize,
}

fn parse_state(pre: &BTreeMap<String, RawAccount>) -> Result<InMemoryDB, CaseError> {
    let mut database = InMemoryDB::default();
    for (address, account) in pre {
        let address = parse_address(address)?;
        let nonce = account.nonce.try_into().map_err(|_| CaseError::Overflow("nonce"))?;
        let mut info = AccountInfo::default().with_code(Bytecode::new_legacy(account.code.clone()));
        info.nonce = nonce;
        info.balance = account.balance;
        database.insert_account_info(address, info);
        for (key, value) in &account.storage {
            database.insert_account_storage(address, parse_u256(key)?, *value);
        }
    }
    Ok(database)
}

fn parse_block(env: &RawEnv) -> Result<BlockEnv, CaseError> {
    Ok(BlockEnv {
        number: env.current_number,
        beneficiary: env.current_coinbase,
        timestamp: env.current_timestamp,
        gas_limit: env.current_gas_limit,
        basefee: env.current_base_fee.unwrap_or_default(),
        difficulty: env.current_difficulty,
        prevrandao: env.current_random.unwrap_or_default(),
        ..BlockEnv::default()
    })
}

fn parse_tx(raw: &RawTransaction, indexes: &Indexes) -> Result<Transaction, CaseError> {
    let caller = match raw.sender {
        Some(sender) => sender,
        None => secret_key_address(raw.secret_key.ok_or(CaseError::MissingSender)?)?,
    };
    let data = raw.data.get(indexes.data).ok_or(CaseError::BadIndex("data"))?.clone();
    let gas_limit = raw
        .gas_limit
        .get(indexes.gas)
        .ok_or(CaseError::BadIndex("gas"))?
        .try_into()
        .map_err(|_| CaseError::Overflow("gasLimit"))?;
    let value = *raw.value.get(indexes.value).ok_or(CaseError::BadIndex("value"))?;
    let nonce = raw.nonce.try_into().map_err(|_| CaseError::Overflow("nonce"))?;
    Ok(Transaction {
        caller,
        to: parse_to(raw.to.as_ref())?,
        nonce,
        gas_limit,
        gas_price: raw.gas_price,
        value,
        data,
    })
}

fn secret_key_address(secret_key: B256) -> Result<Address, CaseError> {
    let signing_key =
        SigningKey::from_slice(secret_key.as_slice()).map_err(|_| CaseError::InvalidSecretKey)?;
    let verifying_key = signing_key.verifying_key();
    let encoded = verifying_key.to_encoded_point(false);
    let hash = alloy_primitives::keccak256(&encoded.as_bytes()[1..]);
    Ok(Address::from_slice(&hash[12..]))
}

fn parse_to(value: Option<&Value>) -> Result<Option<Address>, CaseError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.is_empty() || s == "0x" => Ok(None),
        Some(Value::String(s)) => parse_address(s).map(Some),
        Some(_) => Err(CaseError::InvalidTo),
    }
}

fn parse_address(value: &str) -> Result<Address, CaseError> {
    value.parse().map_err(|_| CaseError::Hex(value.to_string()))
}

fn parse_u256(value: &str) -> Result<U256, CaseError> {
    value.parse().map_err(|_| CaseError::Hex(value.to_string()))
}

fn spec_from_name(name: &str) -> Option<SpecId> {
    match name {
        "Frontier" => Some(SpecId::FRONTIER),
        "Homestead" => Some(SpecId::HOMESTEAD),
        "EIP150" | "TangerineWhistle" => Some(SpecId::TANGERINE),
        "EIP158" | "SpuriousDragon" => Some(SpecId::SPURIOUS_DRAGON),
        "Byzantium" => Some(SpecId::BYZANTIUM),
        "Constantinople" => Some(SpecId::CONSTANTINOPLE),
        "ConstantinopleFix" | "Petersburg" => Some(SpecId::PETERSBURG),
        "Istanbul" => Some(SpecId::ISTANBUL),
        "Berlin" => Some(SpecId::BERLIN),
        "London" => Some(SpecId::LONDON),
        "Paris" | "Merge" => Some(SpecId::MERGE),
        "Shanghai" => Some(SpecId::SHANGHAI),
        "Cancun" => Some(SpecId::CANCUN),
        "Prague" => Some(SpecId::PRAGUE),
        "Osaka" => Some(SpecId::OSAKA),
        "Amsterdam" => Some(SpecId::AMSTERDAM),
        _ => None,
    }
}

#[derive(Debug, Error)]
enum TestError {
    #[error("invalid path: {path}", path = .0.display())]
    InvalidPath(PathBuf),
    #[error("no JSON tests found")]
    NoJsonFiles,
    #[error("walk error under {path}: {source}", path = .0.display(), source = .1)]
    Walk(PathBuf, walkdir::Error),
    #[error("failed to read {path}: {source}", path = .0.display(), source = .1)]
    Read(PathBuf, io::Error),
    #[error("failed to parse {path}: {source}", path = .0.display(), source = .1)]
    Json(PathBuf, serde_json::Error),
    #[error("{path}:{name}: {source}", path = .0.display(), name = .1, source = .2)]
    Case(PathBuf, String, CaseError),
    #[error("failed to spawn worker: {0}")]
    ThreadSpawn(io::Error),
    #[error("worker panicked")]
    Panic,
    #[error("{0} test files failed")]
    Failures(usize),
}

#[derive(Debug, Error)]
enum CaseError {
    #[error("hex decode error: {0}")]
    Hex(String),
    #[error("value overflows {0}")]
    Overflow(&'static str),
    #[error("missing transaction sender")]
    MissingSender,
    #[error("invalid transaction secret key")]
    InvalidSecretKey,
    #[error("bad transaction index: {0}")]
    BadIndex(&'static str),
    #[error("invalid transaction to field")]
    InvalidTo,
    #[error("expected exception did not occur: {0}")]
    ExpectedException(String),
    #[error(transparent)]
    Evm(#[from] EvmError),
    #[error("output mismatch: got {got}, expected {expected}")]
    Output { got: Bytes, expected: Bytes },
    #[error("logs root mismatch: got {got}, expected {expected}")]
    Logs { got: B256, expected: B256 },
    #[error("state root mismatch: got {got}, expected {expected}")]
    StateRoot { got: B256, expected: B256 },
}
