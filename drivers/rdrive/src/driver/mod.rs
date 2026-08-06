use pcie::PcieController;
pub use rdif_base::DriverGeneric;

use crate::{Descriptor, error::FdtChildProviderError, probe::fdt::FdtChild};

pub struct Empty;

impl DriverGeneric for Empty {
    fn name(&self) -> &str {
        "Empty Driver"
    }
}

pub struct PlatformDevice {
    pub descriptor: Descriptor,
}

impl PlatformDevice {
    pub(crate) fn new(descriptor: Descriptor) -> Self {
        Self { descriptor }
    }

    pub fn descriptor(&self) -> &Descriptor {
        &self.descriptor
    }

    /// Register a device to the driver manager.
    ///
    /// # Panics
    /// This method will panic if the device with the same ID is already added
    pub fn register<T: DriverGeneric>(&self, driver: T) {
        crate::edit(|manager| {
            manager
                .dev_container
                .insert(self.descriptor.clone(), driver);
        });
    }

    /// Publishes a capability on an available direct FDT child.
    ///
    /// The child handle must have been prepared from the [`crate::register::FdtInfo`]
    /// associated with this platform device. Rdrive validates ownership and
    /// publishes the child's own firmware identity and lifecycle state.
    ///
    /// # Errors
    ///
    /// Returns a typed error for invalid ownership, an unavailable child, or a
    /// duplicate capability. The registry is unchanged on error.
    pub fn register_fdt_child<T: DriverGeneric>(
        &self,
        child: FdtChild,
        driver: T,
    ) -> Result<(), FdtChildProviderError> {
        crate::probe::fdt::commit_child_provider(self, child, driver)
    }

    /// Atomically publishes the parent transport and one FDT child capability.
    ///
    /// All ownership and duplicate checks run before either capability becomes
    /// visible, so callers can safely retry a failed probe.
    pub fn register_with_fdt_child<P: DriverGeneric, C: DriverGeneric>(
        &self,
        parent_driver: P,
        child: FdtChild,
        child_driver: C,
    ) -> Result<(), FdtChildProviderError> {
        crate::probe::fdt::commit_parent_and_child(self, parent_driver, child, child_driver)
    }

    pub fn register_pcie(&self, drv: PcieController) {
        crate::edit(|manager| {
            manager.dev_container.insert(self.descriptor.clone(), drv);
        });
    }
}
