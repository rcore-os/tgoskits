use rdrive::{PlatformDevice, probe::OnProbeError};

use crate::config::devices;

const PCI_ECAM_SIZE: usize = (devices::PCI_BUS_END + 1) << 20;
const PCI_LEGACY_IRQS: &[usize] = &[16, 17, 18, 19];

rdrive::module_driver!(
    name: "Static PCIe ECAM",
    level: ProbeLevel::PreKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Static {
        on_probe: probe,
    }],
);

fn probe(plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    let mem32 = ax_driver::pci::pci_mem32_from_ranges(devices::PCI_RANGES);
    let mem64 = ax_driver::pci::pci_mem64_from_ranges(devices::PCI_RANGES);
    ax_driver::pci::register_static_legacy_irq_routes(PCI_LEGACY_IRQS, PCI_ECAM_SIZE);
    ax_driver::pci::register_ecam_controller(
        plat_dev,
        devices::PCI_ECAM_BASE,
        PCI_ECAM_SIZE,
        mem32,
        mem64,
    )
}
