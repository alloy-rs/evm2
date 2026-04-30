#[cfg(feature = "nightly")]
macro_rules! extern_table {
    ($(#[$attr:meta])* fn $($f:tt)*) => {
        $(#[$attr])* extern "rust-preserve-none" fn $($f)*
    };
    ($(#[$attr:meta])* $vis:vis fn $($f:tt)*) => {
        $(#[$attr])* $vis extern "rust-preserve-none" fn $($f)*
    };
}
#[cfg(not(feature = "nightly"))]
macro_rules! extern_table {
    ($(#[$attr:meta])* fn $($f:tt)*) => {
        $(#[$attr])* fn $($f)*
    };
    ($(#[$attr:meta])* $vis:vis fn $($f:tt)*) => {
        $(#[$attr])* $vis fn $($f)*
    };
}
