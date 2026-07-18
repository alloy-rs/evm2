use std::{num::NonZeroUsize, ops::RangeInclusive, path::PathBuf};

const DEFAULT_MAX_CONCURRENT_REQUESTS: NonZeroUsize = NonZeroUsize::new(8).unwrap();
const DEFAULT_RPC_RETRIES: u32 = 3;

#[derive(Debug, clap::Parser)]
#[command(name = "evm2", version, about = "Capture and replay Ethereum execution fixtures.")]
pub(crate) struct Args {
    #[command(subcommand)]
    pub(crate) command: Command,
}

#[derive(Debug, clap::Subcommand)]
pub(crate) enum Command {
    /// Capture a mainnet block range from JSON-RPC into an EEST fixture.
    Capture(Capture),
    /// Run the differential fuzzer against revm.
    Fuzzer(crate::fuzzer::Options),
    /// List the test/case names in an EEST fixture (usable with `--filter-test`).
    List(List),
    /// Replay an EEST JSON fixture, or every `.json` fixture under a folder.
    Replay(Replay),
    /// Compile and/or run EVM bytecode.
    #[cfg(feature = "jit")]
    Run(crate::run::RunArgs),
}

#[derive(Debug, clap::Args)]
pub(crate) struct Capture {
    /// JSON-RPC HTTP URL.
    #[arg(long)]
    pub(crate) rpc: String,
    /// Inclusive block range, for example 24855016-24856015.
    #[arg(long, value_parser = parse_block_range)]
    pub(crate) range: RangeInclusive<u64>,
    /// EEST JSON file to write.
    #[arg(long, value_name = "PATH")]
    pub(crate) output: PathBuf,
    /// Maximum number of in-flight JSON-RPC requests.
    #[arg(long, default_value_t = DEFAULT_MAX_CONCURRENT_REQUESTS)]
    pub(crate) max_concurrent_requests: NonZeroUsize,
    /// Maximum number of Alloy retry attempts for retryable RPC errors.
    #[arg(long, default_value_t = DEFAULT_RPC_RETRIES)]
    pub(crate) rpc_retries: u32,
}

#[derive(Debug, clap::Args)]
pub(crate) struct Replay {
    /// Only replay tests whose EEST test/case name matches this glob.
    #[arg(long, value_name = "GLOB")]
    pub(crate) filter_test: Option<String>,
    /// When replaying a folder, only replay `.json` files whose path or file
    /// name matches this glob.
    #[arg(long, value_name = "GLOB")]
    pub(crate) filter_file: Option<String>,
    /// Replay through the evm2 JIT runtime.
    #[cfg(feature = "jit")]
    #[arg(long, conflicts_with = "aot")]
    pub(crate) jit: bool,
    /// Replay through the evm2 AOT runtime.
    #[cfg(feature = "jit")]
    #[arg(long, conflicts_with = "jit")]
    pub(crate) aot: bool,
    /// Print database method call counts after execution.
    #[arg(long)]
    pub(crate) db_stats: bool,
    /// Stream EIP-3155 execution traces (JSON struct logs) to stdout.
    ///
    /// Requires the interpreter backend; incompatible with `--jit`/`--aot`.
    #[arg(long)]
    pub(crate) json_traces: bool,
    /// Print each executed test's outcome (pass/fail, state root, gas) as JSON.
    #[arg(long)]
    pub(crate) json_output: bool,
    /// Dump the post-execution state (accounts and storage) to stdout.
    #[arg(long)]
    pub(crate) dump_state: bool,
    /// EEST JSON fixture to replay, or a folder to replay every `.json` under it.
    #[arg(value_name = "PATH")]
    pub(crate) path: PathBuf,
}

#[derive(Debug, clap::Args)]
pub(crate) struct List {
    /// EEST JSON fixture to inspect.
    #[arg(value_name = "PATH")]
    pub(crate) path: PathBuf,
}

fn parse_block_range(value: &str) -> Result<RangeInclusive<u64>, String> {
    let (start, end) = value
        .split_once('-')
        .ok_or_else(|| "expected inclusive block range START-END".to_string())?;
    let start =
        start.parse::<u64>().map_err(|err| format!("invalid range start {start:?}: {err}"))?;
    let end = end.parse::<u64>().map_err(|err| format!("invalid range end {end:?}: {err}"))?;
    if start > end {
        return Err(format!("range start {start} is greater than end {end}"));
    }
    Ok(start..=end)
}

#[cfg(test)]
mod tests {
    use super::parse_block_range;
    #[cfg(feature = "jit")]
    use super::{Args, Command};
    #[cfg(feature = "jit")]
    use clap::Parser;
    #[cfg(feature = "jit")]
    use std::path::PathBuf;

    #[test]
    fn parse_block_range_accepts_inclusive_range() {
        let range = parse_block_range("10-12").unwrap();
        assert_eq!(*range.start(), 10);
        assert_eq!(*range.end(), 12);
    }

    #[test]
    fn parse_block_range_rejects_reversed_range() {
        assert!(parse_block_range("12-10").unwrap_err().contains("greater"));
    }

    #[cfg(feature = "jit")]
    #[test]
    fn replay_accepts_jit_mode() {
        let args = Args::try_parse_from(["evm2", "replay", "--jit", "fixture.json"]).unwrap();
        let Command::Replay(replay) = args.command else { panic!("expected replay command") };
        assert!(replay.jit);
        assert!(!replay.aot);
        assert_eq!(replay.path, PathBuf::from("fixture.json"));
    }

    #[cfg(feature = "jit")]
    #[test]
    fn replay_accepts_aot_mode() {
        let args = Args::try_parse_from(["evm2", "replay", "--aot", "fixture.json"]).unwrap();
        let Command::Replay(replay) = args.command else { panic!("expected replay command") };
        assert!(!replay.jit);
        assert!(replay.aot);
        assert_eq!(replay.path, PathBuf::from("fixture.json"));
    }

    #[cfg(feature = "jit")]
    #[test]
    fn replay_rejects_jit_and_aot_together() {
        let err =
            Args::try_parse_from(["evm2", "replay", "--jit", "--aot", "fixture.json"]).unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }
}
