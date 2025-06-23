//! Common peripheral drivers for AArch64 platforms.
//!
//! It includes:
//!
//! - PL011 UART driver.
//! - PL031 Real Time Clock (RTC) driver.
//! - GICv2 (Generic Interrupt Controller) driver.
//! - Generic Timer related functions.
//! - PSCI (Power State Coordination Interface) calls.

#![no_std]

#[macro_use]
extern crate log;

pub mod generic_timer;
pub mod gic;
pub mod pl011;
pub mod pl031;
pub mod psci;
