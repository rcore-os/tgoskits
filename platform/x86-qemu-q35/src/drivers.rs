#[cfg(feature = "pci")]
use ax_drivers::pci;
#[cfg(feature = "pci")]
use rdrive::PlatformDevice;
#[cfg(feature = "pci")]
use rdrive::probe::OnProbeError;
use rdrive::{Platform, probe::static_::StaticDeviceDesc};

#[cfg(feature = "pci")]
use crate::config::devices;

mod registers;

static STATIC_DEVICES: &[StaticDeviceDesc] = &[
    #[cfg(feature = "pci")]
    StaticDeviceDesc::new(pci::DEVICE_NAME),
];

pub(crate) fn init() {
    rdrive::init(Platform::Static(STATIC_DEVICES))
        .unwrap_or_else(|err| panic!("failed to initialize static rdrive source: {err:?}"));
    registers::append_linker_registers();
    #[cfg(feature = "pci")]
    register_pcie();
    rdrive::probe_pre_kernel()
        .unwrap_or_else(|err| panic!("failed to run static pre-kernel probes: {err:?}"));
}

#[cfg(feature = "pci")]
fn register_pcie() {
    let ecam_size = (devices::PCI_BUS_END + 1) << 20;
    match pci::register_ecam_controller(
        static_descriptor(pci::DEVICE_NAME),
        devices::PCI_ECAM_BASE,
        ecam_size,
        None,
        None,
    ) {
        Ok(()) | Err(OnProbeError::NotMatch) => {}
        Err(err) => panic!("failed to register static PCIe controller: {err:?}"),
    }
}

#[cfg(feature = "pci")]
fn static_descriptor(name: &'static str) -> PlatformDevice {
    let mut descriptor = rdrive::Descriptor::new();
    descriptor.name = name;
    PlatformDevice { descriptor }
}
