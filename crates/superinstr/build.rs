//! Enables the tail-call executor on nightly compilers, as evm2's interpreter does.

use std::{ffi::OsString, process::Command};

fn main() {
    println!("cargo:rustc-check-cfg=cfg(tco)");
    println!("cargo:rerun-if-changed=build.rs");
    if env("EVM2_SUPERINSTR_NO_TCO").is_none() && !is_cranelift() && rustc_is_nightly() {
        println!("cargo:rustc-cfg=tco");
    }
}

fn rustc_is_nightly() -> bool {
    let rustc = env("RUSTC").unwrap_or_else(|| OsString::from("rustc"));
    let Ok(output) = Command::new(rustc).arg("-Vv").output() else { return false };
    let stdout = String::from_utf8_lossy(&output.stdout);
    output.status.success()
        && stdout.lines().any(|line| {
            line.strip_prefix("release: ").is_some_and(|release| release.contains("nightly"))
        })
}

// Cranelift does not implement `rust-preserve-none`.
fn is_cranelift() -> bool {
    env("CARGO_ENCODED_RUSTFLAGS").is_some_and(|rustflags| {
        rustflags
            .to_string_lossy()
            .split('\x1f')
            .any(|flag| flag.contains("codegen-backend") && flag.contains("cranelift"))
    })
}

fn env(key: &str) -> Option<OsString> {
    println!("cargo:rerun-if-env-changed={key}");
    std::env::var_os(key)
}
