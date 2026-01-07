//! Architecture-specific APIs.

use super::{memory::PhysAddr, vmm::InterruptVector};

/// The API trait for architecture-specific functionalities.
#[crate::api_def]
pub trait ArchIf {
    /// Inject a virtual interrupt to the current virtual CPU.
    #[cfg(target_arch = "aarch64")]
    fn hardware_inject_virtual_interrupt(vector: InterruptVector);

    /// Get the TYPER register of the GIC distributor. Used in virtual GIC initialization.
    #[cfg(target_arch = "aarch64")]
    fn read_vgicd_typer() -> u32;
    /// Get the IIDR register of the GIC distributor. Used in virtual GIC initialization.
    #[cfg(target_arch = "aarch64")]
    fn read_vgicd_iidr() -> u32;

    /// Get the base address of the GIC distributor in the host system.
    #[cfg(target_arch = "aarch64")]
    fn get_host_gicd_base() -> PhysAddr;
    /// Get the base address of the GIC redistributor in the host system.
    #[cfg(target_arch = "aarch64")]
    fn get_host_gicr_base() -> PhysAddr;
}
