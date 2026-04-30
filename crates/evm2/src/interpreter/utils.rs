/// Marks a branch as unreachable in optimized builds while checking it in debug builds.
#[macro_export]
#[collapse_debuginfo(yes)]
macro_rules! debug_unreachable {
    ($($t:tt)*) => {
        if cfg!(debug_assertions) {
            unreachable!($($t)*);
        } else {
            unsafe { core::hint::unreachable_unchecked() };
        }
    };
}

/// Assumes a condition is true in optimized builds while asserting it in debug builds.
#[macro_export]
#[collapse_debuginfo(yes)]
macro_rules! assume {
    ($e:expr $(,)?) => {
        if !$e {
            debug_unreachable!(stringify!($e));
        }
    };

    ($e:expr, $($t:tt)+) => {
        if !$e {
            debug_unreachable!($($t)+);
        }
    };
}

#[cfg(feature = "nightly")]
#[collapse_debuginfo(yes)]
macro_rules! tail_return {
    ($e:expr) => {
        become $e;
    };
}
#[cfg(not(feature = "nightly"))]
#[collapse_debuginfo(yes)]
macro_rules! tail_return {
    ($e:expr) => {
        return $e;
    };
}

#[cfg(feature = "nightly")]
#[collapse_debuginfo(yes)]
macro_rules! extern_table {
    ($(#[$attr:meta])* fn $($f:tt)*) => {
        $(#[$attr])* extern "rust-preserve-none" fn $($f)*
    };
    ($(#[$attr:meta])* $vis:vis fn $($f:tt)*) => {
        $(#[$attr])* $vis extern "rust-preserve-none" fn $($f)*
    };
}
#[cfg(not(feature = "nightly"))]
#[collapse_debuginfo(yes)]
macro_rules! extern_table {
    ($(#[$attr:meta])* fn $($f:tt)*) => {
        $(#[$attr])* fn $($f)*
    };
    ($(#[$attr:meta])* $vis:vis fn $($f:tt)*) => {
        $(#[$attr])* $vis fn $($f)*
    };
}
