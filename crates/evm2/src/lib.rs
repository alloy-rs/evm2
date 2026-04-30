#![cfg_attr(feature = "nightly", feature(rust_preserve_none_cc), allow(incomplete_features))]
#![allow(clippy::missing_safety_doc)]
#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

pub mod interpreter;
