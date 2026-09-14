#![no_std]
#![no_main]
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
}

#[axtest::tests(setup = init_kernel_test_services)]
mod tests {}

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
