use ax_driver::{PlatformDevice, probe::OnProbeError};

use crate::config::devices;

const PCI_ECAM_SIZE: usize = (devices::PCI_BUS_END + 1) << 20;

ax_driver::model_register!(
    name: "Static PCIe ECAM",
    level: ProbeLevel::PreKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Static {
        on_probe: probe,
    }],
);

fn probe(plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    if !ax_driver::pci::has_static_endpoint_drivers() {
        return Err(OnProbeError::NotMatch);
    }

    ax_driver::pci::register_ecam_controller(
        plat_dev,
        devices::PCI_ECAM_BASE,
        PCI_ECAM_SIZE,
        None,
        None,
    )
}
