#![no_std]
#![no_main]
#![feature(likely_unlikely)]
#![allow(missing_docs)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use ax_hal as _;
use ax_runtime as _;
use ax_std as _;

include!("root.rs");

#[axtest::tests]
mod tests {}
