//! The core functionality of a monolithic kernel, including loading user
//! programs and managing processes.

#![no_std]
#![feature(likely_unlikely)]
#![allow(missing_docs)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]

include!("root.rs");
