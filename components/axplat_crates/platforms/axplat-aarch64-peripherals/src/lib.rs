#![no_std]
#![doc = include_str!("../README.md")]

#[cfg(target_arch = "aarch64")]
#[macro_use]
extern crate log;

#[cfg(target_arch = "aarch64")]
pub mod generic_timer;

#[cfg(feature = "irq")]
#[cfg(target_arch = "aarch64")]
pub mod gic;
#[cfg(target_arch = "aarch64")]
pub mod pl011;
#[cfg(target_arch = "aarch64")]
pub mod pl031;
#[cfg(target_arch = "aarch64")]
pub mod psci;
