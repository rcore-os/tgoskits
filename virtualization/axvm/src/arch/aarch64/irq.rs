//! Physical SPI forwarding for AArch64 Hybrid-mode guests.

use alloc::vec::Vec;

use axvm_types::Aarch64GicSpi;

use crate::{
    AxVmError, AxVmResult,
    config::Aarch64ForwardedIrq,
    irq::forwarding::{GenerationOwnerTable, aarch64_virtual_timer_route},
};

const SPI_BASE: usize = 32;
const SPI_LIMIT: usize = 1020;
const SPI_COUNT: usize = SPI_LIMIT - SPI_BASE;
static SPI_OWNERS: GenerationOwnerTable<SPI_COUNT> = GenerationOwnerTable::new();

pub(crate) fn register_platform_irq_injector() {
    ax_crate_interface::call_interface!(
        crate::irq::PlatformIrqInjectorIf::register_virtual_irq_injector(inject_virtual_irq)
    );
}

/// Resolves every Hybrid route before claiming it, then applies host affinity.
///
/// Affinity is not restored on rollback because the platform API has no affinity getter.
pub(crate) fn setup_hybrid_forwarding(vm: &crate::AxVMRef, cpu_id: usize) -> AxVmResult {
    let generation = vm.with_runtime(|runtime| Ok(runtime.forwarding_generation_id()))?;
    let owner = vm
        .id()
        .checked_add(1)
        .expect("VM ID must leave zero available as the unowned SPI marker");
    let routes = hybrid_routes(vm);
    let affinity = ax_hal::irq::IrqAffinity::Fixed(ax_hal::irq::CpuId(cpu_id));
    let resolved = routes
        .iter()
        .map(|route| {
            ax_hal::irq::resolve_external_irq(ax_hal::irq::HwIrq(route.host_intid())).map_err(
                |error| {
                    AxVmError::interrupt(
                        "resolve AArch64 Hybrid SPI",
                        format_args!("INTID {}: {error:?}", route.host_intid()),
                    )
                },
            )
        })
        .collect::<AxVmResult<Vec<_>>>()?;
    let spis = routes
        .iter()
        .map(|route| Aarch64GicSpi::new(route.host_spi_offset()).unwrap())
        .collect::<Vec<_>>();
    let indices = spis
        .iter()
        .map(|spi| spi_index(spi.intid() as usize))
        .collect::<Vec<_>>();
    let newly_claimed = SPI_OWNERS
        .claim_all(owner, generation, &indices)
        .map_err(|index| {
            AxVmError::resource_conflict(
                "AArch64 GIC SPI",
                format_args!("INTID {} is already owned by another VM", index + SPI_BASE),
            )
        })?;

    for irq in resolved {
        if let Err(error) = ax_hal::irq::set_affinity(irq, affinity) {
            SPI_OWNERS.release_generation(owner, generation, &newly_claimed);
            return Err(AxVmError::interrupt(
                "route AArch64 Hybrid SPI",
                format_args!("{error:?}"),
            ));
        }
    }
    Ok(())
}

pub(crate) fn unregister_forward_spis(vm: &crate::AxVMRef, generation: usize) {
    let indices = hybrid_routes(vm)
        .iter()
        .map(|route| spi_index(route.host_intid() as usize))
        .collect::<Vec<_>>();
    let Some(owner) = vm.id().checked_add(1) else {
        return;
    };
    SPI_OWNERS.release_generation(owner, generation, &indices);
}

fn hybrid_routes(vm: &crate::AxVMRef) -> Vec<Aarch64ForwardedIrq> {
    vm.with_config(|config| config.aarch64_hybrid_forwarded_irqs().to_vec())
}

fn checked_spi_index(intid: usize) -> Option<usize> {
    (SPI_BASE..SPI_LIMIT)
        .contains(&intid)
        .then_some(intid - SPI_BASE)
}

fn spi_index(intid: usize) -> usize {
    intid - SPI_BASE
}

fn inject_virtual_irq(intid: usize) -> bool {
    if let Some(route) = aarch64_virtual_timer_route(intid) {
        return super::gic::inject_interrupt_hw1(
            route.virtual_intid,
            route
                .physical_intid
                .expect("the virtual-timer route is hardware mapped"),
        );
    }
    let Some(vm_id) = crate::current_vm_id() else {
        trace!("skip AArch64 physical IRQ {intid}: no current VM context");
        return false;
    };
    let Some(vm) = crate::get_vm_by_id(vm_id) else {
        return false;
    };
    let Some(index) = checked_spi_index(intid) else {
        return false;
    };
    let Some(owner) = vm_id.checked_add(1) else {
        return false;
    };
    if !SPI_OWNERS.is_owned_by(index, owner) {
        return false;
    }
    let guest_intid = vm.with_config(|config| {
        config
            .aarch64_hybrid_forwarded_irqs()
            .iter()
            .find(|route| route.host_intid() as usize == intid)
            .map(|route| route.guest_intid() as usize)
    });
    guest_intid.is_some_and(|guest| super::gic::inject_interrupt_hw1(guest, intid))
}
