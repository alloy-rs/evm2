use crate::execution::ExecutionResources;
use evm2::evm::DbStatsCounts;
use std::{
    panic::{self, AssertUnwindSafe},
    path::PathBuf,
    sync::{
        Arc, Barrier, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread::{self, Builder},
    time::{Duration, Instant},
};

const WORKER_STACK_SIZE: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct FileSummary {
    pub(crate) executed: usize,
    pub(crate) skipped: usize,
    pub(crate) db_stats_counts: DbStatsCounts,
}

impl FileSummary {
    #[inline]
    pub(crate) fn add_assign(&mut self, rhs: Self) {
        self.executed += rhs.executed;
        self.skipped += rhs.skipped;
        self.db_stats_counts += rhs.db_stats_counts;
    }
}

pub(crate) fn run_files<E, F>(
    paths: Vec<PathBuf>,
    resources: ExecutionResources,
    run_file: F,
) -> Result<FileSummary, E>
where
    E: Send + 'static,
    F: Fn(PathBuf, ExecutionResources) -> Result<FileSummary, E> + Send + Sync + 'static,
{
    let n_files = paths.len();
    if n_files == 0 {
        return Ok(FileSummary::default());
    }

    let num_threads = if std::env::var_os("SINGLE_THREAD").is_some() {
        1
    } else {
        thread::available_parallelism().map_or(1, |n| n.get().min(n_files))
    };
    println!("Running {n_files} compiled EEST files on {num_threads} worker threads");

    let paths = Arc::new(paths);
    let next = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let barrier = Arc::new(Barrier::new(num_threads));
    let first_error = Arc::new(Mutex::new(None));
    let summary = Arc::new(Mutex::new(FileSummary::default()));
    let elapsed = Arc::new(Mutex::new(Duration::ZERO));
    let run_file = Arc::new(run_file);

    let mut handles = Vec::with_capacity(num_threads);
    for i in 0..num_threads {
        let paths = Arc::clone(&paths);
        let next = Arc::clone(&next);
        let stop = Arc::clone(&stop);
        let barrier = Arc::clone(&barrier);
        let first_error = Arc::clone(&first_error);
        let summary = Arc::clone(&summary);
        let elapsed = Arc::clone(&elapsed);
        let run_file = Arc::clone(&run_file);
        let resources = resources.clone();

        let handle = Builder::new()
            .name(format!("eest-runner-{i}"))
            .stack_size(WORKER_STACK_SIZE)
            .spawn(move || {
                let result = panic::catch_unwind(AssertUnwindSafe(|| {
                    loop {
                        if stop.load(Ordering::SeqCst) {
                            return;
                        }

                        let i = next.fetch_add(1, Ordering::SeqCst);
                        let Some(path) = paths.get(i).cloned() else {
                            return;
                        };

                        let t0 = Instant::now();
                        let result = run_file(path.clone(), resources.clone());
                        let file_elapsed = t0.elapsed();
                        *elapsed.lock().unwrap() += file_elapsed;

                        if file_elapsed > Duration::from_secs(5) {
                            eprintln!(
                                "slow compiled EEST file ({file_elapsed:?}): {}",
                                path.display()
                            );
                        }

                        match result {
                            Ok(file_summary) => summary.lock().unwrap().add_assign(file_summary),
                            Err(err) => {
                                *first_error.lock().unwrap() = Some(err);
                                stop.store(true, Ordering::SeqCst);
                                return;
                            }
                        }
                    }
                }));
                if result.is_err() {
                    stop.store(true, Ordering::SeqCst);
                }
                barrier.wait();
                result
            })
            .unwrap();

        handles.push(handle);
    }

    for handle in handles {
        if let Err(payload) = handle.join().unwrap() {
            panic::resume_unwind(payload);
        }
    }

    println!(
        "Finished compiled EEST execution. Total CPU time: {:.6}s",
        elapsed.lock().unwrap().as_secs_f64()
    );
    resources.print_runtime_stats();

    if let Some(err) = first_error.lock().unwrap().take() {
        Err(err)
    } else {
        Ok(*summary.lock().unwrap())
    }
}
