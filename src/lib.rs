#![no_std]

//! This module is designed for an environment where the standard library is not available (`no_std`).
//!
//! The `alloc` crate is used to enable dynamic memory allocation in the absence of the standard library.
//!
//! The `log` crate is included for logging purposes, with macros being imported globally.
//!
//! The module is structured into two main parts: `config` and `device`, which manage the configuration and handling of AxVm devices respectively.

extern crate alloc;
#[macro_use]
extern crate log;

mod config;
mod device;

pub use config::AxVmDeviceConfig;
pub use device::AxVmDevices;
