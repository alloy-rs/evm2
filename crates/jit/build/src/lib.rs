#![doc = include_str!("../README.md")]
#![cfg_attr(not(test), warn(unused_extern_crates))]
#![cfg_attr(docsrs, feature(doc_cfg))]

// Must be kept in sync with `evm2-jit-builtins`.
const MANGLE_PREFIX: &str = "__evm2_jit_builtin_";

/// Emits the linker flag to export all the necessary symbols.
pub fn emit() {
    let target_vendor = std::env::var("CARGO_CFG_TARGET_VENDOR").unwrap();
    println!("cargo:rustc-link-arg={}", link_arg(&target_vendor));
}

fn link_arg(target_vendor: &str) -> String {
    if target_vendor == "apple" {
        // Mach-O C symbols have a leading `_`, so `__evm2_jit_builtin_*` becomes
        // `___evm2_jit_builtin_*`.
        format!("-Wl,-exported_symbol,_{MANGLE_PREFIX}*")
    } else {
        format!("-Wl,--export-dynamic-symbol,{MANGLE_PREFIX}*")
    }
}

#[cfg(test)]
mod tests {
    use super::{emit, link_arg};

    #[test]
    fn emits() {
        // SAFETY: These tests do not spawn threads or read this environment variable concurrently.
        unsafe { std::env::set_var("CARGO_CFG_TARGET_VENDOR", "unknown") };
        emit();
    }

    #[test]
    fn emits_macho_export_for_apple_targets() {
        assert_eq!(link_arg("apple"), "-Wl,-exported_symbol,___evm2_jit_builtin_*");
    }

    #[test]
    fn emits_elf_export_for_other_targets() {
        assert_eq!(link_arg("unknown"), "-Wl,--export-dynamic-symbol,__evm2_jit_builtin_*");
    }
}
