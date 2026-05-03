use crate::{
    error::{TestError, TestErrorKind},
    execute::{ExecuteConfig, execute_test_suite},
};
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use std::{
    io::{self, IsTerminal},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

/// State test runner configuration.
#[derive(Clone, Copy, Debug, Default)]
pub struct RunConfig {
    /// Optional fixed worker count.
    pub jobs: Option<usize>,
    /// Force single-threaded execution.
    pub single_thread: bool,
    /// Keep running after failures.
    pub keep_going: bool,
    /// Hide progress output.
    pub omit_progress: bool,
    /// Print revm-style JSON outcome lines.
    pub print_json_outcome: bool,
    /// Request execution tracing.
    pub trace: bool,
}

/// Runs a list of test files with an internal thread pool.
pub fn run(files: Vec<PathBuf>, jobs: usize, keep_going: bool) -> Result<(), TestError> {
    run_with_config(files, RunConfig { jobs: Some(jobs), keep_going, ..RunConfig::default() })
}

/// Runs a list of test files with explicit runner configuration.
pub fn run_with_config(files: Vec<PathBuf>, config: RunConfig) -> Result<(), TestError> {
    if config.trace {
        return Err(TestError::case("", "Runner config", TestErrorKind::TraceUnsupported));
    }

    let n_files = files.len();
    let jobs = determine_thread_count(config, n_files);
    let state = RunnerState::new(files, config.omit_progress);
    let mut handles = Vec::with_capacity(jobs);
    for i in 0..jobs {
        let state = state.clone();
        let handle = thread::Builder::new()
            .name(format!("statetest-{i}"))
            .spawn(move || worker(state, config))
            .map_err(|err| TestError::case("", "Thread spawn", TestErrorKind::ThreadSpawn(err)))?;
        handles.push(handle);
    }

    let mut thread_errors = Vec::new();
    for handle in handles {
        match handle.join() {
            Ok(Ok(())) => {}
            Ok(Err(err)) => thread_errors.push(err),
            Err(_) => thread_errors.push(TestError::case("", "Thread join", TestErrorKind::Panic)),
        };
    }
    state.bar.finish_and_clear();

    let failed = state.failed.load(Ordering::Relaxed);
    let passed = state.passed.load(Ordering::Relaxed);
    let elapsed = state.elapsed.lock().unwrap().as_secs_f64();
    println!(
        "Finished {passed} passed, {failed} failed, {n_files} files in {elapsed:.3}s CPU time"
    );

    if failed != 0 {
        let errors = state.errors.lock().unwrap();
        for error in errors.iter() {
            eprintln!("{error}");
        }
        drop(errors);
        return Err(TestError::case("", "Runner summary", TestErrorKind::Failures(failed)));
    }
    if let Some(error) = thread_errors.into_iter().next() {
        return Err(error);
    }
    Ok(())
}

fn worker(state: RunnerState, config: RunConfig) -> Result<(), TestError> {
    loop {
        if state.stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let Some(path) = state.next() else {
            return Ok(());
        };
        let result = execute_test_suite(
            &path,
            ExecuteConfig { print_json_outcome: config.print_json_outcome },
        );
        state.bar.inc(1);

        match result {
            Ok(outcome) => {
                state.passed.fetch_add(outcome.passed, Ordering::Relaxed);
                *state.elapsed.lock().unwrap() += outcome.elapsed;
            }
            Err(err) => {
                state.failed.fetch_add(1, Ordering::Relaxed);
                if !config.keep_going {
                    state.stop.store(true, Ordering::Relaxed);
                    return Err(err);
                }
                state.errors.lock().unwrap().push(err);
            }
        }
    }
}

#[derive(Clone)]
struct RunnerState {
    queue: Arc<Mutex<(usize, Vec<PathBuf>)>>,
    bar: ProgressBar,
    passed: Arc<AtomicUsize>,
    failed: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    elapsed: Arc<Mutex<Duration>>,
    errors: Arc<Mutex<Vec<TestError>>>,
}

impl RunnerState {
    fn new(files: Vec<PathBuf>, omit_progress: bool) -> Self {
        let total = files.len();
        let draw_target = if omit_progress || !io::stderr().is_terminal() {
            ProgressDrawTarget::hidden()
        } else {
            ProgressDrawTarget::stderr_with_hz(2)
        };
        let bar = ProgressBar::with_draw_target(Some(total as u64), draw_target);
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
            passed: Arc::new(AtomicUsize::new(0)),
            failed: Arc::new(AtomicUsize::new(0)),
            stop: Arc::new(AtomicBool::new(false)),
            elapsed: Arc::new(Mutex::new(Duration::ZERO)),
            errors: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn next(&self) -> Option<PathBuf> {
        let (next, queue) = &mut *self.queue.lock().unwrap();
        let path = queue.get(*next).cloned();
        *next += usize::from(path.is_some());
        path
    }
}

fn determine_thread_count(config: RunConfig, n_files: usize) -> usize {
    if config.single_thread || config.print_json_outcome {
        return 1;
    }
    config
        .jobs
        .or_else(|| thread::available_parallelism().ok().map(|jobs| jobs.get()))
        .unwrap_or(1)
        .min(n_files)
        .max(1)
}
