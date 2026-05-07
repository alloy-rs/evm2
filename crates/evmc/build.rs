//! Generates Rust bindings for the vendored EVMC header.

use std::{env, path::PathBuf};

fn main() {
    let bindings = bindgen::Builder::default()
        .header("include/evmc/evmc.h")
        .allowlist_function("evmc_.*")
        .allowlist_type("evmc_.*")
        .allowlist_var("EVMC_.*")
        .rustified_enum("evmc_.*")
        .generate_comments(false)
        .layout_tests(false)
        .merge_extern_blocks(true)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .generate()
        .expect("failed to generate EVMC bindings");

    let out_path = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR is set"));
    bindings.write_to_file(out_path.join("evmc.rs")).expect("failed to write EVMC bindings");
}
