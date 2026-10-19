//! Utility functions used by the `evm2` CLI.

// Debug-assertion builds keep the system allocator, as solar-cli does.
std::cfg_select! {
    debug_assertions => {
        type AllocatorInner = std::alloc::System;
    }
    feature = "mimalloc" => {
        type AllocatorInner = mimalloc::MiMalloc;
    }
    all(feature = "jemalloc", unix) => {
        type AllocatorInner = tikv_jemallocator::Jemalloc;
    }
    _ => {
        type AllocatorInner = std::alloc::System;
    }
}

/// The global allocator used by the `evm2` binary.
///
/// The library and its benches keep the system allocator.
pub(crate) type Allocator = AllocatorInner;

/// Creates a new instance of the global allocator.
pub(crate) const fn new_allocator() -> Allocator {
    AllocatorInner {}
}
