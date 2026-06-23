use evm2::SpecId;
use std::{env, sync::OnceLock};

/// Environment variable listing extra hardforks to skip, comma-separated by name.
pub(crate) const SKIP_FORKS_ENV: &str = "EVM2_SKIP_FORKS";

/// Hardforks skipped by default because evm2 does not support them yet.
const UNSUPPORTED_FORKS: &[SpecId] = &[];

/// Returns whether all test cases targeting `spec` should be skipped.
#[inline]
pub(crate) fn is_fork_skipped(spec: SpecId) -> bool {
    skipped_forks().contains(&spec)
}

/// Returns the skipped hardforks, combining [`UNSUPPORTED_FORKS`] with any
/// forks named in [`SKIP_FORKS_ENV`].
fn skipped_forks() -> &'static [SpecId] {
    static SKIPPED: OnceLock<Vec<SpecId>> = OnceLock::new();
    SKIPPED.get_or_init(|| {
        let mut forks = UNSUPPORTED_FORKS.to_vec();
        if let Ok(names) = env::var(SKIP_FORKS_ENV) {
            for name in names.split(',').map(str::trim).filter(|name| !name.is_empty()) {
                let Some(spec) = parse_fork(name) else {
                    panic!("{SKIP_FORKS_ENV}: unknown hardfork name `{name}`");
                };
                forks.push(spec);
            }
        }
        forks
    })
}

/// Parses a case-insensitive hardfork name into a spec ID.
fn parse_fork(name: &str) -> Option<SpecId> {
    macro_rules! parse {
        ([$name:ident] $($spec:ident $lower:ident,)*) => {
            match $name.to_ascii_lowercase().as_str() {
                $(stringify!($lower) => Some(SpecId::$spec),)*
                _ => None,
            }
        };
    }
    evm2::for_each_spec!([name] parse)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fork_names() {
        assert_eq!(parse_fork("amsterdam"), Some(SpecId::AMSTERDAM));
        assert_eq!(parse_fork("Osaka"), Some(SpecId::OSAKA));
        assert_eq!(parse_fork("SPURIOUS_DRAGON"), Some(SpecId::SPURIOUS_DRAGON));
        assert_eq!(parse_fork("atlantis"), None);
    }

    #[test]
    fn unsupported_forks_are_skipped() {
        // Absent `EVM2_SKIP_FORKS`, a fork is skipped iff it is in `UNSUPPORTED_FORKS`.
        // The list is currently empty (evm2 targets all forks, including Amsterdam).
        for spec in [SpecId::AMSTERDAM, SpecId::OSAKA, SpecId::FRONTIER] {
            assert_eq!(is_fork_skipped(spec), UNSUPPORTED_FORKS.contains(&spec));
        }
    }
}
