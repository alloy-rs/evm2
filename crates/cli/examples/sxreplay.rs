//! Times block execution of captured mainnet chunks: regular dispatch, superinstructions and JIT.
//!
//! Usage: sxreplay <reps> <modes> <fixture.json.zst>...
//!
//! Modes (comma separated):
//! - `base-cold`, `base-warm`, `sx-cold`, `sx-warm`: the interpreter without and with
//!   superinstructions. `warm` analyzes the pre-state's code before the timer starts, as a node
//!   with a code cache has it; `cold` analyzes during execution.
//! - `jit-warm` (with `--features jit`): LLVM JIT with every contract of the pre-state compiled
//!   before the timer starts (in parallel, on `JIT_THREADS` threads) into one code cache kept for
//!   the whole run, as a node keeps one; contracts created in the chunk compile on their first
//!   frame.
//! - `jit-tiered`: the JIT runtime's defaults (compiles in the background once a contract is seen 8
//!   times, up to 24 KiB of code, on up to 4 workers), with an empty cache each run.
//!
//! JIT modes use the runtime's default optimization level and let frames fall back to the
//! interpreter as a node does; their counts go to stderr. Each run gets a fresh pre-state, built
//! and dropped outside the timing, and skips sender recovery and post-state checks, as the mainnet
//! criterion bench does. Before timing, every mode replays each chunk once comparing receipt roots.
//!
//! With `PIN_CPU` set, the timed runs execute on that CPU only, and everything else (fixture
//! loading, JIT compile threads) on the CPUs other than it and its SMT siblings.
//!
//! Prints one tab-separated line per timed run: chunk, mode, rep, nanoseconds.

use evm2::{SpecId, evm::InMemoryDB};
use evm2_eest::{
    BlockchainTestExecuteConfig as Config, BlockchainTestNoopHook as Noop,
    blockchain_test_pre_state, blockchaintest::BlockchainTestCase, execute_blockchain_test_case_on,
    read_blockchain_fixture,
};
use std::{
    path::{Path, PathBuf},
    time::Instant,
};

#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Interp { sx: bool, warm: bool },
    Jit(Jit),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Jit {
    Warm,
    Tiered,
}

fn parse(m: &str) -> Mode {
    match m {
        "base-cold" => Mode::Interp { sx: false, warm: false },
        "base-warm" => Mode::Interp { sx: false, warm: true },
        "sx-cold" => Mode::Interp { sx: true, warm: false },
        "sx-warm" => Mode::Interp { sx: true, warm: true },
        "jit-warm" => Mode::Jit(Jit::Warm),
        "jit-tiered" => Mode::Jit(Jit::Tiered),
        _ => panic!("unknown mode {m}"),
    }
}

#[cfg(feature = "jit")]
mod jit {
    use super::*;
    use evm2_jit_runtime::{
        OptimizationLevel,
        runtime::{JitBackend, RuntimeConfig, RuntimeTuning},
    };

    pub(crate) type Backend = JitBackend;

    pub(crate) fn backend(kind: Jit) -> Backend {
        let tuning = match kind {
            // Compile everything, waiting for it.
            Jit::Warm => RuntimeTuning {
                jit_hot_threshold: 0,
                jit_max_bytecode_len: 0,
                jit_worker_count: threads(),
                jit_opt_level: OptimizationLevel::default(),
                ..Default::default()
            },
            Jit::Tiered => RuntimeTuning::default(),
        };
        JitBackend::new(RuntimeConfig {
            enabled: true,
            blocking: kind != Jit::Tiered,
            tuning,
            ..Default::default()
        })
        .expect("JIT backend")
    }

    pub(crate) fn threads() -> usize {
        std::env::var("JIT_THREADS").ok().and_then(|v| v.parse().ok()).unwrap_or(14)
    }

    /// Compiles every contract of `db` for `spec` on [`threads`] threads; returns how many and
    /// their code size.
    pub(crate) fn precompile(backend: &Backend, db: &InMemoryDB, spec: SpecId) -> (usize, usize) {
        use evm2_jit_runtime::runtime::{LookupCode, LookupRequest, RuntimeCacheKey};
        let mut codes: alloy_primitives::map::B256Map<&evm2::bytecode::Bytecode> =
            Default::default();
        for (hash, code) in &db.cache.contracts {
            codes.insert(*hash, code);
        }
        for info in db.cache.accounts.values().flatten() {
            if let Some(code) = &info.code {
                codes.insert(info.code_hash, code);
            }
        }
        let codes: Vec<_> =
            codes.into_iter().filter(|(_, c)| !c.original_byte_slice().is_empty()).collect();
        let n = threads();
        std::thread::scope(|scope| {
            for t in 0..n {
                let codes = &codes;
                scope.spawn(move || {
                    for (hash, code) in codes.iter().skip(t).step_by(n) {
                        if backend.get_compiled(*hash, spec).is_some() {
                            continue;
                        }
                        let req = LookupRequest {
                            key: RuntimeCacheKey { code_hash: *hash, spec_id: spec },
                            code: LookupCode::Bytecode(code),
                        };
                        backend.compile_jit_sync(req).expect("compile");
                    }
                });
            }
        });
        (codes.len(), codes.iter().map(|(_, c)| c.original_byte_slice().len()).sum())
    }

    pub(crate) fn run(
        path: &Path,
        name: &str,
        case: &BlockchainTestCase,
        db: &mut InMemoryDB,
        config: Config,
        backend: &Backend,
    ) -> Result<(), String> {
        evm2_eest::execute_blockchain_test_case_on_jit(
            path, name, case, db, config, &mut Noop, backend,
        )
        .map(drop)
        .map_err(|e| e.to_string())
    }

    /// Starts the runtime's threads now, from the calling thread, so they inherit its CPUs.
    pub(crate) fn start(backend: &Backend) {
        use evm2_jit_runtime::runtime::{LookupCode, LookupRequest, RuntimeCacheKey};
        let req = LookupRequest {
            key: RuntimeCacheKey {
                code_hash: alloy_primitives::B256::repeat_byte(0xAB),
                spec_id: SpecId::OSAKA,
            },
            code: LookupCode::Bytes(&[0x00]),
        };
        backend.compile_jit_sync(req).expect("start JIT runtime");
    }

    pub(crate) fn report(chunk: &str, mode: &str, backend: &Backend) {
        let s = backend.stats();
        eprintln!(
            "#jit\t{chunk}\t{mode}\thits={}\tmisses={}\tfallbacks={}\tcompiled={}\tfailed={}\tcode_bytes={}",
            s.lookup_hits,
            s.lookup_misses,
            s.interpreter_fallbacks,
            s.compilations_succeeded,
            s.compilations_failed,
            s.jit_code_bytes
        );
    }
}

#[cfg(not(feature = "jit"))]
mod jit {
    use super::*;
    pub(crate) type Backend = ();
    pub(crate) fn backend(_: Jit) -> Backend {
        panic!("JIT modes need --features jit")
    }
    pub(crate) fn run(
        _: &Path,
        _: &str,
        _: &BlockchainTestCase,
        _: &mut InMemoryDB,
        _: Config,
        _: &Backend,
    ) -> Result<(), String> {
        unreachable!()
    }
    pub(crate) fn report(_: &str, _: &str, _: &Backend) {}
    pub(crate) fn start(_: &Backend) {}
    pub(crate) fn precompile(_: &Backend, _: &InMemoryDB, _: SpecId) -> (usize, usize) {
        unreachable!()
    }
}

fn set_sx(sx: bool) {
    #[cfg(feature = "sx")]
    evm2_eest::sx::set_enabled(sx);
    #[cfg(not(feature = "sx"))]
    assert!(!sx, "built without superinstructions");
}

fn prewarm(db: &InMemoryDB, spec: SpecId) {
    #[cfg(feature = "sx")]
    evm2_eest::sx::prewarm(db, spec);
    #[cfg(not(feature = "sx"))]
    {
        let _ = (db, spec);
        panic!("warm modes need the sx build, whose prewarm runs plain jump analysis when off");
    }
}

fn interp(
    path: &Path,
    name: &str,
    case: &BlockchainTestCase,
    db: &mut InMemoryDB,
    config: Config,
) -> Result<(), String> {
    execute_blockchain_test_case_on(path, name, case, db, config, &mut Noop)
        .map(drop)
        .map_err(|e| e.to_string())
}

/// Moves the calling thread to CPU `cpu`, or lets it run on `cpus` again.
fn pin(cpus: &[usize]) {
    // SAFETY: `set` is a valid CPU set for this call.
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        for &cpu in cpus {
            libc::CPU_SET(cpu, &mut set);
        }
        assert_eq!(libc::sched_setaffinity(0, size_of::<libc::cpu_set_t>(), &set), 0);
    }
}

fn main() {
    let pinned: Option<usize> = std::env::var("PIN_CPU").ok().map(|v| v.parse().unwrap());
    let siblings: Vec<usize> = pinned
        .map(|cpu| {
            let list = std::fs::read_to_string(format!(
                "/sys/devices/system/cpu/cpu{cpu}/topology/thread_siblings_list"
            ))
            .unwrap();
            list.trim().split(',').map(|c| c.parse().unwrap()).collect()
        })
        .unwrap_or_default();
    let others: Vec<usize> = (0..std::thread::available_parallelism().unwrap().get())
        .filter(|c| !siblings.contains(c))
        .collect();
    let mut warm_jit = None;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let reps: usize = args[0].parse().expect("reps");
    let modes: Vec<(String, Mode)> =
        args[1].split(',').map(|m| (m.to_string(), parse(m))).collect();
    let timed = Config { validate: false, ..Default::default() };
    let checked = Config { validate: false, compare_receipt_root: true, ..Default::default() };
    for path in &args[2..] {
        let path = PathBuf::from(path);
        let chunk =
            path.file_name().unwrap().to_string_lossy().trim_end_matches(".json.zst").to_string();
        let suite = read_blockchain_fixture(&path).expect("fixture");
        let (name, case) = suite.0.first_key_value().expect("case");
        let spec = case.network.to_spec_id().expect("single-fork fixture");
        let pre = || blockchain_test_pre_state(&path, name, case).expect("pre-state");

        // Checks each mode once; the warm JIT backend compiles everything here.
        pin(&others);
        let mut ok = true;
        for (m, mode) in &modes {
            let mut db = pre();
            let result = match *mode {
                Mode::Interp { sx, warm } => {
                    set_sx(sx);
                    if warm {
                        prewarm(&db, spec);
                    }
                    interp(&path, name, case, &mut db, checked)
                }
                Mode::Jit(kind) => {
                    set_sx(false);
                    let fresh = (kind != Jit::Warm).then(|| jit::backend(kind));
                    if kind == Jit::Warm && warm_jit.is_none() {
                        warm_jit = Some(jit::backend(kind));
                    }
                    let backend = fresh.as_ref().or(warm_jit.as_ref()).unwrap();
                    if kind == Jit::Warm {
                        let start = Instant::now();
                        let (n, bytes) = jit::precompile(backend, &db, spec);
                        eprintln!(
                            "#compile\t{chunk}\t{n} contracts\t{bytes} bytes\t{:.1} s on {} threads",
                            start.elapsed().as_secs_f64(),
                            jit::threads()
                        );
                    }
                    jit::run(&path, name, case, &mut db, checked, backend)
                }
            };
            if let Err(err) = result {
                eprintln!("{chunk} {m}: {err}");
                ok = false;
            }
        }
        if !ok {
            println!("{chunk}\tFAIL\t0\t0");
            continue;
        }
        if let Some(cpu) = pinned {
            pin(&[cpu]);
        }
        for rep in 0..reps {
            for k in 0..modes.len() {
                let (m, mode) = &modes[(k + rep) % modes.len()];
                let mut db = pre();
                let ns = match *mode {
                    Mode::Interp { sx, warm } => {
                        set_sx(sx);
                        if warm {
                            prewarm(&db, spec);
                        }
                        let start = Instant::now();
                        let r = interp(&path, name, case, &mut db, timed);
                        let ns = start.elapsed().as_nanos();
                        std::hint::black_box(r).expect("replay");
                        ns
                    }
                    Mode::Jit(kind) => {
                        set_sx(false);
                        // The runtime's threads start off the timing CPU.
                        pin(&others);
                        let fresh = (kind != Jit::Warm).then(|| jit::backend(kind));
                        if let Some(backend) = &fresh {
                            jit::start(backend);
                        }
                        if let Some(cpu) = pinned {
                            pin(&[cpu]);
                        }
                        let backend = fresh.as_ref().or(warm_jit.as_ref()).unwrap();
                        let start = Instant::now();
                        let r = jit::run(&path, name, case, &mut db, timed, backend);
                        let ns = start.elapsed().as_nanos();
                        std::hint::black_box(r).expect("replay");
                        if let Some(backend) = &fresh {
                            if rep == 0 {
                                jit::report(&chunk, m, backend);
                            }
                        }
                        drop(fresh);
                        ns
                    }
                };
                println!("{chunk}\t{m}\t{rep}\t{ns}");
                drop(db);
            }
        }
        if let Some(backend) = &warm_jit {
            jit::report(&chunk, "jit-warm", backend);
        }
    }
    // Tearing down the JIT runtime with resident code crashes; the run is over, so skip it.
    std::mem::forget(warm_jit);
    // SAFETY: Nothing is left to flush but stdout, which is flushed here.
    use std::io::Write;
    std::io::stdout().flush().unwrap();
    unsafe { libc::_exit(0) }
}
