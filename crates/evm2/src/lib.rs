#![doc = include_str!("../README.md")]
#![cfg_attr(
    feature = "nightly",
    feature(explicit_tail_calls, rust_preserve_none_cc),
    allow(incomplete_features)
)]
#![allow(clippy::missing_safety_doc)]
#![allow(clippy::missing_const_for_fn)]
#![allow(missing_debug_implementations)]
#![allow(missing_docs)]
#![allow(unreachable_pub)]
#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

pub mod bytecode;
pub mod interpreter;

mod once_lock;
