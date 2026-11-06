#![allow(missing_docs)]

fn main() {
    println!("cargo:rustc-check-cfg=cfg(evm2_bench_allocations)");
    #[cfg(feature = "jit")]
    evm2_jit_build::emit();
}
