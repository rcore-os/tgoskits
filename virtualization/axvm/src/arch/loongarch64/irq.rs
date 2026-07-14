//! LoongArch platform IRQ routing used by AxVM.

const EIOINTC_IRQ: usize = 3;

/// Register the platform IRQ injector for LoongArch dynamic hypervisor builds.
pub(crate) fn register_platform_irq_injector() {
    ax_plat::irq::loongarch64_hv::register_virtual_irq_injector(inject_platform_irq);
    set_irq_enabled(EIOINTC_IRQ, true);
}

/// Route a host physical IRQ to a LoongArch guest interrupt vector.
pub fn register_guest_irq_route(
    physical_irq: usize,
    vm_id: usize,
    vcpu_id: usize,
    guest_vector: usize,
) {
    ax_plat::irq::loongarch64_hv::register_guest_irq_route(
        physical_irq,
        vm_id,
        vcpu_id,
        guest_vector,
    );
}

/// Remove all routed LoongArch guest IRQs owned by one VM.
pub fn unregister_guest_irq_routes(vm_id: usize) {
    ax_plat::irq::loongarch64_hv::unregister_guest_irq_routes(vm_id);
}

fn set_irq_enabled(raw_irq: usize, enabled: bool) {
    use ax_std::os::arceos::modules::ax_hal::irq::{self, IrqSource};

    let gsi = match u32::try_from(raw_irq) {
        Ok(gsi) => gsi,
        Err(_) => {
            warn!("failed to resolve LoongArch passthrough IRQ {raw_irq}: out of GSI range");
            return;
        }
    };
    let irq = match irq::resolve_irq_source(IrqSource::AcpiGsi(gsi)) {
        Ok(irq) => irq,
        Err(err) => {
            warn!("failed to resolve LoongArch passthrough IRQ {raw_irq}: {err:?}");
            return;
        }
    };
    if let Err(err) = irq::set_enable(irq, enabled) {
        warn!(
            "failed to set LoongArch passthrough IRQ {raw_irq} ({irq:?}) enabled={enabled}: \
             {err:?}"
        );
    }
}

fn inject_platform_irq(vm_id: usize, vcpu_id: usize, vector: usize, physical_irq: usize) {
    let interrupt = crate::vm::PendingInterrupt::External {
        vector,
        physical_irq,
    };
    if let Err(err) = crate::runtime::vcpus::queue_pending_interrupt(vm_id, vcpu_id, interrupt) {
        warn!(
            "failed to queue LoongArch platform IRQ {vector:#x}/physical {physical_irq:#x} for \
             VM[{vm_id}] VCpu[{vcpu_id}]: {err:?}"
        );
    }
}
