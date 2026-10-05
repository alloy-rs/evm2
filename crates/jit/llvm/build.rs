#![allow(missing_docs)]

fn main() {
    println!("cargo:rerun-if-changed=cpp/lib.cpp");

    let cxxflags = ["llvm-config", "llvm-config-22"]
        .into_iter()
        .find_map(detect_cxxflags)
        .expect("no llvm-config found");

    cc::Build::new()
        .cpp(true)
        .flags(cxxflags.split_whitespace())
        .flag("-w")
        .file("cpp/lib.cpp")
        .compile("evm2_jit_llvm_cpp");
}

/// Returns the `--cxxflags` reported by `llvm_config`, or `None` if it cannot be used.
///
/// [`Command::output`] only reports whether the process could be spawned, so a `llvm-config` that
/// exists but exits with a non-zero status still yields `Ok`. Checking the status keeps such a
/// broken binary from shadowing a working one later in the candidate list.
fn detect_cxxflags(llvm_config: &str) -> Option<String> {
    let output = std::process::Command::new(llvm_config)
        .arg("--cxxflags")
        .output()
        .inspect_err(|error| eprintln!("failed to run {llvm_config}: {error}"))
        .ok()?;

    let status = output.status;
    if !status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        if stderr.is_empty() {
            eprintln!("{llvm_config} --cxxflags failed with {status}");
        } else {
            eprintln!("{llvm_config} --cxxflags failed with {status}: {stderr}");
        }
        return None;
    }

    String::from_utf8(output.stdout)
        .inspect_err(|error| eprintln!("{llvm_config} --cxxflags is not valid UTF-8: {error}"))
        .ok()
}
