//! Compares `evm2 t8n` with the stored output of go-ethereum on the committed parity cases.
//!
//! Runs `scripts/geth_parity.py --offline` (standard library only) with this build of `evm2`.
//! Every case under `testdata/geth-parity/cases` is compared for each fork and tracer that has
//! stored geth output (see `--bless`). A difference fails the test unless `known-diffs.json`
//! lists it, and a listed difference that no longer occurs fails it too, so a fix deletes its
//! entries.

use std::{path::Path, process::Command};

#[test]
fn matches_stored_geth_output() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = Command::new("python3")
        .arg(root.join("scripts/geth_parity.py"))
        .args(["--offline", "--evm2", env!("CARGO_BIN_EXE_evm2")])
        .output()
        .expect("failed to run python3");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "geth parity check failed ({}):\n{}\n{}",
        output.status,
        stdout.lines().take(60).collect::<Vec<_>>().join("\n"),
        stderr
            .lines()
            .rev()
            .take(20)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n"),
    );
}
