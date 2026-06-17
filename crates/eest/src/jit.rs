use evm2_jit_runtime::{
    OptimizationLevel,
    runtime::{ArtifactStore, JitBackend, RuntimeArtifactStore, RuntimeConfig, RuntimeTuning},
};
use std::sync::Arc;

pub(crate) fn make_backend(aot: bool) -> Result<JitBackend, String> {
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
            jit_worker_count: 1,
            jit_opt_level: OptimizationLevel::None,
            aot_opt_level: OptimizationLevel::None,
            ..Default::default()
        },
        ..Default::default()
    })
    .map_err(|err| err.to_string())
}
