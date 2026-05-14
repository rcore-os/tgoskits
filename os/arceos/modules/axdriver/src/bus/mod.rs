#[cfg(all(bus = "mmio", feature = "bus-mmio"))]
mod mmio;
#[cfg(all(bus = "pci", feature = "bus-pci"))]
mod pci;

#[cfg(not(any(bus = "mmio", bus = "pci")))]
impl crate::AllDevices {
    pub(crate) fn probe_bus_devices(&mut self) {}
}
