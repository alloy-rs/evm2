//! Differential fuzzing proof-of-concept for evm2 against revm.
//!
//! This is intentionally a structured, deterministic generator rather than a
//! property-test harness. Run it with, for example:
//!
//! `cargo run -p evm2-cli -- fuzzer --seed 1 --cases 1000`
//!
//! or by duration:
//!
//! `cargo run -p evm2-cli -- fuzzer --seed 1 --duration 30s`
//!
//! Re-run saved cases with:
//!
//! `cargo run -p evm2-cli -- fuzzer replay crates/cli/fuzzer/corpus/failures/case-....json`
//!
//! or run a directory of cases with:
//!
//! `cargo run -p evm2-cli -- fuzzer corpus crates/cli/fuzzer/corpus/failures`

#![allow(missing_docs)]

mod backend;
mod case;
mod cli;
mod coverage;
mod io;
mod minimize;
mod normalize;
mod precompile;
mod program;
mod rng;

pub use evm2::SpecId;

#[cfg(feature = "jit")]
pub use self::backend::JitEvm2Backend;
pub use self::{
    backend::{Evm2Backend, EvmBackend, RevmBackend},
    case::EvmCase,
    coverage::Coverage,
    io::{case_paths, read_case, write_minimized_case},
    minimize::{differs, minimize_case},
    normalize::Outcome,
};
use self::{
    case::{CALLER, CaseAccount, CaseBlock, CaseTx, TARGET, TxKindCase},
    cli::Command,
    io::write_failure_case,
    normalize::OutcomeKind,
    rng::Gen,
};
use alloy_eips::eip2930::AccessList;
use alloy_primitives::{Bytes, U256};
use std::{
    collections::BTreeMap,
    fmt,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::Instant,
};

pub use cli::Options;

pub fn run(opts: Options) -> Result<(), String> {
    let backends: [&dyn EvmBackend; 2] = [&RevmBackend, &Evm2Backend];
    match opts.command.clone().unwrap_or(Command::Generate) {
        Command::Generate => run_generated(&opts)?,
        Command::Replay { path } => {
            let case = read_case(&path)?;
            let mut coverage = Coverage::default();
            coverage.record_case(&case);
            let outcome = compare_case(&backends, &case, CaseContext::Path(&path))?;
            coverage.record_outcome(&outcome);
            println!("ok: replayed {}", path.display());
            coverage.print();
        }
        Command::Corpus { path } => {
            let mut paths = case_paths(&path)?;
            paths.sort();
            let mut coverage = Coverage::default();
            for path in &paths {
                let case = read_case(path)?;
                coverage.record_case(&case);
                let outcome = compare_case(&backends, &case, CaseContext::Path(path))?;
                coverage.record_outcome(&outcome);
            }
            println!("ok: replayed {} corpus cases", paths.len());
            coverage.print();
        }
        Command::Minimize { path } => {
            let case = read_case(&path)?;
            if !differs(&backends, &case) {
                return Err(format!("{} does not reproduce a mismatch", path.display()));
            }
            let minimized = minimize_case(&backends, case);
            let path = write_minimized_case(&minimized)?;
            println!("ok: wrote minimized case to {}", path.display());
        }
    }
    Ok(())
}

fn run_generated(opts: &Options) -> Result<(), String> {
    let seed = opts.seed.unwrap_or_else(rand::random);
    println!("seed: {seed}");
    let workers = resolve_threads(opts.threads);
    if workers != 1 {
        println!("workers: {workers}");
    }

    let started = Instant::now();
    let cases = opts.cases.or_else(|| opts.duration.is_none().then_some(256));
    let next_case = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));

    let mut coverage = Coverage::default();
    let mut executed = 0;
    thread::scope(|scope| {
        let mut handles = Vec::with_capacity(workers);
        for _ in 0..workers {
            let next_case = Arc::clone(&next_case);
            let stop = Arc::clone(&stop);
            handles.push(scope.spawn(move || {
                let backends: [&dyn EvmBackend; 2] = [&RevmBackend, &Evm2Backend];
                let mut coverage = Coverage::default();
                let mut executed = 0;
                while !stop.load(Ordering::Relaxed)
                    && opts.duration.is_none_or(|duration| started.elapsed() < duration)
                {
                    let case_index = next_case.fetch_add(1, Ordering::Relaxed);
                    if cases.is_some_and(|cases| case_index >= cases) {
                        break;
                    }

                    let case = generate_case(seed, case_index);
                    let context = CaseContext::Generated { seed, case_index };
                    coverage.record_case(&case);
                    let outcome = compare_case(&backends, &case, context).inspect_err(|_| {
                        stop.store(true, Ordering::Relaxed);
                    })?;
                    coverage.record_outcome(&outcome);
                    executed += 1;
                }
                Ok::<_, String>((executed, coverage))
            }));
        }

        for handle in handles {
            let (worker_executed, worker_coverage) =
                handle.join().map_err(|_| "fuzzer worker thread panicked".to_string())??;
            executed += worker_executed;
            coverage.merge(worker_coverage);
        }
        Ok::<_, String>(())
    })?;

    println!("ok: {executed} structured differential cases");
    coverage.print();
    Ok(())
}

fn resolve_threads(threads: usize) -> usize {
    if threads == 0 {
        thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get)
    } else {
        threads
    }
}

/// Hardfork used by the byte-only cargo-fuzz harness.
pub const BYTECODE_FUZZ_SPEC: SpecId = SpecId::OSAKA;

/// Builds a simple execution case whose target account code is exactly `bytecode`.
pub fn bytecode_case(bytecode: &[u8]) -> EvmCase {
    bytecode_case_with_spec(BYTECODE_FUZZ_SPEC, bytecode)
}

/// Builds a simple execution case for `spec` whose target account code is exactly `bytecode`.
pub fn bytecode_case_with_spec(spec: SpecId, bytecode: &[u8]) -> EvmCase {
    EvmCase {
        spec,
        block: CaseBlock {
            number: U256::from(23_935_694),
            timestamp: U256::ZERO,
            gas_limit: 30_000_000,
            basefee: 0,
        },
        tx: CaseTx {
            kind: TxKindCase::Legacy,
            caller: CALLER,
            target: TARGET,
            creates: false,
            gas_limit: 10_000_000,
            gas_price: 1,
            value: U256::ZERO,
            input: Bytes::new(),
            nonce: 0,
            access_list: AccessList::default(),
            blob_hashes: Vec::new(),
            authorization_list: None,
        },
        extra_txs: Vec::new(),
        features: vec!["bytes_only".to_string()],
        accounts: vec![
            CaseAccount {
                address: CALLER,
                balance: U256::from_limbs([0, 0, 1, 0]),
                nonce: 0,
                code: Bytes::new(),
                storage: BTreeMap::new(),
            },
            CaseAccount {
                address: TARGET,
                balance: U256::from(1_000_000),
                nonce: 1,
                code: Bytes::copy_from_slice(bytecode),
                storage: BTreeMap::new(),
            },
        ],
    }
}

/// Returns whether bytecode is eligible for JIT-vs-interpreter fuzzing.
#[cfg(feature = "jit")]
pub fn jit_bytecode_supported(spec: SpecId, bytecode: &[u8]) -> bool {
    use evm2_jit_runtime::{OpcodesIter, op_info_map, runtime::RuntimeTuning};

    if !RuntimeTuning::default().should_compile(bytecode) {
        return false;
    }

    let info = op_info_map(spec);
    OpcodesIter::new(bytecode, spec).all(|opcode| {
        let opcode_info = info[opcode.opcode as usize];
        if opcode_info.is_unknown() || opcode_info.is_disabled() {
            return false;
        }

        let immediate_len = jit_immediate_len(opcode.opcode);
        immediate_len == 0 || opcode.immediate.is_some_and(|imm| imm.len() == immediate_len)
    })
}

#[cfg(feature = "jit")]
const fn jit_immediate_len(opcode: u8) -> usize {
    if opcode >= evm2::interpreter::op::PUSH1 && opcode <= evm2::interpreter::op::PUSH32 {
        (opcode - evm2::interpreter::op::PUSH1 + 1) as usize
    } else {
        match opcode {
            evm2::interpreter::op::DUPN
            | evm2::interpreter::op::SWAPN
            | evm2::interpreter::op::EXCHANGE => 1,
            _ => 0,
        }
    }
}

/// Builds a bounded structured execution case from `arbitrary` input bytes.
pub fn arbitrary_case(data: &[u8]) -> arbitrary::Result<EvmCase> {
    EvmCase::arbitrary_from_bytes(data)
}

/// Builds the same deterministic structured case used by the fuzzer binary.
pub fn generate_case(seed: u64, case_index: u64) -> EvmCase {
    let mut rng = Gen::new(seed ^ case_index.wrapping_mul(0x9e37_79b9_7f4a_7c15));
    EvmCase::generate(&mut rng)
}

/// Builds a deterministic case biased toward transaction validation boundaries.
pub fn generate_validation_case(seed: u64, case_index: u64) -> EvmCase {
    let mut rng = Gen::new(seed ^ case_index.wrapping_mul(0xd1b5_4a32_d192_ed03));
    let spec = match rng.range(14) {
        0 => SpecId::FRONTIER,
        1 => SpecId::HOMESTEAD,
        2 => SpecId::TANGERINE,
        3 => SpecId::SPURIOUS_DRAGON,
        4 => SpecId::BYZANTIUM,
        5 => SpecId::PETERSBURG,
        6 => SpecId::ISTANBUL,
        7 => SpecId::BERLIN,
        8 => SpecId::LONDON,
        9 => SpecId::MERGE,
        10 => SpecId::SHANGHAI,
        11 => SpecId::CANCUN,
        12 => SpecId::PRAGUE,
        _ => SpecId::OSAKA,
    };
    let gas_limit =
        rng.pick(&[0, 20_999, 21_000, 21_001, 29_999, 30_000, 53_000, 60_000, 100_000, 1_000_000]);
    let gas_price = u128::from(rng.pick(&[0_u16, 1, 2, 7, 255]));
    let value = rng.pick(&[U256::ZERO, U256::ONE, U256::from(1_000), U256::from(1_000_000)]);
    let max_cost = U256::from(gas_limit) * U256::from(gas_price);
    let balance = match rng.range(9) {
        0 => U256::ZERO,
        1 => U256::ONE,
        2 => value,
        3 => max_cost,
        4 => max_cost.saturating_add(value),
        5 => max_cost.saturating_add(value).saturating_sub(U256::ONE),
        6 => U256::from(21_000),
        7 => U256::from_limbs([0, 0, 1, 0]),
        _ => U256::MAX,
    };
    let creates = rng.one_in(4);
    let kind = validation_tx_kind(&mut rng, spec, creates);
    let input_len = rng.pick(&[0, 1, 4, 16, 31, 32, 33, 64, 128]);
    let block_basefee = if spec.enables(SpecId::LONDON) { rng.pick(&[0, 1, 2, 7]) } else { 0 };
    let tx = CaseTx {
        kind,
        caller: CALLER,
        target: validation_target(&mut rng),
        creates: creates && kind.supports_create(),
        gas_limit,
        gas_price,
        value,
        input: validation_input(&mut rng, input_len),
        nonce: rng.pick(&[0, 1, 2, u64::MAX]),
        access_list: validation_access_list(&mut rng),
        blob_hashes: validation_blob_hashes(&mut rng),
        authorization_list: (kind == TxKindCase::Eip7702 && rng.one_in(4)).then(Vec::new),
    };
    EvmCase {
        spec,
        block: CaseBlock {
            number: rng.small_word(1_000_000),
            timestamp: rng.small_word(2_000_000_000),
            gas_limit: rng.pick(&[0, 20_999, 21_000, 30_000, 60_000, 100_000, 1_000_000]),
            basefee: block_basefee,
        },
        tx,
        extra_txs: Vec::new(),
        features: vec!["validation".to_string()],
        accounts: vec![
            CaseAccount {
                address: CALLER,
                balance,
                nonce: rng.pick(&[0, 1, 2, u64::MAX]),
                code: Bytes::new(),
                storage: BTreeMap::new(),
            },
            CaseAccount {
                address: TARGET,
                balance: U256::from(1_000_000),
                nonce: 1,
                code: Bytes::from_static(&[evm2::interpreter::op::STOP]),
                storage: BTreeMap::new(),
            },
        ],
    }
}

fn validation_tx_kind(rng: &mut Gen, spec: SpecId, creates: bool) -> TxKindCase {
    let choices = [
        TxKindCase::Legacy,
        TxKindCase::Eip2930,
        TxKindCase::Eip1559,
        TxKindCase::Eip4844,
        TxKindCase::Eip7702,
    ];
    let kind = rng.pick(&choices);
    if creates && !kind.supports_create() {
        return TxKindCase::Legacy;
    }
    if rng.one_in(2) || kind.is_enabled(spec) { kind } else { TxKindCase::Legacy }
}

fn validation_target(rng: &mut Gen) -> alloy_primitives::Address {
    rng.pick(&[
        TARGET,
        CALLER,
        alloy_primitives::Address::ZERO,
        alloy_primitives::Address::with_last_byte(1),
        alloy_primitives::Address::with_last_byte(9),
        alloy_primitives::Address::with_last_byte(0x77),
        alloy_primitives::Address::with_last_byte(6),
    ])
}

fn validation_input(rng: &mut Gen, len: usize) -> Bytes {
    match rng.range(5) {
        0 => Bytes::new(),
        1 => Bytes::from_static(&[0]),
        2 => Bytes::from_static(&[evm2::interpreter::op::STOP]),
        3 => Bytes::from_static(&[
            evm2::interpreter::op::PUSH1,
            0,
            evm2::interpreter::op::PUSH1,
            0,
            evm2::interpreter::op::RETURN,
        ]),
        _ => rng.bytes(len).into(),
    }
}

fn validation_access_list(rng: &mut Gen) -> AccessList {
    let mut items = Vec::new();
    for _ in 0..rng.range_inclusive(0, 4) {
        let mut storage_keys = Vec::new();
        for _ in 0..rng.range_inclusive(0, 4) {
            storage_keys.push(alloy_primitives::B256::from(rng.biased_word().to_be_bytes::<32>()));
        }
        items.push(alloy_eips::eip2930::AccessListItem {
            address: validation_target(rng),
            storage_keys,
        });
    }
    AccessList(items)
}

fn validation_blob_hashes(rng: &mut Gen) -> Vec<alloy_primitives::B256> {
    let len = rng.range_inclusive(0, 4);
    (0..len)
        .map(|_| {
            let mut hash = rng.bytes(32);
            hash[0] = 0x01;
            alloy_primitives::B256::from_slice(&hash)
        })
        .collect()
}

/// Identifies where a differential comparison case came from.
#[derive(Clone, Copy, Debug)]
pub enum CaseContext<'a> {
    Generated { seed: u64, case_index: u64 },
    Path(&'a Path),
    Bytes,
}

impl fmt::Display for CaseContext<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Generated { seed, case_index } => {
                write!(f, "generated case {case_index}, seed {seed}")
            }
            Self::Path(path) => write!(f, "{}", path.display()),
            Self::Bytes => f.write_str("cargo-fuzz input"),
        }
    }
}

/// Runs `case` against revm first, then compares evm2 against that baseline.
pub fn compare_case(
    backends: &[&dyn EvmBackend],
    case: &EvmCase,
    context: CaseContext<'_>,
) -> Result<Outcome, String> {
    debug_assert!(backends.len() >= 2);
    let baseline = backends[0].run(case);
    for backend in &backends[1..] {
        let got = backend.run(case);
        if got != baseline {
            if let CaseContext::Generated { seed, case_index } = context {
                let path = write_failure_case(seed, case_index, case)?;
                eprintln!("wrote failing case to {}", path.display());
                let minimized = minimize_case(backends, case.clone());
                if minimized != *case {
                    let path = write_minimized_case(&minimized)?;
                    eprintln!("wrote minimized failing case to {}", path.display());
                }
            }
            eprintln!("differential mismatch at {context}");
            eprintln!("case:\n{case:#?}");
            eprintln!("{}:\n{baseline:#?}", backends[0].name());
            eprintln!("{}:\n{got:#?}", backend.name());
            return Err("differential mismatch".into());
        }
    }
    Ok(baseline)
}

/// Runs `case` like [`compare_case`], but treats matching transaction-validation rejections as
/// equivalent even if the clients report different first error labels.
pub fn compare_case_acceptance(
    backends: &[&dyn EvmBackend],
    case: &EvmCase,
    context: CaseContext<'_>,
) -> Result<Outcome, String> {
    debug_assert!(backends.len() >= 2);
    let baseline = backends[0].run(case);
    for backend in &backends[1..] {
        let got = backend.run(case);
        if baseline.kind == OutcomeKind::Error && got.kind == OutcomeKind::Error {
            continue;
        }
        if got != baseline {
            if let CaseContext::Generated { seed, case_index } = context {
                let path = write_failure_case(seed, case_index, case)?;
                eprintln!("wrote failing case to {}", path.display());
                let minimized = minimize_case(backends, case.clone());
                if minimized != *case {
                    let path = write_minimized_case(&minimized)?;
                    eprintln!("wrote minimized failing case to {}", path.display());
                }
            }
            eprintln!("differential mismatch at {context}");
            eprintln!("case:\n{case:#?}");
            eprintln!("{}:\n{baseline:#?}", backends[0].name());
            eprintln!("{}:\n{got:#?}", backend.name());
            return Err("differential mismatch".into());
        }
    }
    Ok(baseline)
}
