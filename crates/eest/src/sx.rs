//! Superinstructions in test and block execution, for comparing them with regular dispatch.

use evm2::{
    BaseEvmTypes, EvmTypesHost, SpecId,
    evm::{InMemoryDB, config::EvmConfigSelector},
};
use std::sync::atomic::{AtomicU8, Ordering};

const UNSET: u8 = u8::MAX;
static ENABLED: AtomicU8 = AtomicU8::new(UNSET);

/// Returns whether execution uses superinstructions: as last set with [`set_enabled`], or else
/// whether `EVM2_SX` is set to anything but 0.
pub fn enabled() -> bool {
    match ENABLED.load(Ordering::Relaxed) {
        UNSET => {
            let enabled = env("EVM2_SX");
            ENABLED.store(enabled as u8, Ordering::Relaxed);
            enabled
        }
        enabled => enabled != 0,
    }
}

/// Sets whether execution uses superinstructions.
pub fn set_enabled(enabled: bool) {
    ENABLED.store(enabled as u8, Ordering::Relaxed);
}

/// Analyzes the code of `database` as execution with `spec` would on first use (translation to
/// superinstructions when they are on, else jump destination analysis), as a node with a code cache
/// has it.
pub fn prewarm(database: &InMemoryDB, spec: SpecId) {
    let mut config = <<BaseEvmTypes as EvmTypesHost>::ConfigSelector as EvmConfigSelector<
        BaseEvmTypes,
    >>::execution_config(spec);
    config.set_superinstructions(enabled());
    for code in database.cache.contracts.values() {
        config.analyze(code);
    }
    for info in database.cache.accounts.values().flatten() {
        if let Some(code) = &info.code {
            config.analyze(code);
        }
    }
}

/// [`prewarm`]s when `EVM2_PREWARM` is set to anything but 0.
pub(crate) fn prewarm_from_env(database: &InMemoryDB, spec: SpecId) {
    if env("EVM2_PREWARM") {
        prewarm(database, spec);
    }
}

fn env(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| v != "0")
}
