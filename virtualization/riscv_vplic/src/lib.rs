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
//! use riscv_vplic::VPlicGlobal;
//! use vm_interrupt::{InterruptControllerRoute, VmInterruptRouter};
//!
//! struct InterruptRouter;
//! impl VmInterruptRouter for InterruptRouter {
//!     fn route_interrupt(&self, _route: InterruptControllerRoute) -> AxResult {
//!         Ok(())
//!     }
//! }
//!
//! // Create a virtual PLIC with 2 contexts
//! let router: Arc<dyn VmInterruptRouter> = Arc::new(InterruptRouter);
//! let context_routes = alloc::vec![None, Some(0)];
//! let vplic = VPlicGlobal::new(
//!     GuestPhysAddr::from(0x0c000000),
//!     Some(0x4000),
//!     2,
//!     context_routes,
//!     router,
//! );
//! ```

#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod consts;
mod devops_impl;
mod utils;
mod vplic;

pub use consts::*;
pub use vplic::VPlicGlobal;
