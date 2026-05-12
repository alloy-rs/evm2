//! Configures the optional tail-call interpreter backend.

use std::{env as std_env, ffi::OsString, process::Command};

fn main() {
    println!("cargo:rustc-check-cfg=cfg(dispatch_packed)");
    println!("cargo:rustc-check-cfg=cfg(dispatch_single_return)");
    println!("cargo:rustc-check-cfg=cfg(dispatch_unpacked)");
    println!("cargo:rustc-check-cfg=cfg(tco)");
    println!("cargo:rerun-if-changed=build.rs");

    let is_wasm = target_is_wasm();
    let dispatch_packed = target_pointer_width() == Some(64) && !is_wasm;
    if is_wasm {
        println!("cargo:rustc-cfg=dispatch_single_return");
    }
    if dispatch_packed {
        println!("cargo:rustc-cfg=dispatch_packed");
    }
    if !is_wasm && !dispatch_packed {
        println!("cargo:rustc-cfg=dispatch_unpacked");
    }

    let no_tco_requested = env("CARGO_FEATURE_NO_TCO").is_some();
    let nightly_requested = env("CARGO_FEATURE_NIGHTLY").is_some();
    let is_nightly = rustc_is_nightly();

    if no_tco_requested {
        return;
    }

    if is_nightly {
        println!("cargo:rustc-cfg=tco");
    } else if nightly_requested {
        panic!("requested nightly backend requires a nightly compiler");
    }
}

fn target_is_wasm() -> bool {
    let target_arch =
        env("CARGO_CFG_TARGET_ARCH").and_then(|value| value.into_string().ok()).unwrap_or_default();
    let target_family = env("CARGO_CFG_TARGET_FAMILY")
        .and_then(|value| value.into_string().ok())
        .unwrap_or_default();
    target_arch.starts_with("wasm") || target_family.split(',').any(|family| family == "wasm")
}

fn target_pointer_width() -> Option<u32> {
    env("CARGO_CFG_TARGET_POINTER_WIDTH")?.to_str()?.parse().ok()
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
    env("RUSTC").unwrap_or_else(|| OsString::from("rustc"))
}

fn env(key: &str) -> Option<OsString> {
    println!("cargo:rerun-if-env-changed={key}");
    std_env::var_os(key)
}
