//! Utility functions used by the `evm2` CLI.

#[cfg(feature = "mimalloc")]
use mimalloc as _;
#[cfg(all(feature = "jemalloc", unix))]
use tikv_jemallocator as _;

// Keep the system allocator in tests, where we spawn a ton of processes and any extra startup cost
// slows down tests massively.
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
