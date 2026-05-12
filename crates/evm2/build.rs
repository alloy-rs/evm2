//! Configures the optional tail-call interpreter backend.

use std::{env, ffi::OsString, process::Command};

fn main() {
    println!("cargo:rustc-check-cfg=cfg(dispatch_packed)");
    println!("cargo:rustc-check-cfg=cfg(dispatch_single_return)");
    println!("cargo:rustc-check-cfg=cfg(tco)");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=CARGO_CFG_TARGET_ARCH");
    println!("cargo:rerun-if-env-changed=CARGO_CFG_TARGET_FAMILY");
    println!("cargo:rerun-if-env-changed=CARGO_CFG_TARGET_POINTER_WIDTH");
    println!("cargo:rerun-if-env-changed=RUSTC");
    println!("cargo:rerun-if-env-changed=TARGET");

    let is_wasm = target_is_wasm();
    if is_wasm {
        println!("cargo:rustc-cfg=dispatch_single_return");
    }
    if target_pointer_width() == Some("64") && !is_wasm {
        println!("cargo:rustc-cfg=dispatch_packed");
    }

    let no_tco_requested = env::var_os("CARGO_FEATURE_NO_TCO").is_some();
    let nightly_requested = env::var_os("CARGO_FEATURE_NIGHTLY").is_some();
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
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let target_family = env::var("CARGO_CFG_TARGET_FAMILY").unwrap_or_default();
    target_arch.starts_with("wasm") || target_family.split(',').any(|family| family == "wasm")
}

fn target_pointer_width() -> Option<&'static str> {
    match env::var("CARGO_CFG_TARGET_POINTER_WIDTH").as_deref() {
        Ok("16") => Some("16"),
        Ok("32") => Some("32"),
        Ok("64") => Some("64"),
        _ => None,
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
