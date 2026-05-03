//! EVM precompile support.

use alloc::boxed::Box;

pub use evm2_precompiles::*;

/// Global crypto provider instance.
static CRYPTO: evm2_precompiles::OnceLock<Box<dyn evm2_precompiles::Crypto>> =
    evm2_precompiles::OnceLock::new();

/// Install a custom crypto provider globally.
pub fn install_crypto<C: evm2_precompiles::Crypto + 'static>(crypto: C) -> bool {
    CRYPTO.set(Box::new(crypto)).is_ok()
}

/// Get the installed crypto provider, or the default if none is installed.
pub fn crypto() -> &'static dyn evm2_precompiles::Crypto {
    CRYPTO.get_or_init(|| Box::new(evm2_precompiles::DefaultCrypto)).as_ref()
}
