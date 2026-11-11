#![no_std]
#![no_main]
#![feature(core_io, core_io_borrowed_buf)]
#![feature(likely_unlikely)]
#![feature(allocator_api)]
#![allow(missing_docs)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use ax_hal as _;
use ax_runtime as _;
use ax_std as _;

include!("root.rs");

fn init_kernel_test_services() {
    cgroup::init();
    stop_machine::init();
    trap::init_handlers();
    file::epoll::start_epoll_notify_worker();
}

#[axtest::tests(setup = init_kernel_test_services)]
mod tests {}

#[cfg(target_arch = "x86_64")]
#[axtest::axtest]
fn ioapic_masking_does_not_wait_for_task_registries() {
    use somehal::irq::{HwIrq, IrqDomainKind, IrqError, IrqId, domain_by_kind, intc_by_domain};

    let domain = domain_by_kind(IrqDomainKind::X86IoApic)
        .expect("the x86 test platform must register an IOAPIC");
    // Keep the controller busy so the call cannot modify a live IRQ line.
    let controller = intc_by_domain(domain.id).unwrap().try_lock().unwrap();
    let result = rdrive::with_registry_locked_for_test(|| {
        somehal::irq::with_irq_domains_locked_for_test(|| {
            somehal::irq::irq_set_enable(IrqId::new(domain.id, HwIrq(0)), false)
        })
    });
    drop(controller);

    assert_eq!(result, Err(IrqError::Busy));
}

#[cfg(target_arch = "loongarch64")]
#[axtest::axtest]
fn pch_route_lookup_ignores_unrelated_controller_borrows() {
    use somehal::irq::{
        AcpiGsiController, AcpiGsiRoute, AcpiIrqPolarity, AcpiIrqTrigger, IrqDomainKind, IrqError,
        IrqSource, domain_by_kind, intc_by_domain, resolve_irq_source,
    };

    let pch = domain_by_kind(IrqDomainKind::LoongArchPchPic)
        .expect("the LoongArch test platform must register PCH-PIC");
    // An unmatched identity exercises controller selection without changing
    // a live interrupt input's trigger or polarity.
    let route = IrqSource::AcpiGsiRoute(AcpiGsiRoute {
        gsi: 0,
        vector: 0,
        controller: AcpiGsiController::PchPic,
        controller_id: 0,
        controller_address: 0,
        controller_input: 0,
        trigger: AcpiIrqTrigger::Level,
        polarity: AcpiIrqPolarity::ActiveHigh,
    });
    let unrelated = domain_by_kind(IrqDomainKind::LoongArchEioIntc)
        .expect("the LoongArch test platform must register EIOINTC");
    let unrelated_guard = intc_by_domain(unrelated.id).unwrap().try_lock().unwrap();
    assert_eq!(resolve_irq_source(route), Err(IrqError::Unsupported));

    let target_guard = intc_by_domain(pch.id).unwrap().try_lock().unwrap();
    assert_eq!(resolve_irq_source(route), Err(IrqError::Busy));
    drop(target_guard);
    assert_eq!(resolve_irq_source(route), Err(IrqError::Unsupported));
    drop(unrelated_guard);
}
