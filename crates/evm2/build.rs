//! Configures the optional tail-call interpreter backend.

use std::{env, ffi::OsString, process::Command};

fn main() {
    println!("cargo:rustc-check-cfg=cfg(tco)");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=RUSTC");
    println!("cargo:rerun-if-env-changed=TARGET");

    let nightly_requested = env::var_os("CARGO_FEATURE_NIGHTLY").is_some();
    let is_nightly = rustc_is_nightly();

    if is_nightly {
        println!("cargo:rustc-cfg=tco");
    } else if nightly_requested {
        panic!("requested nightly backend requires a nightly compiler");
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

fn rustc() -> OsString {
    env::var_os("RUSTC").unwrap_or_else(|| OsString::from("rustc"))
}
