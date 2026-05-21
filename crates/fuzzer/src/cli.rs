use clap::{Parser, Subcommand};
use std::{path::PathBuf, time::Duration};

#[derive(Clone, Debug, Parser)]
pub(crate) struct Options {
    /// Seed used for deterministic structured generation.
    #[arg(long, default_value_t = 1, global = true)]
    pub(crate) seed: u64,
    /// Number of generated cases to run. Defaults to 256 unless --duration is set.
    #[arg(long, global = true)]
    pub(crate) cases: Option<u64>,
    /// Run generated cases for at most this duration (for example: 30s, 5m, 1h).
    #[arg(long, global = true, value_parser = parse_duration)]
    pub(crate) duration: Option<Duration>,
    #[command(subcommand)]
    pub(crate) command: Option<Command>,
}

#[derive(Clone, Debug, Subcommand)]
pub(crate) enum Command {
    /// Generate deterministic structured cases and compare all backends.
    Generate,
    /// Replay one saved JSON case.
    Replay { path: PathBuf },
    /// Replay all JSON cases under a path.
    Corpus { path: PathBuf },
    /// Minimize one saved JSON case that still reproduces a mismatch.
    Minimize { path: PathBuf },
}

fn parse_duration(value: &str) -> Result<Duration, String> {
    let value = value.trim();
    let (number, unit) = match value.find(|ch: char| !ch.is_ascii_digit()) {
        Some(index) => (&value[..index], &value[index..]),
        None => (value, "s"),
    };
    let amount = number.parse::<u64>().map_err(|err| format!("invalid duration: {err}"))?;
    let multiplier = match unit {
        "s" | "sec" | "secs" | "second" | "seconds" => 1,
        "m" | "min" | "mins" | "minute" | "minutes" => 60,
        "h" | "hr" | "hrs" | "hour" | "hours" => 60 * 60,
        _ => return Err(format!("unsupported duration unit {unit:?}; use s, m, or h")),
    };
    amount
        .checked_mul(multiplier)
        .map(Duration::from_secs)
        .ok_or_else(|| "duration is too large".to_string())
}
