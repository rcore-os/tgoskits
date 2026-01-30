//! Architecture-specific APIs for the AxVisor hypervisor.
//!
//! This module provides APIs that are specific to certain CPU architectures.
//! Currently, it mainly contains AArch64 GIC (Generic Interrupt Controller)
//! related operations.
//!
//! # Supported Architectures
//!
//! - **AArch64**: GIC distributor and redistributor operations for virtual
//!   interrupt injection and GIC initialization.
//!
//! # Usage
//!
//! The API functions in this module are conditionally compiled based on the
//! target architecture. On non-AArch64 platforms, this module is essentially
//! empty.
//!
//! # Implementation
//!
//! To implement these APIs, use the [`api_impl`](crate::api_impl) attribute
//! macro on an impl block:
//!
//! ```rust,ignore
//! struct ArchIfImpl;
//!
//! #[axvisor_api::api_impl]
//! impl axvisor_api::arch::ArchIf for ArchIfImpl {
//!     // Implement the required functions...
//! }
//! ```

use super::{memory::PhysAddr, vmm::InterruptVector};

/// The API trait for architecture-specific functionalities.
///
/// This trait defines the interface for architecture-specific operations
/// required by the hypervisor. Implementations should be provided by the
/// host system or HAL layer.
#[crate::api_def]
pub trait ArchIf {
    /// Inject a virtual interrupt to the current virtual CPU using hardware
    /// virtualization support.
    ///
    /// This function uses the GIC virtualization interface to directly inject
    /// an interrupt into the guest without causing a VM exit.
    ///
    /// # Arguments
    ///
    /// * `vector` - The interrupt vector number to inject.
    ///
    /// # Platform Support
    ///
    /// This function is only available on AArch64 platforms with GICv2/v3
    /// virtualization extensions.
    #[cfg(target_arch = "aarch64")]
    fn hardware_inject_virtual_interrupt(vector: InterruptVector);

    /// Read the TYPER (Type Register) of the GIC distributor.
    ///
    /// The TYPER register provides information about the GIC implementation,
    /// including the maximum number of SPIs supported and the number of
    /// implemented CPU interfaces.
    ///
    /// # Returns
    ///
    /// The 32-bit value of the GICD_TYPER register.
    ///
    /// # Platform Support
    ///
    /// This function is only available on AArch64 platforms.
    #[cfg(target_arch = "aarch64")]
    fn read_vgicd_typer() -> u32;

    /// Read the IIDR (Implementer Identification Register) of the GIC
    /// distributor.
    ///
    /// The IIDR register provides identification information about the GIC
    /// implementation, including the implementer, revision, and variant.
    ///
    /// # Returns
    ///
    /// The 32-bit value of the GICD_IIDR register.
    ///
    /// # Platform Support
    ///
    /// This function is only available on AArch64 platforms.
    #[cfg(target_arch = "aarch64")]
    fn read_vgicd_iidr() -> u32;

    /// Get the base physical address of the GIC distributor in the host system.
    ///
    /// The GIC distributor is responsible for interrupt prioritization and
    /// distribution to CPU interfaces.
    ///
    /// # Returns
    ///
    /// The physical address of the GICD (GIC Distributor) registers.
    ///
    /// # Platform Support
    ///
    /// This function is only available on AArch64 platforms.
    #[cfg(target_arch = "aarch64")]
    fn get_host_gicd_base() -> PhysAddr;

    /// Get the base physical address of the GIC redistributor in the host
    /// system.
    ///
    /// The GIC redistributor (GICv3+) handles per-PE (Processing Element)
    /// interrupt configuration and provides the LPI (Locality-specific
    /// Peripheral Interrupt) configuration interface.
    ///
    /// # Returns
    ///
    /// The physical address of the GICR (GIC Redistributor) registers.
    ///
    /// # Platform Support
    ///
    /// This function is only available on AArch64 platforms with GICv3 or
    /// later.
    #[cfg(target_arch = "aarch64")]
    fn get_host_gicr_base() -> PhysAddr;
}
