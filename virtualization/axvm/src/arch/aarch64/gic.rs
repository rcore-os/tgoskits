//! AArch64 GIC host operations for the ArceOS-backed AxVM runtime.

use arm_gic_driver::v3::{
    ICH_ELRSR_EL2, ICH_HCR_EL2, ICH_LR_EL2, ICH_VTR_EL2, ReadWriteable, Readable, ich_lr_el2_get,
    ich_lr_el2_write,
};
use ax_memory_addr::{PhysAddr, VirtAddr};

use crate::{
    host::{HostMemory, default_host},
    vm::{
        PassthroughSpiController,
        passthrough_irq::{
            PhysicalSpiDelivery, PhysicalSpiReclaim, PhysicalSpiRoutePolicy, PhysicalSpiState,
        },
    },
};

fn with_gic<T>(f: impl FnOnce(&mut rdif_intc::Intc) -> T) -> T {
    let mut gic = rdrive::get_one::<rdif_intc::Intc>()
        .expect("failed to get GIC driver")
        .lock()
        .expect("failed to lock GIC driver");
    f(&mut gic)
}

/// Hands the physical CPU interface from the host to a passthrough guest.
///
/// The host uses split EOI/deactivate mode while running the hypervisor. A
/// passthrough guest owns the physical CPU interface and must instead start
/// from the architectural combined-EOI mode; otherwise guests that issue only
/// `EOIR` leave their first interrupt active forever. Guest initialization may
/// subsequently select split mode if its driver also issues `DIR`.
pub(crate) fn prepare_passthrough_guest_cpu_interface() {
    with_gic(|gic| {
        if let Some(gic) = gic.typed_mut::<arm_gic_driver::v3::Gic>() {
            gic.cpu_interface().set_eoi_mode(false);
            return;
        }
        if let Some(gic) = gic.typed_mut::<arm_gic_driver::v2::Gic>() {
            gic.cpu_interface().set_eoi_mode_ns(false);
            return;
        }
        panic!("no GIC driver found while preparing a passthrough guest");
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

impl arm_gic_driver::v3::SpiDeliverySpec for PhysicalSpiDelivery {
    fn raw_intid(&self) -> u32 {
        u32::try_from(self.intid).expect("passthrough gate validated its physical SPI INTIDs")
    }

    fn target_affinity(&self) -> arm_gic_driver::v3::Affinity {
        arm_gic_driver::v3::Affinity::from_mpidr(self.target_mpidr as u64)
    }

    fn route_policy(&self) -> arm_gic_driver::v3::SpiRoutePolicy {
        match self.route_policy {
            PhysicalSpiRoutePolicy::Configure => arm_gic_driver::v3::SpiRoutePolicy::Configure,
            PhysicalSpiRoutePolicy::Preserve => arm_gic_driver::v3::SpiRoutePolicy::Preserve,
        }
    }
}

impl arm_gic_driver::v3::SpiReclaimSpec for PhysicalSpiReclaim {
    fn raw_intid(&self) -> u32 {
        u32::try_from(self.intid).expect("passthrough gate validated its physical SPI INTIDs")
    }

    fn record_state(&mut self, state: arm_gic_driver::v3::SpiLineState) {
        self.state = Some(PhysicalSpiState {
            active: state.active,
            pending: state.pending,
        });
    }
}

struct HostGicPassthroughSpiController<'a> {
    gic: &'a mut rdif_intc::Intc,
}

impl PassthroughSpiController for HostGicPassthroughSpiController<'_> {
    fn deliver_spis(&mut self, requests: &[PhysicalSpiDelivery]) -> crate::AxVmResult {
        if let Some(gic) = self.gic.typed_mut::<arm_gic_driver::v3::Gic>() {
            return gic
                .route_enable_and_pend_spis(requests)
                .map_err(|error| crate::AxVmError::interrupt("deliver physical SPIs", error));
        }
        if self.gic.typed_mut::<arm_gic_driver::v2::Gic>().is_some() {
            return Err(crate::AxVmError::unsupported(
                "deliver physical SPIs",
                "GICv2 target routing is not implemented for emulated passthrough devices",
            ));
        }
        Err(crate::AxVmError::resource_unavailable(
            "GIC driver",
            "no supported host GIC is registered",
        ))
    }

    fn reclaim_spis(&mut self, requests: &mut [PhysicalSpiReclaim]) -> crate::AxVmResult {
        if let Some(gic) = self.gic.typed_mut::<arm_gic_driver::v3::Gic>() {
            return gic
                .reclaim_pending_spis(requests)
                .map_err(|error| crate::AxVmError::interrupt("reclaim physical SPIs", error));
        }
        if self.gic.typed_mut::<arm_gic_driver::v2::Gic>().is_some() {
            return Err(crate::AxVmError::unsupported(
                "reclaim physical SPIs",
                "GICv2 pending-state reclamation is not implemented for passthrough devices",
            ));
        }
        Err(crate::AxVmError::resource_unavailable(
            "GIC driver",
            "no supported host GIC is registered",
        ))
    }
}

/// Runs one ownership-gate transition while holding the host GIC lock.
///
/// Passthrough paths always acquire the global GIC lock before the per-VM gate.
/// The caller must release both by returning from this closure before waking a
/// vCPU task.
pub(crate) fn with_passthrough_spi_controller<T>(
    f: impl FnOnce(&mut dyn PassthroughSpiController) -> T,
) -> T {
    with_gic(|gic| f(&mut HostGicPassthroughSpiController { gic }))
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

pub(crate) fn fetch_irq() -> usize {
    handle_current_irq().unwrap_or(0)
}
