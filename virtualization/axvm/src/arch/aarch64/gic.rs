//! AArch64 GIC host operations for the ArceOS-backed AxVM runtime.

use arm_gic_driver::v3::{
    ICH_ELRSR_EL2, ICH_HCR_EL2, ICH_LR_EL2, ICH_VTR_EL2, ReadWriteable, Readable, ich_lr_el2_get,
    ich_lr_el2_set,
};
use ax_memory_addr::{PhysAddr, VirtAddr};

use crate::{
    host::{HostMemory, default_host},
    irq::forwarding::{LrRouteRequest, LrSnapshot, LrState, lr_matches_route},
};

fn with_gic<T>(f: impl FnOnce(&mut rdif_intc::Intc) -> T) -> T {
    let mut gic = rdrive::get_one::<rdif_intc::Intc>()
        .expect("failed to get GIC driver")
        .lock()
        .expect("failed to lock GIC driver");
    f(&mut gic)
}

pub(crate) fn inject_interrupt(irq: usize) {
    debug!("Injecting virtual interrupt: {irq}");

    with_gic(|gic| {
        if let Some(gic) = gic.typed_mut::<arm_gic_driver::v2::Gic>() {
            use arm_gic_driver::{
                IntId,
                v2::{VirtualInterruptConfig, VirtualInterruptState},
            };

            let gich = gic.hypervisor_interface().expect("failed to get GICH");
            gich.enable();
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
            return;
        }

        if gic.typed_mut::<arm_gic_driver::v3::Gic>().is_some() {
            assert!(
                inject_interrupt_gic_v3(irq, None),
                "no free list register to inject IRQ {irq}"
            );
            return;
        }

        panic!("no GIC driver found");
    });
}

fn find_free_lr(elsr: u64, lr_num: usize) -> Option<usize> {
    (0..lr_num).find(|&index| (1u64 << index) & elsr != 0)
}

fn inject_interrupt_gic_v3(vector: usize, physical_intid: Option<usize>) -> bool {
    debug!("Injecting virtual interrupt: vector={vector}, physical_intid={physical_intid:?}");
    let elsr = ICH_ELRSR_EL2.read(ICH_ELRSR_EL2::STATUS);
    let lr_num = ICH_VTR_EL2.read(ICH_VTR_EL2::LISTREGS) as usize + 1;
    let mut free_lr = find_free_lr(elsr, lr_num);
    let request = LrRouteRequest {
        virtual_intid: vector,
        physical_intid,
    };

    for i in 0..lr_num {
        let lr_val = ich_lr_el2_get(i);
        let hardware = lr_val.read(ICH_LR_EL2::HW) != 0;
        let snapshot = LrSnapshot {
            virtual_intid: lr_val.read(ICH_LR_EL2::VINTID) as usize,
            hardware,
            physical_intid: hardware.then(|| lr_val.read(ICH_LR_EL2::PINTID) as usize),
            state: match lr_val.read(ICH_LR_EL2::STATE) {
                0 => LrState::Invalid,
                1 => LrState::Pending,
                2 => LrState::Active,
                _ => LrState::PendingActive,
            },
        };
        if lr_matches_route(snapshot, request) {
            debug!("Virtual interrupt {vector} already pending/active in LR{i}, skipping");
            return true;
        }
    }

    free_lr = free_lr.or_else(|| {
        (0..lr_num).find(|&i| ich_lr_el2_get(i).matches_all(ICH_LR_EL2::STATE::Invalid))
    });
    let Some(free_lr) = free_lr else {
        warn!("no free list register to inject IRQ {vector}");
        return false;
    };

    let mut lr = ich_lr_el2_get(free_lr);
    lr.set(build_gic_v3_lr(vector, physical_intid));
    ich_lr_el2_set(free_lr, lr);

    if !ICH_HCR_EL2.is_set(ICH_HCR_EL2::EN) {
        warn!("Virtual interrupt interface not enabled, enabling now");
        ICH_HCR_EL2.modify(ICH_HCR_EL2::EN::SET);
    }

    debug!("Virtual interrupt {vector} injected successfully in LR{free_lr}");
    true
}

fn build_gic_v3_lr(vector: usize, physical_intid: Option<usize>) -> u64 {
    let mut lr = (ICH_LR_EL2::VINTID.val(vector as u64)
        + ICH_LR_EL2::STATE::Pending
        + ICH_LR_EL2::GROUP::SET)
        .value;
    if let Some(physical_intid) = physical_intid {
        lr |= (ICH_LR_EL2::PINTID.val(physical_intid as u64) + ICH_LR_EL2::HW.val(1)).value;
    }
    lr
}

/// Injects a physical interrupt through a hardware-mapped GICv3 list register.
pub(crate) fn inject_interrupt_hw1(vector: usize, physical_intid: usize) -> bool {
    inject_interrupt_gic_v3(vector, Some(physical_intid))
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
    ax_std::os::arceos::modules::ax_hal::irq::handle_irq(0).then_some(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hardware_mapped_lr_links_virtual_and_physical_intids() {
        let lr = build_gic_v3_lr(45, Some(45));

        assert_eq!((lr >> 61) & 1, 1);
        assert_eq!((lr >> 32) & 0x1fff, 45);
        assert_eq!(lr & 0xffff_ffff, 45);
    }

    #[test]
    fn free_lr_selection_reports_full_state() {
        assert_eq!(find_free_lr(0, 4), None);
        assert_eq!(find_free_lr(1 << 2, 4), Some(2));
    }

    #[test]
    fn software_injected_lr_has_no_physical_mapping() {
        let lr = build_gic_v3_lr(27, None);

        assert_eq!((lr >> 61) & 1, 0);
        assert_eq!((lr >> 32) & 0x1fff, 0);
        assert_eq!(lr & 0xffff_ffff, 27);
    }
}

pub(crate) fn fetch_irq() -> usize {
    handle_current_irq().unwrap_or(0)
}
