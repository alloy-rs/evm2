//! evm2 [Inspector](evm2::Inspector) implementations, such as call tracers
#![doc = include_str!("../README.md")]
#![doc(
    html_logo_url = "https://raw.githubusercontent.com/paradigmxyz/reth/main/assets/reth-docs.png",
    html_favicon_url = "https://avatars0.githubusercontent.com/u/97369466?s=256",
    issue_tracker_base_url = "https://github.com/paradigmxyz/reth/issues/"
)]
#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![deny(unused_must_use, rust_2018_idioms)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

#[cfg(feature = "std")]
use anstyle as _;
#[cfg(feature = "js-tracer")]
use boa_engine as _;
#[cfg(feature = "js-tracer")]
use boa_gc as _;
#[cfg(feature = "serde")]
use serde as _;
use serde_json as _;
use thiserror as _;

pub mod access_list;

/// An inspector for tracking edge coverage.
pub mod edge_cov;

/// Implementation of an opcode counter for the EVM.
pub mod opcode;
pub use opcode::{OpcodeGasInspector, immediate_size};

pub mod tracing;

pub mod transfer;

pub mod storage;

pub use colorchoice::ColorChoice;
