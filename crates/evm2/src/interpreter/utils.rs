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

/// Emits an inline assembly comment.
#[macro_export]
#[collapse_debuginfo(yes)]
macro_rules! asm_comment {
    ($comment:literal $(,)?) => {
        #[cfg(any(target_arch = "aarch64", target_arch = "x86", target_arch = "x86_64"))]
        unsafe {
            core::arch::asm!(
                concat!("/* ", $comment, " */"),
                options(nomem, nostack, preserves_flags)
            );
        }
    };
}
