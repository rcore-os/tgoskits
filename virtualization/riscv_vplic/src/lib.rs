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
//! use axvm_types::GuestPhysAddr;
//! use riscv_vplic::VPlicGlobal;
//!
//! // Create a virtual PLIC with 2 contexts
//! let vplic = VPlicGlobal::new(GuestPhysAddr::from(0x0c000000), Some(0x4000), 2)?;
//! # Ok::<(), riscv_vplic::VplicError>(())
//! ```

#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod consts;
mod devops_impl;
mod error;
mod vplic;

pub use consts::*;
pub use error::{ForwardedBatchError, VplicError, VplicResult};
pub use vplic::VPlicGlobal;

#[cfg(test)]
mod test_lock_runtime {
    use ax_kspin::{LockRuntime, LockdepEvent, impl_trait};

    struct TestLockRuntime;

    impl_trait! {
        impl LockRuntime for TestLockRuntime {
            fn irq_enter() {}
            fn irq_exit() {}
            fn preempt_enter() {}
            fn preempt_exit() {}
            unsafe fn preempt_exit_irq_return() {}
            fn current_thread_id() -> u64 { 1 }
            fn lockdep_acquire(_event: LockdepEvent) {}
            fn lockdep_release(_event: LockdepEvent) {}
            fn lockdep_set_trace_enabled(_enabled: bool) {}
            fn lockdep_dump_trace() {}
        }
    }
}
