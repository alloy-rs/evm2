use evm2_jit_runtime::{
    OptimizationLevel,
    runtime::{ArtifactStore, JitBackend, RuntimeArtifactStore, RuntimeConfig, RuntimeTuning},
};
use std::sync::Arc;

pub(crate) fn make_backend(aot: bool) -> Result<JitBackend, String> {
    let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
    let store = if aot {
        Some(Arc::new(RuntimeArtifactStore::new().map_err(|err| err.to_string())?)
            as Arc<dyn ArtifactStore>)
    } else {
        None
    };
    JitBackend::new(RuntimeConfig {
        enabled: true,
        blocking: true,
        aot,
        store,
        tuning: RuntimeTuning {
            jit_hot_threshold: 0,
            jit_worker_count: cpus,
            jit_opt_level: OptimizationLevel::None,
            aot_opt_level: OptimizationLevel::None,
            // Every resident program holds memory mappings, and the full corpus compiles more
            // distinct programs than a default `vm.max_map_count` allows. A small budget evicts old
            // programs.
            resident_code_cache_bytes: 8 * 1024 * 1024,
            ..Default::default()
        },
        ..Default::default()
    })
    .map_err(|err| err.to_string())
}
