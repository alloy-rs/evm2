use crate::{
    args::Replay,
    error::{Error, Result},
    fixture::{self, FixtureKind},
    style,
};
use alloy_primitives::U256;
use evm2_eest::{
    BlockchainTestBlockFailed, BlockchainTestBlockFinished, BlockchainTestBlockStarted,
    BlockchainTestCaseStarted, BlockchainTestExecuteConfig, BlockchainTestExecutionMode,
    BlockchainTestHook, BlockchainTestTransactionFailed, BlockchainTestTransactionFinished,
    BlockchainTestTransactionStarted, EntryPoint, StateTestExecuteConfig, StateTestExecutionMode,
    execute_blockchain_tests_str, execute_blockchain_tests_suite,
    execute_state_tests_str_with_filter,
};
use std::{
    path::{Path, PathBuf},
    time::Instant,
};

/// Settings shared across every fixture replayed in one command invocation.
struct ReplayOptions {
    trace: bool,
    print_json_outcome: bool,
    dump_state: bool,
    blockchain_mode: BlockchainTestExecutionMode,
    state_mode: StateTestExecutionMode,
    db_stats: bool,
    /// Glob over the EEST test/case name (`--filter-test`).
    test_filter: EntryPoint,
    /// Glob over the `.json` file path or name in folder mode (`--filter-file`).
    file_filter: EntryPoint,
}

/// Executed/skipped counts and detected kind for a single fixture file.
struct FileReplay {
    kind: &'static str,
    executed: usize,
    skipped: usize,
}

pub(crate) fn run(command: Replay) -> Result<()> {
    #[cfg(feature = "jit")]
    if command.json_traces && (command.jit || command.aot) {
        return Err(Error::InvalidArgs(
            "--json-traces requires the interpreter backend; drop --jit/--aot".to_string(),
        ));
    }
    let options = ReplayOptions {
        trace: command.json_traces,
        print_json_outcome: command.json_output,
        dump_state: command.dump_state,
        blockchain_mode: replay_blockchain_execution_mode(&command),
        state_mode: replay_state_execution_mode(&command),
        db_stats: command.db_stats,
        test_filter: EntryPoint::new(command.filter_test),
        file_filter: EntryPoint::new(command.filter_file),
    };

    if command.path.is_dir() {
        return run_dir(&command.path, &options);
    }

    let replay = replay_file(&command.path, &options)?;
    let ok = style::OK;
    println!(
        "{ok}ok{ok:#}: replayed {} fixture {}: {} executed, {} skipped",
        replay.kind,
        command.path.display(),
        replay.executed,
        replay.skipped
    );
    Ok(())
}

/// Replays every `.json` file found anywhere under `dir`, continuing past files
/// that fail so a single bad fixture does not hide the rest. Returns an error if
/// any file failed.
fn run_dir(dir: &Path, options: &ReplayOptions) -> Result<()> {
    let found = collect_json_files(dir)?;
    if found.is_empty() {
        let warn = style::WARN;
        println!("{warn}warning{warn:#}: no .json files found under {}", dir.display());
        return Ok(());
    }
    let files: Vec<&PathBuf> =
        found.iter().filter(|path| file_selected(&options.file_filter, path)).collect();
    if files.is_empty() {
        let warn = style::WARN;
        println!(
            "{warn}warning{warn:#}: none of the {} .json files under {} matched --filter-file",
            found.len(),
            dir.display()
        );
        return Ok(());
    }

    let info = style::INFO;
    eprintln!("{info}replay{info:#}: {} json files under {}", files.len(), dir.display());

    let mut executed = 0;
    let mut skipped = 0;
    let mut failed = 0;
    for &file in &files {
        match replay_file(file, options) {
            Ok(replay) => {
                executed += replay.executed;
                skipped += replay.skipped;
                let ok = style::OK;
                println!(
                    "{ok}ok{ok:#}: {} {}: {} executed, {} skipped",
                    replay.kind,
                    file.display(),
                    replay.executed,
                    replay.skipped
                );
            }
            Err(err) => {
                failed += 1;
                let error = style::ERROR;
                eprintln!("{error}failed{error:#}: {}: {err}", file.display());
            }
        }
    }

    let total = files.len();
    let summary_style = if failed == 0 { style::OK } else { style::ERROR };
    println!(
        "{summary_style}done{summary_style:#}: replayed {total} files under {}: {executed} executed, {skipped} skipped, {failed} failed",
        dir.display()
    );
    if failed > 0 {
        return Err(Error::ReplayFailures { failed, total });
    }
    Ok(())
}

/// Replays one fixture file, auto-detecting its kind (binary blockchain, or a
/// text state/blockchain test).
fn replay_file(path: &Path, options: &ReplayOptions) -> Result<FileReplay> {
    if fixture::is_binary_path(path) {
        let suite = fixture::read_blockchain(path)?;
        let mut hook = ReplayProgressHook::default();
        let summary = execute_blockchain_tests_suite(
            path,
            &suite,
            BlockchainTestExecuteConfig {
                mode: options.blockchain_mode,
                db_stats: options.db_stats,
                trace: options.trace,
                print_json_outcome: options.print_json_outcome,
                dump_state: options.dump_state,
                compare_state_root: true,
                ..Default::default()
            },
            &options.test_filter,
            &mut hook,
        )
        .map_err(|source| Error::BlockchainTest { source })?;
        return Ok(FileReplay {
            kind: "blockchain",
            executed: summary.executed,
            skipped: summary.skipped,
        });
    }

    let input = fixture::read_text(path)?;
    match fixture::detect_str(path, &input)? {
        Some(FixtureKind::StateTest) => {
            let summary = execute_state_tests_str_with_filter(
                path,
                &input,
                StateTestExecuteConfig {
                    mode: options.state_mode,
                    db_stats: options.db_stats,
                    trace: options.trace,
                    print_json_outcome: options.print_json_outcome,
                    dump_state: options.dump_state,
                },
                &options.test_filter,
            )
            .map_err(|source| Error::StateTest { source })?;
            Ok(FileReplay { kind: "state", executed: summary.executed, skipped: summary.skipped })
        }
        Some(FixtureKind::BlockchainTest) => {
            let mut hook = ReplayProgressHook::default();
            let summary = execute_blockchain_tests_str(
                path,
                &input,
                BlockchainTestExecuteConfig {
                    mode: options.blockchain_mode,
                    db_stats: options.db_stats,
                    trace: options.trace,
                    print_json_outcome: options.print_json_outcome,
                    dump_state: options.dump_state,
                    compare_state_root: true,
                    ..Default::default()
                },
                &options.test_filter,
                &mut hook,
            )
            .map_err(|source| Error::BlockchainTest { source })?;
            Ok(FileReplay {
                kind: "blockchain",
                executed: summary.executed,
                skipped: summary.skipped,
            })
        }
        None => Err(Error::UnknownFixtureKind { path: path.to_path_buf() }),
    }
}

/// Whether `path` is selected by the `--filter-file` glob. The pattern matches
/// against either the full path or the bare file name, so `*/create/*` and
/// `create_*.json` both work; an empty filter selects everything.
fn file_selected(filter: &EntryPoint, path: &Path) -> bool {
    filter.matches(&path.to_string_lossy())
        || path.file_name().is_some_and(|name| filter.matches(&name.to_string_lossy()))
}

/// Recursively collects every `.json` file under `dir`, sorted for a stable
/// replay order.
fn collect_json_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    collect_json_files_into(dir, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_json_files_into(dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    let entries = std::fs::read_dir(dir)
        .map_err(|source| Error::ReadInput { path: dir.to_path_buf(), source })?;
    for entry in entries {
        let entry = entry.map_err(|source| Error::ReadInput { path: dir.to_path_buf(), source })?;
        let path = entry.path();
        if path.is_dir() {
            collect_json_files_into(&path, files)?;
        } else if path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("json")) {
            files.push(path);
        }
    }
    Ok(())
}

const fn replay_state_execution_mode(_command: &Replay) -> StateTestExecutionMode {
    #[cfg(feature = "jit")]
    {
        if _command.jit {
            return StateTestExecutionMode::Jit;
        }
        if _command.aot {
            return StateTestExecutionMode::Aot;
        }
    }
    StateTestExecutionMode::Interpreter
}

const fn replay_blockchain_execution_mode(_command: &Replay) -> BlockchainTestExecutionMode {
    #[cfg(feature = "jit")]
    {
        if _command.jit {
            return BlockchainTestExecutionMode::Jit;
        }
        if _command.aot {
            return BlockchainTestExecutionMode::Aot;
        }
    }
    BlockchainTestExecutionMode::Interpreter
}

#[derive(Default)]
struct ReplayProgressHook {
    block_started_at: Option<Instant>,
    printed_transaction_failure: bool,
    case_name: Option<String>,
    case_elapsed_sec: f64,
    case_gas_used: u128,
    case_blocks_with_gas: usize,
}

impl BlockchainTestHook for ReplayProgressHook {
    fn case_started(&mut self, event: BlockchainTestCaseStarted<'_>) {
        self.case_name = Some(event.name.to_owned());
        self.case_elapsed_sec = 0.0;
        self.case_gas_used = 0;
        self.case_blocks_with_gas = 0;
        let info = style::INFO;
        eprintln!(
            "{info}replay{info:#}: case {}: {} blocks, network {:?}",
            event.name, event.total_blocks, event.network
        );
    }

    fn block_started(&mut self, _event: BlockchainTestBlockStarted) {
        self.block_started_at = Some(Instant::now());
        self.printed_transaction_failure = false;
    }

    fn block_finished(&mut self, event: BlockchainTestBlockFinished) {
        let elapsed = self.take_block_elapsed();
        self.record_block(event.block_gas_used, elapsed);
        let block_index = event.block_index + 1;
        if !style::should_print_progress(block_index, event.total_blocks) {
            if block_index == event.total_blocks {
                self.print_case_summary(event.total_blocks);
            }
            return;
        }
        let ok = style::OK;
        let block_width = decimal_width(event.total_blocks);
        let total_blocks = event.total_blocks;
        let block_number = display_block_number(event.block_number, event.block_index);
        if let Some(block_gas_used) = event.block_gas_used
            && let Some(ggas_per_second) =
                ggas_per_second_from_gas(block_gas_used.saturating_to::<u128>(), elapsed)
        {
            let block_ggas = ggas(block_gas_used.saturating_to::<u128>());
            eprintln!(
                "{ok}done{ok:#}: block {block_index:block_width$}/{total_blocks} number={block_number} in {elapsed:.2}s ({block_ggas:.3} Ggas, {ggas_per_second:.3} Ggas/s)"
            );
        } else {
            eprintln!(
                "{ok}done{ok:#}: block {block_index:block_width$}/{total_blocks} number={block_number} in {elapsed:.2}s"
            );
        }

        if block_index == event.total_blocks {
            self.print_case_summary(event.total_blocks);
        }
    }

    fn block_failed(&mut self, event: BlockchainTestBlockFailed<'_>) {
        let elapsed = self.take_block_elapsed();
        let error = style::ERROR;
        if self.printed_transaction_failure {
            eprintln!(
                "{error}failed{error:#}: block {}/{} number={} in {:.2}s after transaction failure",
                event.block_index + 1,
                event.total_blocks,
                display_block_number(event.block_number, event.block_index),
                elapsed
            );
        } else {
            eprintln!(
                "{error}failed{error:#}: block {}/{} number={} in {:.2}s: {}",
                event.block_index + 1,
                event.total_blocks,
                display_block_number(event.block_number, event.block_index),
                elapsed,
                event.error
            );
        }
    }

    fn transaction_started(&mut self, _event: BlockchainTestTransactionStarted) {}

    fn transaction_finished(&mut self, event: BlockchainTestTransactionFinished) {
        if event.total_transactions >= 1_000
            && ((event.transaction_index + 1).is_multiple_of(500)
                || event.transaction_index + 1 == event.total_transactions)
        {
            let info = style::INFO;
            eprintln!(
                "{info}progress{info:#}: tx {}/{} in block {}/{} number={} done",
                event.transaction_index + 1,
                event.total_transactions,
                event.block_index + 1,
                event.total_blocks,
                display_block_number(event.block_number, event.block_index)
            );
        }
    }

    fn transaction_failed(&mut self, event: BlockchainTestTransactionFailed<'_>) {
        self.printed_transaction_failure = true;
        let error = style::ERROR;
        eprintln!(
            "{error}failed{error:#}: tx {}/{} in block {}/{} number={}: {}",
            event.transaction_index + 1,
            event.total_transactions,
            event.block_index + 1,
            event.total_blocks,
            display_block_number(event.block_number, event.block_index),
            event.error
        );
    }
}

impl ReplayProgressHook {
    fn take_block_elapsed(&mut self) -> f64 {
        self.block_started_at.take().map(|started| started.elapsed().as_secs_f64()).unwrap_or(0.0)
    }

    fn record_block(&mut self, block_gas_used: Option<U256>, elapsed: f64) {
        self.case_elapsed_sec += elapsed;
        if let Some(gas_used) = block_gas_used.map(|gas| gas.saturating_to::<u128>()) {
            self.case_gas_used = self.case_gas_used.saturating_add(gas_used);
            self.case_blocks_with_gas += 1;
        }
    }

    fn print_case_summary(&self, total_blocks: usize) {
        let Some(case_name) = &self.case_name else {
            return;
        };
        let Some(ggas_per_second) =
            ggas_per_second_from_gas(self.case_gas_used, self.case_elapsed_sec)
        else {
            let ok = style::OK;
            eprintln!(
                "{ok}done{ok:#}: case {}: {} blocks in {:.2}s",
                case_name, total_blocks, self.case_elapsed_sec
            );
            return;
        };

        let ok = style::OK;
        if self.case_blocks_with_gas == total_blocks {
            eprintln!(
                "{ok}done{ok:#}: case {}: {} blocks in {:.2}s ({:.3} Ggas/s aggregate, {:.3} Ggas total)",
                case_name,
                total_blocks,
                self.case_elapsed_sec,
                ggas_per_second,
                ggas(self.case_gas_used)
            );
        } else {
            let warn = style::WARN;
            eprintln!(
                "{warn}done{warn:#}: case {}: {} blocks in {:.2}s ({:.3} Ggas/s aggregate, {:.3} Ggas total across {}/{} blocks with gasUsed)",
                case_name,
                total_blocks,
                self.case_elapsed_sec,
                ggas_per_second,
                ggas(self.case_gas_used),
                self.case_blocks_with_gas,
                total_blocks
            );
        }
    }
}

fn display_block_number(block_number: Option<U256>, fallback: usize) -> String {
    block_number.map(|number| number.to_string()).unwrap_or_else(|| fallback.to_string())
}

fn decimal_width(value: usize) -> usize {
    value.checked_ilog10().unwrap_or_default() as usize + 1
}

fn ggas_per_second_from_gas(gas_used: u128, elapsed: f64) -> Option<f64> {
    (elapsed > 0.0).then_some(ggas(gas_used) / elapsed)
}

fn ggas(gas_used: u128) -> f64 {
    gas_used as f64 / 1_000_000_000.0
}

#[cfg(test)]
mod collect_tests {
    use super::collect_json_files;
    use std::{
        fs,
        sync::atomic::{AtomicUsize, Ordering},
    };

    /// Creates a unique scratch directory under the system temp dir.
    fn scratch_dir() -> std::path::PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir()
            .join(format!("evm2-replay-collect-{}-{unique}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn collects_json_files_recursively_and_sorted() {
        let root = scratch_dir();
        let nested = root.join("nested");
        fs::create_dir_all(&nested).unwrap();
        fs::write(root.join("b.json"), "{}").unwrap();
        fs::write(root.join("a.json"), "{}").unwrap();
        fs::write(root.join("notes.txt"), "ignored").unwrap();
        fs::write(nested.join("c.JSON"), "{}").unwrap();

        let files = collect_json_files(&root).unwrap();
        let names: Vec<_> =
            files.iter().map(|path| path.file_name().unwrap().to_str().unwrap()).collect();

        assert_eq!(names, ["a.json", "b.json", "c.JSON"]);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn empty_directory_yields_no_files() {
        let root = scratch_dir();
        assert!(collect_json_files(&root).unwrap().is_empty());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn file_filter_matches_path_or_file_name() {
        use super::{EntryPoint, file_selected};
        use std::path::Path;

        let path = Path::new("test-fixtures/frontier/create/create_one_byte.json");

        // An empty filter selects everything.
        assert!(file_selected(&EntryPoint::new(None), path));
        // Path-oriented glob.
        assert!(file_selected(&EntryPoint::new(Some("*/create/*".to_string())), path));
        // File-name-oriented glob.
        assert!(file_selected(&EntryPoint::new(Some("create_*.json".to_string())), path));
        // Non-matching glob.
        assert!(!file_selected(&EntryPoint::new(Some("*/access_list/*".to_string())), path));
    }
}

#[cfg(all(test, feature = "jit"))]
mod tests {
    use super::{
        BlockchainTestExecutionMode, Replay, StateTestExecutionMode,
        replay_blockchain_execution_mode, replay_state_execution_mode,
    };
    use std::path::PathBuf;

    fn replay(jit: bool, aot: bool) -> Replay {
        Replay {
            filter_test: None,
            filter_file: None,
            jit,
            aot,
            db_stats: false,
            json_traces: false,
            json_output: false,
            dump_state: false,
            path: PathBuf::from("fixture.json"),
        }
    }

    #[test]
    fn replay_execution_mode_defaults_to_interpreter() {
        let command = replay(false, false);

        assert_eq!(replay_state_execution_mode(&command), StateTestExecutionMode::Interpreter);
        assert_eq!(
            replay_blockchain_execution_mode(&command),
            BlockchainTestExecutionMode::Interpreter
        );
    }

    #[test]
    fn replay_execution_mode_selects_jit() {
        let command = replay(true, false);

        assert_eq!(replay_state_execution_mode(&command), StateTestExecutionMode::Jit);
        assert_eq!(replay_blockchain_execution_mode(&command), BlockchainTestExecutionMode::Jit);
    }

    #[test]
    fn replay_execution_mode_selects_aot() {
        let command = replay(false, true);

        assert_eq!(replay_state_execution_mode(&command), StateTestExecutionMode::Aot);
        assert_eq!(replay_blockchain_execution_mode(&command), BlockchainTestExecutionMode::Aot);
    }
}
