//! AArch64 GIC host operations for the ArceOS-backed AxVM runtime.

use arm_vcpu::{ArmInterruptVirtualization, ArmVcpuError, ArmVcpuResult, ArmVirtualIntId};
use ax_memory_addr::{PhysAddr, VirtAddr};

use crate::host::{HostMemory, default_host};

fn with_gic<T>(f: impl FnOnce(&mut rdif_intc::Intc) -> T) -> T {
    let mut gic = rdrive::get_one::<rdif_intc::Intc>()
        .expect("failed to get GIC driver")
        .lock()
        .expect("failed to lock GIC driver");
    f(&mut gic)
}

pub(crate) fn interrupt_virtualization() -> ArmVcpuResult<ArmInterruptVirtualization> {
    with_gic(|gic| {
        if gic.typed_mut::<arm_gic_driver::v2::Gic>().is_some() {
            return Ok(ArmInterruptVirtualization::GicV2);
        }
        if gic.typed_mut::<arm_gic_driver::v3::Gic>().is_some() {
            return Ok(ArmInterruptVirtualization::GicV3);
        }
        Err(ArmVcpuError::Unsupported)
    })
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
            gich.set_virtual_interrupt(0, super::gicv2::direct_injection_config(virtual_id));
            return Ok(());
        }

        // GICv3 injection must go through ArmVcpu's bound ICH context. Keeping
        // this host callback GICv2-only prevents an unguarded LR/HCR access
        // path from bypassing the CPU-local ownership boundary.
        Err(ArmVcpuError::Unsupported)
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
    ax_std::os::arceos::modules::ax_hal::irq::handle_irq(0).then_some(0)
}

pub(crate) fn fetch_irq() -> usize {
    handle_current_irq().unwrap_or(0)
}
