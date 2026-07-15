//! LoongArch platform IRQ routing used by AxVM.

use alloc::collections::BTreeMap;

use axdevice::{
    ControllerInputId, InterruptTopology, InterruptTriggerMode, IrqLine, WiredIrqRequest,
};

use crate::{AxVmResult, VmStatus, ax_err, ax_err_type};

const EIOINTC_IRQ: usize = 3;

pub(crate) struct VmArchState {
    external_irq_lines: BTreeMap<usize, IrqLine>,
}

impl VmArchState {
    pub(crate) fn new() -> Self {
        Self {
            external_irq_lines: BTreeMap::new(),
        }
    }

    fn connect_external_irq_line(
        &mut self,
        topology: &InterruptTopology,
        source: usize,
        input: ControllerInputId,
        trigger: InterruptTriggerMode,
    ) -> AxVmResult {
        if let Some(existing) = self.external_irq_lines.get(&source) {
            if existing.input() == input && existing.trigger() == trigger {
                return Ok(());
            }
            return ax_err!(
                AlreadyExists,
                alloc::format!(
                    "external interrupt source {source} is already connected to input {:?}",
                    existing.input()
                )
            );
        }
        let line = topology.connect_irq(WiredIrqRequest::new(input, trigger))?;
        self.external_irq_lines.insert(source, line);
        Ok(())
    }

    fn signal_external_interrupt(&self, source: usize) -> AxVmResult {
        self.external_irq_lines
            .get(&source)
            .ok_or_else(|| {
                ax_err_type!(
                    NotFound,
                    alloc::format!("external interrupt source {source} is not connected")
                )
            })?
            .pulse()?;
        Ok(())
    }
}

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
) -> crate::AxVmResult {
    if vcpu_id != 0 {
        return crate::ax_err!(
            Unsupported,
            alloc::format!("LoongArch PCH-PIC routes currently target vCPU 0, got vCPU {vcpu_id}")
        );
    }
    let vm = crate::get_vm_by_id(vm_id)
        .ok_or_else(|| crate::ax_err_type!(NotFound, alloc::format!("VM[{vm_id}] not found")))?;
    let topology = vm.prepared_interrupt_topology()?;
    vm.with_resources_mut(|resources| {
        resources.arch_state_mut().connect_external_irq_line(
            &topology,
            physical_irq,
            ControllerInputId::new(guest_vector),
            InterruptTriggerMode::EdgeTriggered,
        )
    })?;
    ax_plat::irq::loongarch64_hv::register_guest_irq_route(
        physical_irq,
        vm_id,
        vcpu_id,
        guest_vector,
    );
    Ok(())
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
    let Some(vm) = crate::get_vm_by_id(vm_id) else {
        warn!("failed to signal LoongArch platform IRQ {physical_irq:#x}: VM[{vm_id}] not found");
        return;
    };
    if vcpu_id != 0 {
        warn!(
            "failed to signal LoongArch platform IRQ {physical_irq:#x}: route targets unsupported \
             VCpu[{vcpu_id}]"
        );
        return;
    }
    if let Err(err) = signal_external_interrupt(&vm, physical_irq) {
        warn!(
            "failed to signal LoongArch platform IRQ input {vector:#x}/physical {physical_irq:#x} \
             for VM[{vm_id}] VCpu[{vcpu_id}]: {err:?}"
        );
    }
}

fn signal_external_interrupt(vm: &crate::AxVM, source: usize) -> AxVmResult {
    match vm.status() {
        VmStatus::Running | VmStatus::Paused => vm.with_resources_mut(|resources| {
            resources.arch_state_mut().signal_external_interrupt(source)
        }),
        status => ax_err!(
            BadState,
            alloc::format!("VM[{}] cannot accept IRQ in {status:?}", vm.id())
        ),
    }
}
