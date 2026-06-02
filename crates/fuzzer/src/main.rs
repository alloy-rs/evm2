//! Small differential fuzzing proof-of-concept for evm2 against revm.
//!
//! This is intentionally a structured, deterministic generator rather than a
//! property-test harness. Run it with, for example:
//!
//! `cargo run -p evm2-fuzzer -- --seed 1 --cases 1000`
//!
//! or by duration:
//!
//! `cargo run -p evm2-fuzzer -- --seed 1 --duration 30s`
//!
//! Re-run saved cases with:
//!
//! `cargo run -p evm2-fuzzer -- replay crates/fuzzer/corpus/failures/seed-1-case-0.json`
//!
//! or run a directory of cases with:
//!
//! `cargo run -p evm2-fuzzer -- corpus crates/fuzzer/corpus/regressions`

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

use crate::{
    backend::{Evm2Backend, EvmBackend, RevmBackend},
    case::EvmCase,
    cli::{Command, Options},
    coverage::Coverage,
    io::{case_paths, read_case, write_failure_case, write_minimized_case},
    minimize::{differs, minimize_case},
    normalize::Outcome,
    rng::Gen,
};
use clap::Parser;
use std::{
    fmt,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::Instant,
};

fn main() {
    if let Err(err) = run(Options::parse()) {
        eprintln!("{err}");
        std::process::exit(1);
    }
}

fn run(opts: Options) -> Result<(), String> {
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

                    let mut rng = Gen::new(seed ^ case_index.wrapping_mul(0x9e37_79b9_7f4a_7c15));
                    let case = EvmCase::generate(&mut rng);
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

#[derive(Clone, Copy, Debug)]
enum CaseContext<'a> {
    Generated { seed: u64, case_index: u64 },
    Path(&'a Path),
}

impl fmt::Display for CaseContext<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Generated { seed, case_index } => {
                write!(f, "generated case {case_index}, seed {seed}")
            }
            Self::Path(path) => write!(f, "{}", path.display()),
        }
    }
}

fn compare_case(
    backends: &[&dyn EvmBackend; 2],
    case: &EvmCase,
    context: CaseContext<'_>,
) -> Result<Outcome, String> {
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
