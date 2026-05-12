extern crate alloc;

use log::debug;
use rdif_intc::Intc;
use rdrive::{
    PlatformDevice,
    probe::OnProbeError,
    register::{DriverRegister, FdtInfo, ProbeKind, ProbeLevel, ProbePriority},
};

pub fn register() -> DriverRegister {
    DriverRegister {
        name: "Virtio",
        probe_kinds: &[ProbeKind::Fdt {
            compatibles: &["virtio,mmio"],
            on_probe: probe,
        }],
        level: ProbeLevel::PostKernel,
        priority: ProbePriority::DEFAULT,
    }
}

fn probe(info: FdtInfo<'_>, _dev: PlatformDevice) -> Result<(), OnProbeError> {
    let mut reg = info.node.regs().into_iter();
    let base_reg = reg.next().ok_or(OnProbeError::other(format!(
        "[{}] has no reg",
        info.node.name()
    )))?;

    if let Some(irq) = _dev.descriptor.irq_parent {
        let intc = rdrive::get::<Intc>(irq).unwrap();

        for interrupt in info.interrupts() {
            let irq_id = intc.lock().unwrap().setup_irq_by_fdt(&interrupt.specifier);
            debug!(
                "virtio mmio device [{}] setup irq: {:?}",
                info.node.name(),
                irq_id
            );
        }

        println!("parent intc: {:?}", intc.descriptor().name);
    }

    let mmio_size = base_reg.size.unwrap_or(0x1000);

    debug!(
        "virtio block device MMIO base address: {:#x}, size: {}",
        base_reg.address, mmio_size
    );

    Err(OnProbeError::NotMatch)
}
