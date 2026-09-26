//! USB Video Class wire formats shared by kernel and direct USB drivers.
#![no_std]

extern crate alloc;

pub mod descriptors;
pub mod payload;
pub mod stream_control;
