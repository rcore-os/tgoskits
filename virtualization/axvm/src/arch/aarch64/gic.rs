//! AArch64 GIC host operations for the ArceOS-backed AxVM runtime.

use core::sync::atomic::{AtomicBool, Ordering};

use arm_gic_driver::v3::{
    ICH_ELRSR_EL2, ICH_HCR_EL2, ICH_LR_EL2, ICH_VTR_EL2, ReadWriteable, Readable, ich_lr_el2_get,
    ich_lr_el2_write,
};
use arm_vcpu::ArmHostIrq;
use ax_memory_addr::{PhysAddr, VirtAddr};

use crate::host::{HostMemory, default_host};

fn with_gic<T>(f: impl FnOnce(&mut rdif_intc::Intc) -> T) -> T {
    let mut gic = rdrive::get_one::<rdif_intc::Intc>()
        .expect("failed to get GIC driver")
        .lock()
        .expect("failed to lock GIC driver");
    f(&mut gic)
}

pub(crate) fn init_current_cpu() {
    with_gic(|gic| {
        let Some(gic) = gic.typed_mut::<arm_gic_driver::v2::Gic>() else {
            return;
        };
        if let Some(mut gich) = gic.hypervisor_interface() {
            gich.init_current_cpu();
        }
    });
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
            inject_interrupt_gic_v3(irq);
            return;
        }

        panic!("no GIC driver found");
    });
}

const GICV2_IRQ_COUNT: usize = 1024;

static GICV2_GUEST_IRQ_ENABLES: [AtomicBool; GICV2_IRQ_COUNT] =
    [const { AtomicBool::new(false) }; GICV2_IRQ_COUNT];

#[derive(Debug)]
enum Gicv2ExitIrq {
    Spurious,
    GuestForwarded(u32),
    Host(u32),
    Dropped,
}

fn inject_gicv2_hardware_interrupt(
    gich: &arm_gic_driver::v2::HypervisorInterface,
    irq: arm_gic_driver::IntId,
) -> bool {
    let Some(lr_index) =
        (0..gich.get_list_register_count()).find(|&lr_index| gich.is_list_register_empty(lr_index))
    else {
        return false;
    };

    use arm_gic_driver::v2::{VirtualInterruptConfig, VirtualInterruptState};

    let irq_number = irq.to_u32();
    gich.enable();
    gich.set_virtual_interrupt(
        lr_index,
        VirtualInterruptConfig::hardware(irq, irq_number, 0, VirtualInterruptState::Pending, false),
    );
    true
}

fn claim_gicv2_exit_irq() -> Option<Gicv2ExitIrq> {
    with_gic(|gic| {
        use arm_gic_driver::v2::Ack;

        let gic = gic.typed_mut::<arm_gic_driver::v2::Gic>()?;
        let cpu = gic.cpu_interface();
        let ack = cpu.ack();
        if ack.is_special() {
            return Some(Gicv2ExitIrq::Spurious);
        }

        let irq = match ack {
            Ack::Other(irq) => irq,
            Ack::SGI { intid, .. } => intid,
        };
        let irq_number = irq.to_u32();
        let guest_enabled = GICV2_GUEST_IRQ_ENABLES
            .get(irq_number as usize)
            .is_some_and(|enabled| enabled.load(Ordering::Acquire));
        if irq_number == 27 || guest_enabled {
            let gich = gic.hypervisor_interface().expect("failed to get GICH");
            if !inject_gicv2_hardware_interrupt(&gich, irq) {
                warn!("No free GICv2 LR for guest IRQ {irq_number}; deactivating it");
                cpu.eoi(ack);
                if cpu.eoi_mode_ns() {
                    cpu.dir(ack);
                }
                return Some(Gicv2ExitIrq::Dropped);
            }

            // In two-step EOI mode, leave the physical IRQ active. The hardware
            // LR link deactivates it when the guest EOIs the virtual IRQ through GICV.
            cpu.eoi(ack);
            return Some(Gicv2ExitIrq::GuestForwarded(irq_number));
        }

        cpu.eoi(ack);
        if cpu.eoi_mode_ns() {
            cpu.dir(ack);
        }
        Some(Gicv2ExitIrq::Host(irq_number))
    })
}

fn dispatch_claimed_gicv2_irq(irq: Gicv2ExitIrq) -> Option<ArmHostIrq> {
    match irq {
        Gicv2ExitIrq::Spurious | Gicv2ExitIrq::Dropped => None,
        Gicv2ExitIrq::GuestForwarded(irq) => Some(ArmHostIrq::guest_forwarded(irq as usize)),
        Gicv2ExitIrq::Host(irq) => {
            use ax_std::os::arceos::modules::ax_hal::irq::{
                HwIrq, dispatch_irq, resolve_percpu_irq,
            };

            match resolve_percpu_irq(HwIrq(irq)) {
                Ok(host_irq) => {
                    let outcome = dispatch_irq(host_irq);
                    if !outcome.handled {
                        warn!("Unhandled host IRQ {host_irq:?} during AArch64 VM exit");
                    }
                }
                Err(error) => warn!("Failed to resolve host GIC IRQ {irq}: {error:?}"),
            }
            Some(ArmHostIrq::host(irq as usize))
        }
    }
}

fn inject_interrupt_gic_v3(vector: usize) {
    debug!("Injecting virtual interrupt: vector={vector}");
    let elsr = ICH_ELRSR_EL2.read(ICH_ELRSR_EL2::STATUS);
    let lr_num = ICH_VTR_EL2.read(ICH_VTR_EL2::LISTREGS) as usize + 1;

    let mut free_lr = None;
    for i in 0..lr_num {
        if (1 << i) & elsr > 0 {
            free_lr.get_or_insert(i);
            continue;
        }

        let lr_val = ich_lr_el2_get(i);
        if lr_val.read(ICH_LR_EL2::VINTID) == vector as u64
            && lr_val.matches_any(&[ICH_LR_EL2::STATE::Pending, ICH_LR_EL2::STATE::Active])
        {
            debug!("Virtual interrupt {vector} already pending/active in LR{i}, skipping");
            return;
        }
    }

    let free_lr = free_lr
        .or_else(|| {
            (0..lr_num).find(|&i| ich_lr_el2_get(i).matches_all(ICH_LR_EL2::STATE::Invalid))
        })
        .unwrap_or_else(|| panic!("no free list register to inject IRQ {vector}"));

    ich_lr_el2_write(
        free_lr,
        ICH_LR_EL2::VINTID.val(vector as u64) + ICH_LR_EL2::STATE::Pending + ICH_LR_EL2::GROUP::SET,
    );

    if !ICH_HCR_EL2.is_set(ICH_HCR_EL2::EN) {
        warn!("Virtual interrupt interface not enabled, enabling now");
        ICH_HCR_EL2.modify(ICH_HCR_EL2::EN::SET);
    }

    debug!("Virtual interrupt {vector} injected successfully in LR{free_lr}");
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

pub(crate) fn current_cpu_target() -> u8 {
    with_gic(|gic| {
        if let Some(gic) = gic.typed_mut::<arm_gic_driver::v2::Gic>() {
            return gic.cpu_interface().current_cpu_target().as_u8();
        }
        0
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

pub(crate) fn set_host_irq_enable(irq: u32, enable: bool) {
    with_gic(|gic| {
        let Some(gic) = gic.typed_mut::<arm_gic_driver::v2::Gic>() else {
            return;
        };
        let Ok(intid) = arm_gic_driver::checked_intid(irq, gic.max_intid()) else {
            return;
        };
        let Some(guest_enabled) = GICV2_GUEST_IRQ_ENABLES.get(irq as usize) else {
            return;
        };

        if enable {
            if !intid.is_private() {
                let target = gic.cpu_interface().current_cpu_target();
                gic.set_target_cpu(intid, target);
            }
            guest_enabled.store(true, Ordering::Release);
            gic.set_irq_enable(intid, true);
        } else {
            gic.set_irq_enable(intid, false);
            guest_enabled.store(false, Ordering::Release);
        }
    });
}

pub(crate) fn handle_current_irq() -> Option<ArmHostIrq> {
    if let Some(irq) = claim_gicv2_exit_irq() {
        return dispatch_claimed_gicv2_irq(irq);
    }

    // AArch64 ArceOS GICv3 handlers acknowledge the current IRQ internally.
    ax_std::os::arceos::modules::ax_hal::irq::handle_irq(0).then_some(ArmHostIrq::host(0))
}
