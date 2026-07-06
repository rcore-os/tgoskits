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
//! extern crate alloc;
//!
//! use alloc::sync::Arc;
//!
//! use ax_errno::AxResult;
//! use axaddrspace::GuestPhysAddr;
//! use axdevice_base::{InterruptLineLevel, VcpuInterrupt, VmInterruptSink};
//! use riscv_vplic::VPlicGlobal;
//!
//! struct InterruptSink;
//! impl VmInterruptSink for InterruptSink {
//!     fn set_vcpu_interrupt(
//!         &self,
//!         _interrupt: VcpuInterrupt,
//!         _level: InterruptLineLevel,
//!     ) -> AxResult {
//!         Ok(())
//!     }
//! }
//!
//! // Create a virtual PLIC with 2 contexts
//! let sink: Arc<dyn VmInterruptSink> = Arc::new(InterruptSink);
//! let vplic = VPlicGlobal::new(GuestPhysAddr::from(0x0c000000), Some(0x4000), 2, sink);
//! ```

#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod consts;
mod devops_impl;
mod utils;
mod vplic;

pub use consts::*;
pub use vplic::VPlicGlobal;
