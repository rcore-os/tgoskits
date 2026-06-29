extern crate alloc;

use log::debug;
use rdif_intc::Intc;
use rdrive::{
    probe::OnProbeError,
    register::{DriverRegister, ProbeFdt, ProbeKind, ProbeLevel, ProbePriority},
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

fn probe(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, dev) = probe.into_parts();
    let mut reg = info.node.regs().into_iter();
    let base_reg = reg.next().ok_or(OnProbeError::other(format!(
        "[{}] has no reg",
        info.node.name()
    )))?;

    if let Some(irq) = dev.descriptor.irq_parent {
        let intc = rdrive::get::<Intc>(irq).unwrap();

        for interrupt in info.interrupts() {
            let mut intc = intc.lock().unwrap();
            let translation = intc
                .translate_fdt(&interrupt.specifier)
                .expect("failed to translate interrupt");
            intc.configure(&translation)
                .expect("failed to configure interrupt");
            let irq_id = translation.id;
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
