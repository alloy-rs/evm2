//! Superinstructions in test execution: with `EVM2_SX` set to anything but 0, tests run with the
//! pre-state's code translated before they start.

use evm2::{
    BaseEvmTypes, EvmTypesHost, SpecId,
    evm::{InMemoryDB, config::EvmConfigSelector},
};

/// Returns whether tests run with superinstructions.
pub(crate) fn enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("EVM2_SX").is_some_and(|v| v != "0"))
}

/// Translates the code of `database` for `spec` if tests run with superinstructions, which would
/// otherwise translate only code that runs often.
pub(crate) fn translate(database: &InMemoryDB, spec: SpecId) {
    if !enabled() {
        return;
    }
    let mut config = <<BaseEvmTypes as EvmTypesHost>::ConfigSelector as EvmConfigSelector<
        BaseEvmTypes,
    >>::execution_config(spec);
    config.set_superinstructions(true);
    let accounts = database.cache.accounts.values().flatten().filter_map(|info| info.code.as_ref());
    for code in database.cache.contracts.values().chain(accounts) {
        config.analyze(code);
    }
}
