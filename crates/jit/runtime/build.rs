#![allow(missing_docs)]

fn main() {
    #[cfg(feature = "llvm")]
    evm2_jit_build::emit();
}
