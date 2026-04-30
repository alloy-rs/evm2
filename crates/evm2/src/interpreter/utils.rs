use core::hint::cold_path;

#[cfg(feature = "nightly")]
macro_rules! tail_return {
    ($e:expr) => {
        become $e;
    };
}
#[cfg(not(feature = "nightly"))]
macro_rules! tail_return {
    ($e:expr) => {
        return $e;
    };
}

#[cfg(feature = "nightly")]
macro_rules! extern_table {
    ($(#[$attr:meta])* fn $($f:tt)*) => {
        $(#[$attr])* extern "rust-preserve-none" fn $($f)*
    };
}
#[cfg(not(feature = "nightly"))]
macro_rules! extern_table {
    ($(#[$attr:meta])* fn $($f:tt)*) => {
        $(#[$attr])* fn $($f)*
    };
}

#[inline(always)]
pub(crate) fn likely(b: bool) -> bool {
    if b {
        true
    } else {
        cold_path();
        false
    }
}
