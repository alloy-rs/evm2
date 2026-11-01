use evm2::SpecId;
use std::{env, sync::OnceLock};

/// Environment variable listing hardforks to skip, comma-separated by name.
pub(crate) const SKIP_FORKS_ENV: &str = "EVM2_SKIP_FORKS";

/// Returns whether all test cases targeting `spec` should be skipped.
#[inline]
pub(crate) fn is_fork_skipped(spec: SpecId) -> bool {
    skipped_forks().contains(&spec)
}

/// Returns the hardforks named in [`SKIP_FORKS_ENV`].
fn skipped_forks() -> &'static [SpecId] {
    static SKIPPED: OnceLock<Vec<SpecId>> = OnceLock::new();
    SKIPPED.get_or_init(|| {
        let mut forks = Vec::new();
        if let Ok(names) = env::var(SKIP_FORKS_ENV) {
            for name in names.split(',').map(str::trim).filter(|name| !name.is_empty()) {
                let Ok(spec) = name.parse() else {
                    panic!("{SKIP_FORKS_ENV}: unknown hardfork name `{name}`");
                };
                forks.push(spec);
            }
        }
        forks
    })
}
