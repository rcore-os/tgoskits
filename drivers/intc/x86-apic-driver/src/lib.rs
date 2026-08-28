//! OS-independent driver core for the x86 interrupt controllers.
//!
//! This crate covers the two x86 interrupt-controller devices the way
//! [`arm-gic-driver`] covers the Arm GIC: a `no_std` register-level driver
//! that any kernel or hypervisor can consume, with no OS crates in its
//! dependency set. Device discovery (ACPI MADT), `ioremap`, IRQ-domain
//! allocation, handler registration, and ack/eoi policy stay in the OS glue.
//!
//! Register operations are delegated to the mature [`x2apic`] crate
//! (`x2apic::lapic::LocalApic` for xAPIC MMIO / x2APIC MSR access and
//! `x2apic::ioapic::IoApic` for I/O APIC redirection-table programming)
//! instead of defining a third register layout. A small private raw-access
//! supplement in [`local_apic`] fills the few operations the `x2apic` public
//! API cannot express; each site documents the reason and the plan to remove
//! it once upstream grows the API.
//!
//! [`arm-gic-driver`]: https://crates.io/crates/arm-gic-driver
//! [`x2apic`]: https://crates.io/crates/x2apic
//!
//! # Capability layout
//!
//! - [`local_apic`]: per-CPU local APIC (bring-up, EOI, fixed/self IPIs, LVT
//!   timer programming including the TSC-deadline mode).
//! - [`ioapic`]: I/O APIC chips (redirection-table programming, masking).
//! - [`msi`]: LAPIC MSI address/data composition (pure function).
//! - With the `rdif` feature: [`rdif`] implements the [`rdif_intc`]
//!   `Interface` contract for the ACPI GSI domain so the driver can register
//!   into `rdrive` like `arm-gic-driver` does.
//!
//! SMP bring-up IPIs (INIT-SIPI-SIPI) are intentionally not part of this
//! driver: they are the x86 CPU-wake mechanism, owned by the boot layer just
//! like PSCI wake on aarch64, and their encodings live there.
//!
//! [`rdif_intc`]: rdif_intc

#![no_std]

#[cfg(all(target_arch = "x86_64", feature = "rdif"))]
extern crate alloc;

#[cfg(target_arch = "x86_64")]
pub mod ioapic;
#[cfg(target_arch = "x86_64")]
pub mod local_apic;
pub mod msi;
#[cfg(all(target_arch = "x86_64", feature = "rdif"))]
pub mod rdif;

#[cfg(target_arch = "x86_64")]
pub use local_apic::{ApicMode, LocalApicConfig, X86LocalApic};
#[cfg(all(target_arch = "x86_64", feature = "rdif"))]
pub use rdif::IoApicIntc;
#[cfg(target_arch = "x86_64")]
pub use x2apic::lapic::{TimerDivide, TimerMode};

/// A kernel virtual address of a memory-mapped device register page.
///
/// The driver never maps memory itself; the OS glue maps the device page and
/// passes the mapped address in, mirroring `arm-gic-driver`'s `VirtAddr`.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct VirtAddr(usize);

impl VirtAddr {
    /// Creates a virtual address from a raw value.
    pub const fn new(addr: usize) -> Self {
        Self(addr)
    }

    /// Returns the raw virtual address value.
    pub const fn as_usize(&self) -> usize {
        self.0
    }

    /// Returns the address as a mutable pointer to `T`.
    pub fn as_ptr<T>(&self) -> *mut T {
        self.0 as *mut T
    }
}

impl From<usize> for VirtAddr {
    fn from(addr: usize) -> Self {
        Self(addr)
    }
}

impl From<*mut u8> for VirtAddr {
    fn from(ptr: *mut u8) -> Self {
        Self(ptr as usize)
    }
}

/// Register-level failures reported by the x86 interrupt-controller driver.
///
/// The OS glue maps these onto its own error type (for example
/// `irq_framework::IrqError`) at the capability boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ApicError {
    /// The APIC id cannot be encoded in the 8-bit xAPIC destination field.
    #[error("APIC id {0:#x} exceeds the 8-bit xAPIC destination field")]
    XapicDestinationOverflow(u32),

    /// The local APIC ICR stayed busy past the delivery-wait budget.
    #[error("timed out waiting for local APIC IPI delivery")]
    IpiDeliveryTimeout,

    /// The local APIC did not retain the mask bit for both local interrupt pins.
    #[error(
        "local APIC interrupt pins remain unmasked after initialization (LINT0={lint0:#x}, \
         LINT1={lint1:#x})"
    )]
    LocalInterruptPinsUnmasked {
        /// LVT LINT0 value read back after masking.
        lint0: u32,
        /// LVT LINT1 value read back after masking.
        lint1: u32,
    },

    /// The I/O APIC input pin is outside this chip's redirection table.
    #[error("I/O APIC input {0} is outside the redirection table")]
    InvalidIoApicInput(u8),

    /// The CPU does not expose a usable local APIC.
    #[error("local APIC unsupported: {0}")]
    ApicUnsupported(&'static str),
}
