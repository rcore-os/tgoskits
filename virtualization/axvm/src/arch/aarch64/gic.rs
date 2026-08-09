//! AArch64 GIC host operations for the ArceOS-backed AxVM runtime.

use alloc::vec::Vec;

use arm_gic_driver::v3::{
    ICH_ELRSR_EL2, ICH_HCR_EL2, ICH_LR_EL2, ICH_VTR_EL2, ReadWriteable, Readable, ich_lr_el2_get,
    ich_lr_el2_write,
};
use ax_memory_addr::{PhysAddr, VirtAddr};

use crate::host::{HostMemory, default_host};

fn with_gic<T>(f: impl FnOnce(&mut rdif_intc::Intc) -> T) -> T {
    let mut gic = rdrive::get_one::<rdif_intc::Intc>()
        .expect("failed to get GIC driver")
        .lock()
        .expect("failed to lock GIC driver");
    f(&mut gic)
}

pub(crate) fn inject_interrupt(irq: usize) -> Result<(), ()> {
    debug!("Injecting virtual interrupt: {irq}");

    with_gic(|gic| {
        if let Some(gic) = gic.typed_mut::<arm_gic_driver::v2::Gic>() {
            use arm_gic_driver::{
                IntId,
                v2::{VirtualInterruptConfig, VirtualInterruptState},
            };

            let gich = gic.hypervisor_interface().expect("failed to get GICH");
            gich.enable();
            if crate::irq::model::lr_slot_occupied(gich.get_virtual_interrupt(0).state as u32) {
                // The GICv2 path uses list register 0 only; it is occupied, so
                // report a retryable failure and let the caller re-queue the
                // edge instead of overwriting an in-flight interrupt.
                crate::runtime::vcpus::LR_SKIP_COUNT
                    .fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                return Err(());
            }
            gich.set_virtual_interrupt(
                0,
                VirtualInterruptConfig::software(
                    unsafe { IntId::raw(irq as _) },
                    None,
                    0,
                    VirtualInterruptState::Pending,
                    false,
                    true,
                ),
            );
            return Ok(());
        }

        if gic.typed_mut::<arm_gic_driver::v3::Gic>().is_some() {
            return inject_interrupt_gic_v3(irq);
        }

        panic!("no GIC driver found");
    })
}

fn inject_interrupt_gic_v3(vector: usize) -> Result<(), ()> {
    debug!("Injecting virtual interrupt: vector={vector}");
    let elsr = ICH_ELRSR_EL2.read(ICH_ELRSR_EL2::STATUS);
    let lr_num = ICH_VTR_EL2.read(ICH_VTR_EL2::LISTREGS) as usize + 1;

    if virtual_interrupt_busy(vector) {
        debug!("Virtual interrupt {vector} already pending/active in an LR, skipping");
        crate::runtime::vcpus::LR_SKIP_COUNT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        return Err(());
    }

    let mut free_lr = None;
    for i in 0..lr_num {
        if (1 << i) & elsr > 0 {
            free_lr.get_or_insert(i);
            break;
        }
    }

    let free_lr = free_lr.or_else(|| {
        (0..lr_num).find(|&i| ich_lr_el2_get(i).matches_all(ICH_LR_EL2::STATE::Invalid))
    });
    let Some(free_lr) = free_lr else {
        // The busy check above already keeps this vector queued when the list
        // registers are full; this branch is only reachable through a race.
        // Report retryable failure so the caller re-queues the edge instead of
        // losing it.
        debug!("Virtual interrupt {vector} deferred: no free list register");
        crate::runtime::vcpus::LR_SKIP_COUNT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        return Err(());
    };

    ich_lr_el2_write(
        free_lr,
        ICH_LR_EL2::VINTID.val(vector as u64) + ICH_LR_EL2::STATE::Pending + ICH_LR_EL2::GROUP::SET,
    );

    if !ICH_HCR_EL2.is_set(ICH_HCR_EL2::EN) {
        warn!("Virtual interrupt interface not enabled, enabling now");
        ICH_HCR_EL2.modify(ICH_HCR_EL2::EN::SET);
    }

    debug!("Virtual interrupt {vector} injected successfully in LR{free_lr}");
    Ok(())
}

/// Returns true when `vector` cannot be injected right now: either it is
/// already pending/active in a GICv3 list register, or every list register is
/// occupied by a different vector (no free slot).
pub(crate) fn virtual_interrupt_busy(vector: usize) -> bool {
    with_gic(|gic| {
        if let Some(gic) = gic.typed_mut::<arm_gic_driver::v2::Gic>() {
            let gich = gic.hypervisor_interface().expect("failed to get GICH");
            return crate::irq::model::lr_slot_occupied(gich.get_virtual_interrupt(0).state as u32);
        }
        if gic.typed_mut::<arm_gic_driver::v3::Gic>().is_some() {
            let lr_num = ICH_VTR_EL2.read(ICH_VTR_EL2::LISTREGS) as usize + 1;
            let slots = (0..lr_num)
                .map(|i| {
                    let lr_val = ich_lr_el2_get(i);
                    (
                        lr_val.read(ICH_LR_EL2::VINTID),
                        lr_val.read(ICH_LR_EL2::STATE),
                    )
                })
                .collect::<Vec<_>>();
            return crate::irq::model::lr_blocked(&slots, vector);
        }
        false
    })
}

pub(crate) fn read_gicd_iidr() -> u32 {
    with_gic(|gic| {
        if let Some(gic) = gic.typed_mut::<arm_gic_driver::v2::Gic>() {
            return gic.iidr_raw();
        }
        if let Some(gic) = gic.typed_mut::<arm_gic_driver::v3::Gic>() {
            return gic.iidr_raw();
        }
        panic!("no GIC driver found");
    })
}

pub(crate) fn read_gicd_typer() -> u32 {
    with_gic(|gic| {
        if let Some(gic) = gic.typed_mut::<arm_gic_driver::v2::Gic>() {
            return gic.typer_raw();
        }
        if let Some(gic) = gic.typed_mut::<arm_gic_driver::v3::Gic>() {
            return gic.typer_raw();
        }
        panic!("no GIC driver found");
    })
}

pub(crate) fn host_gicd_base() -> PhysAddr {
    with_gic(|gic| {
        if let Some(gic) = gic.typed_mut::<arm_gic_driver::v2::Gic>() {
            return default_host().virt_to_phys(VirtAddr::from(usize::from(gic.gicd_addr())));
        }
        if let Some(gic) = gic.typed_mut::<arm_gic_driver::v3::Gic>() {
            return default_host().virt_to_phys(VirtAddr::from(usize::from(gic.gicd_addr())));
        }
        panic!("no GIC driver found");
    })
}

pub(crate) fn host_gicr_base() -> PhysAddr {
    with_gic(|gic| {
        if let Some(gic) = gic.typed_mut::<arm_gic_driver::v3::Gic>() {
            return default_host().virt_to_phys(VirtAddr::from(usize::from(gic.gicr_addr())));
        }
        panic!("no GICv3 driver found");
    })
}

pub(crate) fn handle_current_irq() -> Option<usize> {
    // AArch64 ArceOS platform IRQ handlers acknowledge the current IRQ
    // internally. The raw vector argument is ignored by current GIC-backed
    // platforms, so keep the ack/EOI ownership inside the platform handler.
    let handled = ax_std::os::arceos::modules::ax_hal::irq::handle_irq(0);
    if handled
        && let (Some(vm_id), Some(vcpu_id)) = (crate::current_vm_id(), crate::current_vcpu_id())
        && let Some(vm) = crate::get_vm_by_id(vm_id)
    {
        let _ = vm.with_runtime(|runtime| {
            runtime.trace_virq_event(
                vm_id,
                crate::runtime::VirqTraceKind::HostIrqReceived,
                vcpu_id,
                0,
            );
            Ok(())
        });
    }
    handled.then_some(0)
}

pub(crate) fn fetch_irq() -> usize {
    handle_current_irq().unwrap_or(0)
}
