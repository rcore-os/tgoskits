//! Physical SPI forwarding for AArch64 Hybrid-mode guests.

use alloc::vec::Vec;

use crate::{
    AxVmError, AxVmResult,
    config::Aarch64ForwardedIrq,
    irq::forwarding::{
        ControllerIrqRegistry, PhysicalIrqRoute, aarch64_virtual_timer_route,
        resolve_exclusive_irq_owner,
    },
};

const SPI_BASE: usize = 32;
const SPI_LIMIT: usize = 1020;
const SPI_COUNT: usize = SPI_LIMIT - SPI_BASE;
static SPI_REGISTRY: ControllerIrqRegistry<SPI_COUNT> = ControllerIrqRegistry::new(SPI_BASE as u32);

pub(crate) fn register_platform_irq_injector() {
    ax_crate_interface::call_interface!(
        crate::irq::PlatformIrqInjectorIf::register_virtual_irq_injector(inject_virtual_irq)
    );
}

/// Resolves every Hybrid route before claiming it, then applies host affinity.
///
/// Affinity is not restored on rollback because the platform API has no affinity getter.
pub(crate) fn setup_hybrid_forwarding(
    vm: &crate::AxVMRef,
    cpu_id: usize,
    generation: usize,
) -> AxVmResult {
    let owner = vm
        .id()
        .checked_add(1)
        .expect("VM ID must leave zero available as the unowned SPI marker");
    let routes = hybrid_routes(vm);
    let affinity = ax_hal::irq::IrqAffinity::Fixed(ax_hal::irq::CpuId(cpu_id));
    let resolved = routes
        .iter()
        .map(|route| {
            ax_hal::irq::resolve_external_irq(ax_hal::irq::HwIrq(route.host_intid()))
                .map(|irq| PhysicalIrqRoute::new(irq, route.guest_intid() as usize))
                .map_err(|error| {
                    AxVmError::interrupt(
                        "resolve AArch64 Hybrid SPI",
                        format_args!("INTID {}: {error:?}", route.host_intid()),
                    )
                })
        })
        .collect::<AxVmResult<Vec<_>>>()?;
    for route in &resolved {
        SPI_REGISTRY
            .bind_domain(route.host_irq().domain)
            .map_err(|domain| {
                AxVmError::interrupt(
                    "bind AArch64 Hybrid GIC domain",
                    format_args!("registry is already bound to {domain:?}"),
                )
            })?;
    }
    let claims = SPI_REGISTRY
        .claim_all(owner, generation, &resolved)
        .map_err(|route| {
            AxVmError::resource_conflict(
                "AArch64 GIC SPI",
                format_args!(
                    "INTID {} is already owned by another VM",
                    route.host_irq().hwirq.0
                ),
            )
        })?;

    for route in resolved {
        ax_hal::irq::set_affinity(route.host_irq(), affinity).map_err(|error| {
            AxVmError::interrupt("route AArch64 Hybrid SPI", format_args!("{error:?}"))
        })?;
    }
    claims.commit();
    Ok(())
}

pub(crate) fn unregister_forward_spis(vm: &crate::AxVMRef, generation: usize) {
    let routes = hybrid_routes(vm)
        .iter()
        .filter_map(|route| {
            SPI_REGISTRY
                .bound_irq(route.host_intid())
                .map(|irq| PhysicalIrqRoute::new(irq, route.guest_intid() as usize))
        })
        .collect::<Vec<_>>();
    let Some(owner) = vm.id().checked_add(1) else {
        return;
    };
    SPI_REGISTRY.release_generation(owner, generation, &routes);
}

fn hybrid_routes(vm: &crate::AxVMRef) -> Vec<Aarch64ForwardedIrq> {
    vm.with_config(|config| config.aarch64_hybrid_forwarded_irqs().to_vec())
}

fn inject_virtual_irq(irq: ax_hal::irq::IrqId) -> bool {
    let intid = irq.hwirq.0 as usize;
    if let Some(route) = aarch64_virtual_timer_route(intid) {
        return super::gic::inject_interrupt_hw1(
            route.virtual_intid,
            route
                .physical_intid
                .expect("the virtual-timer route is hardware mapped"),
        );
    }
    let current_claim = crate::manager::current_forwarding_token().and_then(|token| {
        token
            .vm_id
            .checked_add(1)
            .map(|owner| (owner, token.generation))
    });
    let Some((owner, _generation)) =
        resolve_exclusive_irq_owner(SPI_REGISTRY.active_claim(irq), current_claim)
    else {
        trace!("skip AArch64 physical IRQ {intid}: no compatible VM owner");
        return false;
    };
    let Some(vm_id) = owner.checked_sub(1) else {
        return false;
    };
    let Some(vm) = crate::get_vm_by_id(vm_id) else {
        return false;
    };
    let route = vm.with_config(|config| {
        config
            .aarch64_hybrid_forwarded_irqs()
            .iter()
            .find(|route| route.host_intid() as usize == intid)
            .map(|route| PhysicalIrqRoute::new(irq, route.guest_intid() as usize))
    });
    route.is_some_and(|route| super::gic::inject_interrupt_hw1(route.guest_irq(), intid))
}
