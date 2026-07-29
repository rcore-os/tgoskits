//! AArch64 GIC host operations for the ArceOS-backed AxVM runtime.

use arm_gic_driver::v3::{
    ICH_ELRSR_EL2, ICH_HCR_EL2, ICH_VTR_EL2, ReadWriteable, Readable, ich_lr_el2_get,
    ich_lr_el2_set_raw,
};
use arm_vcpu::{
    ArmVcpuError, ArmVcpuResult, ArmVirtualIntId, IchDirectInjection, IchLrEntry, IchLrState,
    plan_direct_injection,
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

pub(crate) fn inject_interrupt(intid: ArmVirtualIntId) -> ArmVcpuResult {
    debug!("Injecting virtual interrupt: {intid}");

    with_gic(|gic| {
        if let Some(gic) = gic.typed_mut::<arm_gic_driver::v2::Gic>() {
            use arm_gic_driver::IntId;

            let gich = gic.hypervisor_interface().expect("failed to get GICH");
            gich.enable();
            // SAFETY: ArmVirtualIntId excludes the GIC special INTID range.
            let virtual_id = unsafe { IntId::raw(intid.as_u32()) };
            gich.set_virtual_interrupt(
                0,
                crate::arch::aarch64_gicv2::direct_injection_config(virtual_id),
            );
            return Ok(());
        }

        if gic.typed_mut::<arm_gic_driver::v3::Gic>().is_some() {
            return inject_interrupt_gic_v3(intid);
        }

        Err(ArmVcpuError::Unsupported)
    })
}

fn inject_interrupt_gic_v3(intid: ArmVirtualIntId) -> ArmVcpuResult {
    debug!("Injecting virtual interrupt: intid={intid}");
    let lr_num = ICH_VTR_EL2.read(ICH_VTR_EL2::LISTREGS) as usize + 1;
    if !(1..=16).contains(&lr_num) {
        return Err(ArmVcpuError::InvalidListRegisterCount { count: lr_num });
    }

    let mut raw_lrs = [0; 16];
    for (slot, raw) in raw_lrs[..lr_num].iter_mut().enumerate() {
        *raw = ich_lr_el2_get(slot).get();
    }
    let empty_status = ICH_ELRSR_EL2.read(ICH_ELRSR_EL2::STATUS) as u16;
    let free_lr = match plan_direct_injection(intid, empty_status, &raw_lrs[..lr_num])? {
        IchDirectInjection::AlreadyPresent => {
            debug!("Virtual interrupt {intid} already pending/active, skipping");
            return Ok(());
        }
        IchDirectInjection::Vacant(slot) => slot,
    };
    ich_lr_el2_set_raw(
        free_lr,
        IchLrEntry::Software {
            intid,
            state: IchLrState::Pending,
            priority: 0,
            group1: true,
            eoi: false,
        }
        .encode(),
    );

    if !ICH_HCR_EL2.is_set(ICH_HCR_EL2::EN) {
        warn!("Virtual interrupt interface not enabled, enabling now");
        ICH_HCR_EL2.modify(ICH_HCR_EL2::EN::SET);
    }

    debug!("Virtual interrupt {intid} injected successfully in LR{free_lr}");
    Ok(())
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
