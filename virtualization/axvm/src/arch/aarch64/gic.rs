//! AArch64 GIC host operations for the ArceOS-backed AxVM runtime.

use arm_gic_driver::v3::{
    ICH_ELRSR_EL2, ICH_HCR_EL2, ICH_LR_EL2, ICH_VTR_EL2, ReadWriteable, Readable, ich_lr_el2_get,
    ich_lr_el2_write,
};
use arm_vgic::{VgicError, VgicResult};
use ax_memory_addr::{PhysAddr, VirtAddr};

use crate::host::{HostMemory, default_host};

fn with_gic<T>(f: impl FnOnce(&mut rdif_intc::Intc) -> T) -> T {
    let mut gic = rdrive::get_one::<rdif_intc::Intc>()
        .expect("failed to get GIC driver")
        .lock()
        .expect("failed to lock GIC driver");
    f(&mut gic)
}

fn try_with_gic<T>(f: impl FnOnce(&mut rdif_intc::Intc) -> VgicResult<T>) -> VgicResult<T> {
    let device = rdrive::get_one::<rdif_intc::Intc>().ok_or_else(|| VgicError::Backend {
        operation: "access host GIC",
        detail: "GIC driver is unavailable".into(),
    })?;
    let mut gic = device.lock().map_err(|error| VgicError::Backend {
        operation: "lock host GIC",
        detail: alloc::format!("{error}"),
    })?;
    f(&mut gic)
}

fn checked_physical_spi(raw_irq: u32, max_intid: u32) -> VgicResult<arm_gic_driver::IntId> {
    let intid =
        arm_gic_driver::checked_intid(raw_irq, max_intid).map_err(|_| VgicError::InvalidIrq {
            irq: raw_irq as usize,
            max: max_intid as usize,
        })?;
    if intid.is_private() {
        return Err(VgicError::NotSpi {
            irq: raw_irq as usize,
        });
    }
    Ok(intid)
}

pub(crate) fn route_physical_spi(
    irq: u32,
    cpu_phys_id: usize,
    affinity: (u8, u8, u8, u8),
) -> VgicResult {
    try_with_gic(|gic| {
        if let Some(gic) = gic.typed_mut::<arm_gic_driver::v2::Gic>() {
            let intid = checked_physical_spi(irq, gic.max_intid())?;
            let cpu_bit = u32::try_from(cpu_phys_id)
                .ok()
                .and_then(|cpu| 1u8.checked_shl(cpu))
                .and_then(arm_gic_driver::v2::TargetList::from_one_hot)
                .ok_or_else(|| VgicError::Unsupported {
                    operation: "route physical SPI on GICv2",
                    detail: alloc::format!(
                        "CPU interface {cpu_phys_id} cannot be represented by ITARGETSR"
                    ),
                })?;
            gic.set_target_cpu(intid, cpu_bit);
            return Ok(());
        }

        if let Some(gic) = gic.typed_mut::<arm_gic_driver::v3::Gic>() {
            let intid = checked_physical_spi(irq, gic.max_intid())?;
            gic.set_target_cpu(
                intid,
                Some(arm_gic_driver::v3::Affinity {
                    aff3: affinity.0,
                    aff2: affinity.1,
                    aff1: affinity.2,
                    aff0: affinity.3,
                }),
            );
            return Ok(());
        }

        Err(VgicError::Unsupported {
            operation: "route physical SPI",
            detail: "registered interrupt controller is not GICv2 or GICv3".into(),
        })
    })
}

pub(crate) fn begin_physical_spi_quiesce(irq: u32) -> VgicResult {
    try_with_gic(|gic| {
        if let Some(gic) = gic.typed_mut::<arm_gic_driver::v2::Gic>() {
            let intid = checked_physical_spi(irq, gic.max_intid())?;
            gic.begin_spi_quiesce(intid)
                .map_err(|_| VgicError::NotSpi { irq: irq as usize })?;
            return Ok(());
        }

        if let Some(gic) = gic.typed_mut::<arm_gic_driver::v3::Gic>() {
            let intid = checked_physical_spi(irq, gic.max_intid())?;
            gic.begin_spi_quiesce(intid)
                .map_err(|_| VgicError::NotSpi { irq: irq as usize })?;
            return Ok(());
        }

        Err(VgicError::Unsupported {
            operation: "quiesce physical SPI",
            detail: "registered interrupt controller is not GICv2 or GICv3".into(),
        })
    })
}

pub(crate) fn poll_physical_distributor_write_complete() -> VgicResult<bool> {
    try_with_gic(|gic| {
        if let Some(gic) = gic.typed_mut::<arm_gic_driver::v2::Gic>() {
            return Ok(gic.poll_distributor_write_complete());
        }
        if let Some(gic) = gic.typed_mut::<arm_gic_driver::v3::Gic>() {
            return Ok(gic.poll_distributor_write_complete());
        }
        Err(VgicError::Unsupported {
            operation: "poll physical distributor write completion",
            detail: "registered interrupt controller is not GICv2 or GICv3".into(),
        })
    })
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
