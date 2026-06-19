//! Configures the optional tail-call interpreter backend.

use std::{ffi::OsString, process::Command};

fn main() {
    for cfg in ["dispatch_packed", "dispatch_single_return", "dispatch_unpacked", "tco"] {
        println!("cargo:rustc-check-cfg=cfg({cfg})");
    }
    println!("cargo:rerun-if-changed=build.rs");

    // Select interpreter backend.
    let is_wasm = target_is_wasm();
    let target_pointer_width = target_pointer_width();
    let no_tco = env("CARGO_FEATURE_NO_TCO");
    match DispatchBackend::load().resolve(is_wasm, target_pointer_width, no_tco.is_some()) {
        DispatchBackend::Auto => unreachable!("auto backend must resolve to a concrete backend"),
        DispatchBackend::Tco => println!("cargo:rustc-cfg=tco"),
        DispatchBackend::Packed => println!("cargo:rustc-cfg=dispatch_packed"),
        DispatchBackend::SingleReturn => println!("cargo:rustc-cfg=dispatch_single_return"),
        DispatchBackend::Unpacked => println!("cargo:rustc-cfg=dispatch_unpacked"),
    }
}

enum DispatchBackend {
    Auto,
    Tco,
    Packed,
    SingleReturn,
    Unpacked,
}

impl DispatchBackend {
    fn load() -> Self {
        let Some(value) = env("EVM2_DISPATCH_BACKEND") else {
            return Self::Auto;
        };
        let value = value.to_str().expect("EVM2_DISPATCH_BACKEND must be valid UTF-8");
        match value {
            "" | "auto" => Self::Auto,
            "tco" => Self::Tco,
            "packed" => Self::Packed,
            "single_return" | "single-return" => Self::SingleReturn,
            "unpacked" => Self::Unpacked,
            _ => panic!(
                "invalid EVM2_DISPATCH_BACKEND={value:?}; expected auto, tco, packed, single_return, or unpacked"
            ),
        }
    }

    fn resolve(self, is_wasm: bool, target_pointer_width: Option<u32>, no_tco: bool) -> Self {
        match self {
            Self::Auto => {
                if !no_tco && rustc_is_nightly() {
                    Self::Tco
                } else if is_wasm {
                    Self::SingleReturn
                } else if target_pointer_width == Some(64) {
                    Self::Packed
                } else {
                    Self::Unpacked
                }
            }
            concrete => concrete,
        }
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
    std::env::var_os(key)
}
