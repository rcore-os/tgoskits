pub use rdif_intc;
use rdif_intc::Intc;
pub type IrqId = rdif_intc::IrqId;
use rdrive::DeviceId;

use crate::{arch::Plat, common::PlatOp};

#[must_use = "dropping ActiveIrq completes the interrupt in the interrupt controller"]
pub struct ActiveIrq {
    inner: <Plat as PlatOp>::ActiveIrq,
}

impl ActiveIrq {
    pub fn id(&self) -> IrqId {
        Plat::active_irq_id(&self.inner)
    }
}

/// Target specification for inter-processor interrupts.
#[derive(Clone, Copy, Debug)]
pub enum IpiTarget {
    /// Send to the current CPU.
    Current {
        /// The logical CPU ID of the current CPU.
        cpu_id: usize,
    },
    /// Send to a specific CPU.
    Other {
        /// The logical CPU ID of the target CPU.
        cpu_id: usize,
    },
    /// Send to all other CPUs.
    AllExceptCurrent {
        /// The logical CPU ID of the current CPU.
        cpu_id: usize,
        /// The total number of CPUs.
        cpu_num: usize,
    },
}

pub fn irq_setup_by_fdt(irq_parent: DeviceId, irq_cell: &[u32]) -> IrqId {
    let mut intc = rdrive::get::<Intc>(irq_parent).unwrap().lock().unwrap();
    debug!("Setting up IRQ {:?}", irq_cell);
    let id: usize = intc.setup_irq_by_fdt(irq_cell).into();
    id.into()
}

pub fn irq_set_enable(irq: IrqId, enable: bool) {
    debug!("Setting IRQ {:?} enable to {}", irq, enable);
    Plat::irq_set_enable(irq, enable);
}

pub fn send_ipi(irq: IrqId, target: IpiTarget) {
    Plat::send_ipi(irq, target);
}

pub fn systick_irq() -> IrqId {
    Plat::systick_irq()
}

pub fn begin_irq(raw: usize) -> Option<ActiveIrq> {
    Plat::begin_irq(raw).map(|inner| ActiveIrq { inner })
}

pub fn send_ipi_to_cpu(cpu_id: usize) {
    Plat::send_ipi_to_cpu(cpu_id);
}
