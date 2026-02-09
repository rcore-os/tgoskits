//! # RISC-V Virtual Platform-Level Interrupt Controller
//!
//! This crate provides a virtual PLIC implementation for RISC-V hypervisors.
//! It emulates the PLIC 1.0.0 memory map and supports interrupt management for guest VMs.
//!
//! ## Main Features
//! - PLIC 1.0.0 compliant memory map
//! - Interrupt priority, pending, and enable management
//! - Context-based interrupt handling with claim/complete mechanism
//! - Integration with the hypervisor's device emulation framework
//!
//! ## Basic Usage
//! ```rust,no_run
//! use riscv_vplic::VPlicGlobal;
//! use axaddrspace::GuestPhysAddr;
//!
//! // Create a virtual PLIC with 2 contexts
//! let vplic = VPlicGlobal::new(
//!     GuestPhysAddr::from(0x0c000000),
//!     Some(0x4000),
//!     2
//! );
//! ```

#![cfg_attr(not(test), no_std)]

mod consts;
mod devops_impl;
mod utils;
mod vplic;

pub use consts::*;
pub use vplic::VPlicGlobal;
