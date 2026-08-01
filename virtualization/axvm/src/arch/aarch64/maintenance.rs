//! Per-CPU observation of GIC virtualization maintenance interrupts.

use core::sync::atomic::{AtomicU64, Ordering};

use ax_hal::irq::{CpuId, CpuMask, IrqError, IrqHandle, IrqReturn};
use axvm_types::{VCpuId, VMId, VmBackendError, VmBackendResult};
use spin::Once;

use super::{
    maintenance_registration::{
        MaintenanceHandlerRegistrationError, MaintenanceHandlerStatus, registration_status,
    },
    maintenance_state::{self, MaintenancePublication},
};

const MAX_TRACKED_CPUS: usize = usize::BITS as usize;
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);
static PUBLICATIONS: [MaintenancePublication; MAX_TRACKED_CPUS] =
    [const { MaintenancePublication::new() }; MAX_TRACKED_CPUS];
static ORPHAN_MAINTENANCE: AtomicU64 = AtomicU64::new(0);
static HANDLER: Once<Result<IrqHandle, MaintenanceHandlerRegistrationError>> = Once::new();

pub(super) fn register_handler() -> MaintenanceHandlerStatus {
    HANDLER.call_once(|| {
        let irq = match ax_hal::irq::gic_maintenance_irq() {
            Ok(irq) => irq,
            Err(IrqError::Unsupported) => {
                return Err(MaintenanceHandlerRegistrationError::Unavailable);
            }
            Err(error) => return Err(MaintenanceHandlerRegistrationError::Error(error)),
        };
        let raw_mask = crate::percpu::enabled_cpu_mask();
        let mut cpus = CpuMask::empty();
        for cpu in 0..usize::BITS as usize {
            if raw_mask & (1usize << cpu) != 0 {
                cpus.insert(CpuId(cpu));
            }
        }
        ax_hal::irq::request_percpu_irq(irq, cpus, |context| {
            observe(context.cpu.0);
            IrqReturn::Handled
        })
        .map_err(MaintenanceHandlerRegistrationError::Error)
    });
    let status = handler_status();
    if !matches!(status, MaintenanceHandlerStatus::Registered) {
        warn!("GIC maintenance IRQ handler is unavailable: {status:?}");
    }
    status
}

pub(super) fn handler_status() -> MaintenanceHandlerStatus {
    registration_status(HANDLER.get())
}

pub(super) fn next_generation() -> VmBackendResult<u64> {
    maintenance_state::next_generation(&NEXT_GENERATION)
}

pub(super) fn publish(
    cpu_id: usize,
    vm_id: VMId,
    vcpu_id: VCpuId,
    generation: u64,
) -> VmBackendResult {
    let publication = PUBLICATIONS
        .get(cpu_id)
        .ok_or(VmBackendError::InvalidInput)?;
    publication.publish(vm_id, vcpu_id, generation)
}

pub(super) fn consume(
    cpu_id: usize,
    vm_id: VMId,
    vcpu_id: VCpuId,
    generation: u64,
) -> VmBackendResult<bool> {
    let publication = PUBLICATIONS
        .get(cpu_id)
        .ok_or(VmBackendError::InvalidInput)?;
    publication.consume(vm_id, vcpu_id, generation)
}

pub(super) fn withdraw(
    cpu_id: usize,
    vm_id: VMId,
    vcpu_id: VCpuId,
    generation: u64,
) -> VmBackendResult {
    let publication = PUBLICATIONS
        .get(cpu_id)
        .ok_or(VmBackendError::InvalidInput)?;
    if publication.withdraw(vm_id, vcpu_id, generation).is_err() {
        warn!("discarding mismatched GIC maintenance owner publication on CPU {cpu_id}");
        return Err(VmBackendError::InvalidState);
    }
    Ok(())
}

fn observe(cpu_id: usize) {
    let Some(publication) = PUBLICATIONS.get(cpu_id) else {
        ORPHAN_MAINTENANCE.fetch_add(1, Ordering::Relaxed);
        return;
    };
    if !publication.observe() {
        ORPHAN_MAINTENANCE.fetch_add(1, Ordering::Relaxed);
    }
}
