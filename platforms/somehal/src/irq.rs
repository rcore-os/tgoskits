use rdif_intc::Intc;
pub use rdif_intc::{self, IrqId};
use rdrive::DeviceId;
pub use someboot::irq::*;

use crate::{arch::Plat, common::PlatOp};

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

pub(crate) fn _handle_irq(hwirq: IrqId) {
    unsafe extern "Rust" {
        fn _someboot_handle_irq(hwirq: IrqId);
    }
    unsafe {
        _someboot_handle_irq(hwirq);
    }
}

pub fn irq_handler_raw() -> IrqId {
    Plat::irq_handler().raw().into()
}

pub fn irq_handler_with_raw(raw: usize) -> Option<IrqId> {
    Plat::irq_handler_with_raw(raw).map(|irq| irq.raw().into())
}

#[cfg(target_arch = "riscv64")]
pub fn claim_external_irq() -> Option<IrqId> {
    crate::arch::claim_external_irq().map(|irq| irq.raw().into())
}

#[cfg(target_arch = "riscv64")]
pub fn complete_external_irq(irq: IrqId) {
    crate::arch::complete_external_irq(someboot::irq::IrqId::new(irq.raw()));
}

pub fn send_ipi_to_cpu(cpu_id: usize) {
    Plat::send_ipi_to_cpu(cpu_id);
}
