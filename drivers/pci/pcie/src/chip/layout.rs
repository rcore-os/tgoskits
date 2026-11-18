use crate::PciAddress;

/// Register addressing within a PCI host's configuration aperture.
///
/// The layout describes the controller, not the CPU architecture: virtual
/// machines and physical systems can use different layouts on the same CPU.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ConfigSpaceLayout {
    /// PCI Express Enhanced Configuration Access Mechanism.
    #[default]
    Ecam,
    /// Loongson CFG1, including its Type 1 and extended-register selectors.
    Loongson,
}

impl ConfigSpaceLayout {
    /// Returns a byte offset from the controller's bus-zero configuration base.
    /// Hidden functions and registers outside the PCIe 4 KiB space are absent.
    pub fn register_offset(self, address: PciAddress, register: u16) -> Option<usize> {
        if register >= 4096 {
            return None;
        }
        let bus = usize::from(address.bus());
        let device = usize::from(address.device());
        let function = usize::from(address.function());
        let register = usize::from(register);
        match self {
            Self::Ecam => Some(bus << 20 | device << 15 | function << 12 | register),
            Self::Loongson => {
                // LS7A root ports alias nonexistent functions. Linux's
                // pci-loongson controller excludes these before any access.
                if bus == 0 && (9..=20).contains(&device) && function != 0 {
                    return None;
                }
                let type1 = if bus == 0 { 0 } else { 1 << 28 | bus << 16 };
                Some(
                    type1
                        | device << 11
                        | function << 8
                        | (register & 0xff)
                        | (register & 0xf00) << 16,
                )
            }
        }
    }

    /// Extent covering every register through the given controller-local bus.
    pub const fn window_size(self, last_bus: u8) -> usize {
        match self {
            Self::Ecam => (last_bus as usize + 1) << 20,
            Self::Loongson => (0x1f00_0000 | (last_bus as usize) << 16) + 0x1_0000,
        }
    }
}
