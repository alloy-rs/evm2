//! Ethereum state test execution for evm2.

use alloy_primitives::{Address, B256, Bytes, U256};
use evm2::{
    Evm, EvmVersion,
    bytecode::Bytecode,
    env::BlockEnv,
    evm::{
        AccountInfo as EvmAccountInfo, InMemoryDB, logs_hash,
        transaction::{Error as EvmError, Transaction},
    },
    interpreter::SpecId,
    registry::TxRegistry,
};
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use k256::ecdsa::SigningKey;
use serde::{Deserialize, Deserializer, de};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, IsTerminal},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use thiserror::Error;
use walkdir::WalkDir;

/// Default local state-test root used by the CLI and nextest harness.
pub const DEFAULT_STATE_TEST_ROOT: &str =
    "/home/doni/github/danipopes/revm/test-fixtures/legacytests/Constantinople/GeneralStateTests";

/// Top-level state test suite.
#[derive(Debug, Deserialize)]
pub struct TestSuite(pub BTreeMap<String, TestUnit>);

/// A single named state test.
#[derive(Debug, Deserialize)]
pub struct TestUnit {
    /// Optional ethereum/tests metadata.
    #[serde(default, rename = "_info")]
    pub info: Option<serde_json::Value>,
    /// Block environment.
    pub env: Env,
    /// Pre-state accounts.
    pub pre: BTreeMap<Address, AccountInfo>,
    /// Expected post-state roots by fork.
    pub post: BTreeMap<SpecName, Vec<Test>>,
    /// Transaction parts indexed by each post entry.
    pub transaction: TransactionParts,
    /// Expected output.
    #[serde(default)]
    pub out: Option<Bytes>,
}

/// State test block environment.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Env {
    /// Chain ID for transaction execution.
    #[serde(rename = "currentChainID")]
    pub current_chain_id: Option<U256>,
    /// Block beneficiary.
    pub current_coinbase: Address,
    /// Pre-merge difficulty.
    #[serde(default)]
    pub current_difficulty: U256,
    /// Block gas limit.
    pub current_gas_limit: U256,
    /// Block number.
    pub current_number: U256,
    /// Block timestamp.
    pub current_timestamp: U256,
    /// EIP-1559 base fee.
    pub current_base_fee: Option<U256>,
    /// Previous block hash.
    pub previous_hash: Option<B256>,
    /// Post-merge randomness.
    pub current_random: Option<B256>,
    /// EIP-4788 beacon root.
    pub current_beacon_root: Option<B256>,
    /// Withdrawals root.
    pub current_withdrawals_root: Option<B256>,
    /// EIP-4844 excess blob gas.
    pub current_excess_blob_gas: Option<U256>,
    /// Beacon slot number.
    pub slot_number: Option<U256>,
}

/// State test account entry.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountInfo {
    /// Account balance.
    pub balance: U256,
    /// Account code.
    pub code: Bytes,
    /// Account nonce.
    #[serde(deserialize_with = "deserialize_str_as_u64")]
    pub nonce: u64,
    /// Account storage.
    pub storage: BTreeMap<U256, U256>,
}

/// State test transaction parts.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransactionParts {
    /// Explicit transaction type.
    #[serde(rename = "type")]
    pub tx_type: Option<u8>,
    /// Input data variants.
    pub data: Vec<Bytes>,
    /// Gas limit variants.
    pub gas_limit: Vec<U256>,
    /// Legacy gas price.
    pub gas_price: Option<U256>,
    /// Transaction nonce.
    pub nonce: U256,
    /// Sender private key.
    #[serde(default)]
    pub secret_key: B256,
    /// Explicit sender.
    #[serde(default)]
    pub sender: Option<Address>,
    /// Transaction recipient, or none for create.
    #[serde(default, deserialize_with = "deserialize_maybe_empty")]
    pub to: Option<Address>,
    /// Value variants.
    pub value: Vec<U256>,
    /// EIP-1559 max fee.
    pub max_fee_per_gas: Option<U256>,
    /// EIP-1559 priority fee.
    pub max_priority_fee_per_gas: Option<U256>,
    /// EIP-7873 initcodes.
    pub initcodes: Option<Vec<Bytes>>,
    /// EIP-2930 access list variants.
    #[serde(default)]
    pub access_lists: Vec<Option<Vec<AccessListItem>>>,
    /// EIP-7702 authorizations.
    pub authorization_list: Option<Vec<TestAuthorization>>,
    /// EIP-4844 blob hashes.
    #[serde(default)]
    pub blob_versioned_hashes: Vec<B256>,
    /// EIP-4844 max fee per blob gas.
    pub max_fee_per_blob_gas: Option<U256>,
}

/// Access list entry.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccessListItem {
    /// Accessed account.
    pub address: Address,
    /// Accessed storage keys.
    pub storage_keys: Vec<B256>,
}

/// EIP-7702 authorization entry.
#[derive(Clone, Debug)]
pub struct TestAuthorization {
    /// Raw authorization JSON.
    pub value: serde_json::Value,
}

impl<'de> Deserialize<'de> for TestAuthorization {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let mut value = serde_json::Value::deserialize(deserializer)?;
        if let Some(object) = value.as_object_mut()
            && object.contains_key("v")
            && object.contains_key("yParity")
        {
            object.remove("v");
        }
        Ok(Self { value })
    }
}

/// State test fork name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Hash)]
pub enum SpecName {
    /// Frontier.
    Frontier,
    /// Frontier to Homestead transition.
    FrontierToHomesteadAt5,
    /// Homestead.
    Homestead,
    /// Homestead to DAO transition.
    HomesteadToDaoAt5,
    /// Homestead to EIP-150 transition.
    HomesteadToEIP150At5,
    /// EIP-150.
    EIP150,
    /// EIP-158.
    EIP158,
    /// EIP-158 to Byzantium transition.
    EIP158ToByzantiumAt5,
    /// Byzantium.
    Byzantium,
    /// Skipped Constantinople transition.
    ByzantiumToConstantinopleAt5,
    /// Byzantium to Petersburg transition.
    ByzantiumToConstantinopleFixAt5,
    /// Skipped Constantinople.
    Constantinople,
    /// Petersburg.
    ConstantinopleFix,
    /// Istanbul.
    Istanbul,
    /// Berlin.
    Berlin,
    /// Berlin to London transition.
    BerlinToLondonAt5,
    /// London.
    London,
    /// Paris.
    Paris,
    /// Merge.
    Merge,
    /// Shanghai.
    Shanghai,
    /// Cancun.
    Cancun,
    /// Prague.
    Prague,
    /// Osaka.
    Osaka,
    /// Amsterdam.
    Amsterdam,
    /// Unknown fork.
    #[serde(other)]
    Unknown,
}

impl SpecName {
    #[inline]
    const fn to_spec_id(self) -> Option<SpecId> {
        match self {
            Self::Frontier => Some(SpecId::FRONTIER),
            Self::FrontierToHomesteadAt5 | Self::Homestead => Some(SpecId::HOMESTEAD),
            Self::HomesteadToDaoAt5 | Self::HomesteadToEIP150At5 | Self::EIP150 => {
                Some(SpecId::TANGERINE)
            }
            Self::EIP158 => Some(SpecId::SPURIOUS_DRAGON),
            Self::EIP158ToByzantiumAt5 | Self::Byzantium => Some(SpecId::BYZANTIUM),
            Self::ByzantiumToConstantinopleFixAt5 | Self::ConstantinopleFix => {
                Some(SpecId::PETERSBURG)
            }
            Self::Istanbul => Some(SpecId::ISTANBUL),
            Self::Berlin => Some(SpecId::BERLIN),
            Self::BerlinToLondonAt5 | Self::London => Some(SpecId::LONDON),
            Self::Paris | Self::Merge => Some(SpecId::MERGE),
            Self::Shanghai => Some(SpecId::SHANGHAI),
            Self::Cancun => Some(SpecId::CANCUN),
            Self::Prague => Some(SpecId::PRAGUE),
            Self::Osaka => Some(SpecId::OSAKA),
            Self::Amsterdam => Some(SpecId::AMSTERDAM),
            Self::ByzantiumToConstantinopleAt5 | Self::Constantinople | Self::Unknown => None,
        }
    }
}

/// State test post entry.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Test {
    /// Expected exception.
    pub expect_exception: Option<String>,
    /// Transaction part indexes.
    pub indexes: TxPartIndices,
    /// Expected post-state root.
    pub hash: B256,
    /// Expected post-state account map.
    #[serde(default)]
    pub post_state: BTreeMap<Address, AccountInfo>,
    /// Expected logs root.
    pub logs: B256,
    /// Optional full expected state.
    #[serde(default)]
    pub state: BTreeMap<Address, AccountInfo>,
    /// Optional encoded transaction bytes.
    pub txbytes: Option<Bytes>,
}

/// Transaction part indexes.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TxPartIndices {
    /// Data index.
    pub data: usize,
    /// Gas index.
    pub gas: usize,
    /// Value index.
    pub value: usize,
}

/// Per-spec execution outcome.
#[derive(Clone, Debug)]
pub struct SpecOutcome {
    /// Computed state root.
    pub state_root: B256,
    /// Computed logs root.
    pub logs_root: B256,
    /// Transaction output.
    pub output: Bytes,
}

/// Finds all JSON state test files under `paths`.
pub fn find_json_tests(paths: &[PathBuf]) -> Result<Vec<PathBuf>, TestError> {
    let mut files = Vec::new();
    for path in paths {
        if path.is_file() {
            if is_json_test(path) {
                files.push(path.clone());
            }
            continue;
        }
        if !path.exists() {
            return Err(TestError::InvalidPath(path.clone()));
        }
        for entry in WalkDir::new(path).follow_links(false) {
            let entry = entry.map_err(|err| TestError::Walk(path.clone(), err))?;
            if entry.file_type().is_file() && is_json_test(entry.path()) {
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

/// Runs a list of test files with an internal thread pool.
pub fn run(files: Vec<PathBuf>, jobs: usize, keep_going: bool) -> Result<(), TestError> {
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

/// Executes a single state test JSON file.
pub fn execute_file(path: &Path) -> Result<usize, TestError> {
    if skip_test(path) {
        return Ok(0);
    }
    let input = fs::read_to_string(path).map_err(|err| TestError::Read(path.to_path_buf(), err))?;
    execute_str(path, &input)
}

/// Executes a single state test JSON file from an already-loaded string.
pub fn execute_str(path: &Path, input: &str) -> Result<usize, TestError> {
    if skip_test(path) {
        return Ok(0);
    }
    let suite: TestSuite =
        serde_json::from_str(input).map_err(|err| TestError::Json(path.to_path_buf(), err))?;
    let mut passed = 0;
    for (name, unit) in suite.0 {
        passed += execute_unit(path, &name, unit)?;
    }
    Ok(passed)
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

fn execute_unit(path: &Path, name: &str, unit: TestUnit) -> Result<usize, TestError> {
    let mut passed = 0;
    let state =
        parse_state(&unit.pre).map_err(|err| TestError::Case(path.into(), name.into(), err))?;
    let block =
        parse_block(&unit.env).map_err(|err| TestError::Case(path.into(), name.into(), err))?;
    for (spec_name, posts) in &unit.post {
        let Some(spec) = spec_name.to_spec_id() else {
            continue;
        };
        for post in posts {
            let tx = match build_tx(&unit.transaction, &post.indexes) {
                Ok(tx) => tx,
                Err(_) if post.expect_exception.is_some() => {
                    passed += 1;
                    continue;
                }
                Err(err) => return Err(TestError::Case(path.into(), name.into(), err)),
            };
            let result = execute_spec(spec, block, state.clone(), &tx);
            match (&post.expect_exception, result) {
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
                    validate_outcome(path, name, &unit, post, outcome)?;
                    passed += 1;
                }
            }
        }
    }
    Ok(passed)
}

fn validate_outcome(
    path: &Path,
    name: &str,
    unit: &TestUnit,
    post: &Test,
    outcome: SpecOutcome,
) -> Result<(), TestError> {
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
    Ok(())
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

fn parse_state(pre: &BTreeMap<Address, AccountInfo>) -> Result<InMemoryDB, CaseError> {
    let mut database = InMemoryDB::default();
    for (address, account) in pre {
        let mut info =
            EvmAccountInfo::default().with_code(Bytecode::new_legacy(account.code.clone()));
        info.nonce = account.nonce;
        info.balance = account.balance;
        database.insert_account_info(*address, info);
        for (key, value) in &account.storage {
            database.insert_account_storage(*address, *key, *value);
        }
    }
    Ok(database)
}

fn parse_block(env: &Env) -> Result<BlockEnv, CaseError> {
    Ok(BlockEnv {
        number: env.current_number,
        beneficiary: env.current_coinbase,
        timestamp: env.current_timestamp,
        gas_limit: env.current_gas_limit,
        basefee: env.current_base_fee.unwrap_or_default(),
        difficulty: env.current_difficulty,
        prevrandao: env
            .current_random
            .map_or(U256::ZERO, |random| U256::from_be_slice(random.as_slice())),
        slot_num: env.slot_number.unwrap_or_default(),
        ..BlockEnv::default()
    })
}

fn build_tx(raw: &TransactionParts, indexes: &TxPartIndices) -> Result<Transaction, CaseError> {
    let caller = match raw.sender {
        Some(sender) => sender,
        None => recover_address(raw.secret_key.as_slice())
            .ok_or(CaseError::UnknownPrivateKey(raw.secret_key))?,
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
        to: raw.to,
        nonce,
        gas_limit,
        gas_price: raw.gas_price.or(raw.max_fee_per_gas).unwrap_or_default(),
        value,
        data,
    })
}

fn recover_address(private_key: &[u8]) -> Option<Address> {
    let key = SigningKey::from_slice(private_key).ok()?;
    let public_key = key.verifying_key().to_encoded_point(false);
    Some(Address::from_raw_public_key(&public_key.as_bytes()[1..]))
}

fn deserialize_str_as_u64<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    let string = String::deserialize(deserializer)?;
    if let Some(stripped) = string.strip_prefix("0x") {
        u64::from_str_radix(stripped, 16)
    } else {
        string.parse()
    }
    .map_err(de::Error::custom)
}

fn deserialize_maybe_empty<'de, D>(deserializer: D) -> Result<Option<Address>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::String(string) if string.is_empty() || string == "0x" => Ok(None),
        serde_json::Value::String(string) => string.parse().map(Some).map_err(de::Error::custom),
        _ => Err(de::Error::custom("invalid transaction to field")),
    }
}

fn is_json_test(path: &Path) -> bool {
    path.file_name().is_none_or(|name| name != "index.json")
        && path.extension().is_some_and(|ext| ext == "json")
}

fn skip_test(path: &Path) -> bool {
    let path_str = path.to_str().unwrap_or_default();
    if path_str.contains("paris/eip7610_create_collision") {
        return true;
    }

    let name = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
    matches!(
        name,
        "RevertInCreateInInit_Paris.json"
            | "RevertInCreateInInit.json"
            | "dynamicAccountOverwriteEmpty.json"
            | "dynamicAccountOverwriteEmpty_Paris.json"
            | "RevertInCreateInInitCreate2Paris.json"
            | "create2collisionStorage.json"
            | "RevertInCreateInInitCreate2.json"
            | "create2collisionStorageParis.json"
            | "InitCollision.json"
            | "InitCollisionParis.json"
            | "test_init_collision_create_opcode.json"
            | "ValueOverflow.json"
            | "ValueOverflowParis.json"
            | "Call50000_sha256.json"
            | "static_Call50000_sha256.json"
            | "loopMul.json"
            | "CALLBlake2f_MaxRounds.json"
    )
}

/// State test runner error.
#[derive(Debug, Error)]
pub enum TestError {
    /// Invalid test path.
    #[error("invalid path: {path}", path = .0.display())]
    InvalidPath(PathBuf),
    /// No JSON tests were found.
    #[error("no JSON tests found")]
    NoJsonFiles,
    /// Directory traversal failed.
    #[error("walk error under {path}: {source}", path = .0.display(), source = .1)]
    Walk(PathBuf, walkdir::Error),
    /// File read failed.
    #[error("failed to read {path}: {source}", path = .0.display(), source = .1)]
    Read(PathBuf, io::Error),
    /// JSON decoding failed.
    #[error("failed to parse {path}: {source}", path = .0.display(), source = .1)]
    Json(PathBuf, serde_json::Error),
    /// A specific case failed.
    #[error("{path}:{name}: {source}", path = .0.display(), name = .1, source = .2)]
    Case(PathBuf, String, CaseError),
    /// Worker thread spawn failed.
    #[error("failed to spawn worker: {0}")]
    ThreadSpawn(io::Error),
    /// Worker thread panicked.
    #[error("worker panicked")]
    Panic,
    /// One or more files failed.
    #[error("{0} test files failed")]
    Failures(usize),
}

/// Per-case state test error.
#[derive(Debug, Error)]
pub enum CaseError {
    /// Numeric value overflowed the target type.
    #[error("value overflows {0}")]
    Overflow(&'static str),
    /// Sender could not be recovered.
    #[error("unknown private key: {0:?}")]
    UnknownPrivateKey(B256),
    /// Transaction part index was invalid.
    #[error("bad transaction index: {0}")]
    BadIndex(&'static str),
    /// An expected exception did not occur.
    #[error("expected exception did not occur: {0}")]
    ExpectedException(String),
    /// EVM execution failed.
    #[error(transparent)]
    Evm(#[from] EvmError),
    /// Output mismatch.
    #[error("output mismatch: got {got}, expected {expected}")]
    Output {
        /// Actual output.
        got: Bytes,
        /// Expected output.
        expected: Bytes,
    },
    /// Logs root mismatch.
    #[error("logs root mismatch: got {got}, expected {expected}")]
    Logs {
        /// Actual logs root.
        got: B256,
        /// Expected logs root.
        expected: B256,
    },
    /// State root mismatch.
    #[error("state root mismatch: got {got}, expected {expected}")]
    StateRoot {
        /// Actual state root.
        got: B256,
        /// Expected state root.
        expected: B256,
    },
}
