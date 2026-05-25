use rdrive::{PlatformDevice, probe::OnProbeError};

use crate::config::devices;

const PCI_ECAM_SIZE: usize = (devices::PCI_BUS_END + 1) << 20;

rdrive::module_driver!(
    name: "Static PCIe ECAM",
    level: ProbeLevel::PreKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Static {
        on_probe: probe,
    }],
);

fn probe(plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    ax_driver::pci::register_ecam_controller(
        plat_dev,
        devices::PCI_ECAM_BASE,
        PCI_ECAM_SIZE,
        None,
        None,
    )
}
