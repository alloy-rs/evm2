#!/usr/bin/env -S cargo -Zscript
---
[package]
edition = "2024"

[dependencies]
bindgen = "0.72"
---

fn main() {
    let bindings = bindgen::Builder::default()
        .header("include/evmc/evmc.h")
        .allowlist_function("evmc_.*")
        .allowlist_type("evmc_.*")
        .allowlist_var("EVMC_.*")
        .default_enum_style(bindgen::EnumVariation::Consts)
        .prepend_enum_name(false)
        .generate_comments(false)
        .layout_tests(false)
        .merge_extern_blocks(true)
        .generate()
        .expect("failed to generate EVMC bindings");

    bindings.write_to_file("src/ffi.rs").expect("failed to write EVMC bindings");
}
