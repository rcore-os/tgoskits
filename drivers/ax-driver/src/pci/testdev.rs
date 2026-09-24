//! QEMU IOMMU test device discovery for the ArceOS integration test.

use ax_sync::SpinLock;
use pcie::CommandRegister;
use rdrive::probe::{
    OnProbeError,
    pci::{PciAddress, ProbePci},
};

static TESTDEV: SpinLock<Option<(PciAddress, usize)>> = SpinLock::new(None);

crate::model_register!(
    name: "QEMU IOMMU test device",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Pci { on_probe: probe }],
);

fn probe(mut probe: ProbePci<'_>) -> Result<(), OnProbeError> {
    let endpoint = probe.endpoint();
    if endpoint.vendor_id() != 0x1b36 || endpoint.device_id() != 0x0005 {
        return Err(OnProbeError::NotMatch);
    }
    if probe.info().iommu.is_none() {
        return Err(OnProbeError::other(
            "QEMU IOMMU test device has no firmware IOMMU route",
        ));
    }
    let bar = endpoint
        .bar_mmio(0)
        .ok_or_else(|| OnProbeError::other("QEMU IOMMU test device has no BAR0"))?;
    let address = probe.info().address;
    let _dma = super::bound_dma(address, u64::MAX)?;
    probe.endpoint_mut().update_command(|mut cmd| {
        cmd.insert(CommandRegister::MEMORY_ENABLE | CommandRegister::BUS_MASTER_ENABLE);
        cmd
    });
    *TESTDEV.lock() = Some((address, bar.start));
    Ok(())
}

pub fn iommu_testdev_endpoint() -> Option<(PciAddress, usize)> {
    *TESTDEV.lock()
}
