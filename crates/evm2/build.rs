//! Configures the optional tail-call interpreter backend.

use std::{ffi::OsString, process::Command};

fn main() {
    for cfg in ["dispatch_packed", "dispatch_single_return", "dispatch_unpacked", "tco"] {
        println!("cargo:rustc-check-cfg=cfg({cfg})");
    }
    println!("cargo:rerun-if-changed=build.rs");

    link_mcl_cpp_stdlib();

    // Select interpreter backend.
    let is_wasm = target_is_wasm();
    let target_pointer_width = target_pointer_width();
    let no_tco = env("CARGO_FEATURE_NO_TCO");
    match dispatch_backend_override() {
        Some(DispatchBackend::Tco) => println!("cargo:rustc-cfg=tco"),
        Some(DispatchBackend::Packed) => println!("cargo:rustc-cfg=dispatch_packed"),
        Some(DispatchBackend::SingleReturn) => println!("cargo:rustc-cfg=dispatch_single_return"),
        Some(DispatchBackend::Unpacked) => println!("cargo:rustc-cfg=dispatch_unpacked"),
        None => {
            if is_wasm {
                println!("cargo:rustc-cfg=dispatch_single_return");
            } else if target_pointer_width == Some(64) {
                println!("cargo:rustc-cfg=dispatch_packed");
            } else {
                println!("cargo:rustc-cfg=dispatch_unpacked");
            }
            if no_tco.is_none() && rustc_is_nightly() {
                println!("cargo:rustc-cfg=tco");
            }
        }
    }
}

enum DispatchBackend {
    Tco,
    Packed,
    SingleReturn,
    Unpacked,
}

fn dispatch_backend_override() -> Option<DispatchBackend> {
    let value = env("EVM2_DISPATCH_BACKEND")?;
    let value = value.to_str().expect("EVM2_DISPATCH_BACKEND must be valid UTF-8");
    match value {
        "" | "auto" => None,
        "tco" => Some(DispatchBackend::Tco),
        "packed" => Some(DispatchBackend::Packed),
        "single_return" | "single-return" => Some(DispatchBackend::SingleReturn),
        "unpacked" => Some(DispatchBackend::Unpacked),
        _ => panic!(
            "invalid EVM2_DISPATCH_BACKEND={value:?}; expected auto, tco, packed, single_return, or unpacked"
        ),
    }
}

fn link_mcl_cpp_stdlib() {
    if env("CARGO_FEATURE_BN254_MCL").is_none() || target_is_wasm() {
        return;
    }

    match target_os().as_deref() {
        Some("macos" | "ios") => println!("cargo:rustc-link-lib=c++"),
        Some("windows") => {}
        _ => println!("cargo:rustc-link-lib=stdc++"),
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

fn target_os() -> Option<String> {
    env("CARGO_CFG_TARGET_OS")?.into_string().ok()
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
    std::env::var_os(key)
}
