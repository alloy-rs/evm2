use std::{ops::RangeInclusive, path::PathBuf};

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
    /// List replay entrypoints in an EEST fixture.
    List(List),
    /// Replay an EEST JSON fixture.
    Replay(Replay),
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
}

#[derive(Debug, clap::Args)]
pub(crate) struct Replay {
    /// Logical EEST test or case name glob to run.
    #[arg(long)]
    pub(crate) entrypoint: Option<String>,
    /// EEST JSON fixture to replay.
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
}
