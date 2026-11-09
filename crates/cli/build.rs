#![allow(missing_docs)]

#[cfg(feature = "jit")]
fn main() {
    evm2_jit_build::emit();
}

#[cfg(not(feature = "jit"))]
fn main() {}
