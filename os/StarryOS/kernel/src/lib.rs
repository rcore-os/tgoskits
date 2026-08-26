//! The core functionality of a monolithic kernel, including loading user
//! programs and managing processes.

#![no_std]
#![cfg_attr(not(axtest), feature(likely_unlikely))]
#![cfg_attr(not(axtest), feature(c_variadic))]
#![allow(missing_docs)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]

// The freestanding axtest target includes `root.rs` directly so that module-local
// tests and kernel entry symbols are emitted exactly once. Keep Cargo's implicit
// library dependency empty for that target; linking a second kernel instance
// would duplicate trap handlers, allocators, and runtime state.
#[cfg(not(axtest))]
include!("root.rs");
