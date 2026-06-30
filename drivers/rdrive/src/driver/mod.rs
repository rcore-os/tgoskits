use pcie::PcieController;
pub use rdif_base::DriverGeneric;

use crate::Descriptor;

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
    pub fn register<T: DriverGeneric>(self, driver: T) {
        crate::edit(|manager| {
            manager.dev_container.insert(self.descriptor, driver);
        });
    }

    pub fn register_pcie(self, drv: PcieController) {
        crate::edit(|manager| {
            manager.dev_container.insert(self.descriptor, drv);
        });
    }
}
