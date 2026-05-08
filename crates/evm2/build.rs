//! Configures the optional tail-call interpreter backend.

use std::{
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

fn main() {
    println!("cargo:rustc-check-cfg=cfg(evm2_tco)");
    println!("cargo:rerun-if-env-changed=RUSTC");
    println!("cargo:rerun-if-env-changed=TARGET");

    let tco_requested = env::var_os("CARGO_FEATURE_TCO").is_some();
    let nightly_requested = env::var_os("CARGO_FEATURE_NIGHTLY").is_some();
    let is_nightly = rustc_is_nightly();

    if !(is_nightly || tco_requested || nightly_requested) {
        return;
    }

    if probe_tco_support() {
        println!("cargo:rustc-cfg=evm2_tco");
    } else if tco_requested || nightly_requested {
        panic!("requested tco backend is not supported by this rustc/target");
    }
}

fn rustc_is_nightly() -> bool {
    let output = Command::new(rustc()).arg("-Vv").output();
    let Ok(output) = output else { return false };
    let stdout = String::from_utf8_lossy(&output.stdout);
    output.status.success()
        && stdout.lines().any(|line| {
            line.strip_prefix("release: ").is_some_and(|release| release.contains("nightly"))
        })
}

fn probe_tco_support() -> bool {
    let Some(target) = env::var_os("TARGET") else { return false };
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set by Cargo"));
    let source = out_dir.join("evm2_tco_probe.rs");
    let metadata = out_dir.join("evm2_tco_probe.rmeta");

    if fs::write(&source, TCO_PROBE).is_err() {
        return false;
    }

    let output = Command::new(rustc())
        .arg("--crate-name")
        .arg("evm2_tco_probe")
        .arg("--crate-type")
        .arg("lib")
        .arg("--edition")
        .arg("2024")
        .arg("--target")
        .arg(target)
        .arg("--emit")
        .arg("metadata")
        .arg("-o")
        .arg(&metadata)
        .arg(&source)
        .output();

    remove_file(&source);
    remove_file(&metadata);

    output.is_ok_and(|output| output.status.success())
}

fn rustc() -> OsString {
    env::var_os("RUSTC").unwrap_or_else(|| OsString::from("rustc"))
}

fn remove_file(path: &Path) {
    if path.exists() {
        let _ = fs::remove_file(path);
    }
}

const TCO_PROBE: &str = r#"
#![no_std]
#![feature(explicit_tail_calls, rust_preserve_none_cc)]
#![allow(incomplete_features)]

pub extern "rust-preserve-none" fn caller(x: usize) -> usize {
    become callee(x)
}

extern "rust-preserve-none" fn callee(x: usize) -> usize {
    x
}
"#;
