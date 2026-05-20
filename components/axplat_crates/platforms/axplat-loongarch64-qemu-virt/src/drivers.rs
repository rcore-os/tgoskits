use ax_drivers::pci;
use rdrive::{
    Platform, PlatformDevice,
    probe::{
        OnProbeError,
        pci::{PciMem32, PciMem64},
        static_::StaticDeviceDesc,
    },
};

use crate::config::devices;

mod registers;

static STATIC_DEVICES: &[StaticDeviceDesc] = &[StaticDeviceDesc::new(pci::DEVICE_NAME)];

pub(super) fn init() {
    rdrive::init(Platform::Static(STATIC_DEVICES))
        .unwrap_or_else(|err| panic!("failed to initialize static rdrive source: {err:?}"));
    registers::append_linker_registers();
    register_pcie();
    rdrive::probe_pre_kernel()
        .unwrap_or_else(|err| panic!("failed to run static pre-kernel probes: {err:?}"));
}

fn register_pcie() {
    let ecam_size = (devices::PCI_BUS_END + 1) << 20;
    let mem32 = pci_mem32_from_config();
    let mem64 = pci_mem64_from_config();
    match pci::register_ecam_controller(
        static_descriptor(pci::DEVICE_NAME),
        devices::PCI_ECAM_BASE,
        ecam_size,
        mem32,
        mem64,
    ) {
        Ok(()) | Err(OnProbeError::NotMatch) => {}
        Err(err) => panic!("failed to register static PCIe controller: {err:?}"),
    }
}

fn pci_mem32_from_config() -> Option<PciMem32> {
    let (address, size) = devices::PCI_RANGES.get(1).copied()?;
    if size == 0 {
        return None;
    }
    Some(PciMem32 {
        address: u32::try_from(address).ok()?,
        size: u32::try_from(size).ok()?,
    })
}

fn pci_mem64_from_config() -> Option<PciMem64> {
    let (address, size) = devices::PCI_RANGES.get(2).copied()?;
    if size == 0 || usize::BITS <= 32 {
        return None;
    }
    Some(PciMem64 {
        address: address as u64,
        size: size as u64,
    })
}

fn static_descriptor(name: &'static str) -> PlatformDevice {
    let mut descriptor = rdrive::Descriptor::new();
    descriptor.name = name;
    PlatformDevice { descriptor }
}
