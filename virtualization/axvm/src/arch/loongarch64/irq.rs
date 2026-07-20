//! LoongArch platform IRQ routing used by AxVM.

use alloc::collections::BTreeMap;

use axdevice::{
    ControllerInputId, InterruptEndpointRegistration, InterruptPlanAuthority, InterruptSharing,
    InterruptTopology, InterruptTriggerMode, IrqLine, WiredIrqRequest,
};

use crate::{AxVmResult, VmStatus, ax_err, ax_err_type, machine::HostInterruptResource};

const EIOINTC_IRQ: usize = 3;

pub(crate) struct VmArchState {
    external_irq_routes: BTreeMap<usize, ExternalIrqRoute>,
}

struct ExternalIrqRoute {
    input: ControllerInputId,
    trigger: InterruptTriggerMode,
    delivery: ExternalIrqDelivery,
}

enum ExternalIrqDelivery {
    Mediated {
        line: IrqLine,
        _registration: InterruptEndpointRegistration,
    },
    Direct,
}

impl VmArchState {
    pub(crate) fn new() -> Self {
        Self {
            external_irq_routes: BTreeMap::new(),
        }
    }

    pub(crate) fn connect_external_irq_lines(
        &mut self,
        topology: &InterruptTopology,
        authority: &InterruptPlanAuthority,
        sources: &[HostInterruptResource],
    ) -> AxVmResult {
        for interrupt in sources {
            let source = interrupt.input().value();
            self.connect_external_irq_line(
                topology,
                authority,
                source,
                interrupt.input(),
                interrupt.trigger(),
            )?;
        }
        Ok(())
    }

    fn connect_external_irq_line(
        &mut self,
        topology: &InterruptTopology,
        authority: &InterruptPlanAuthority,
        source: usize,
        input: ControllerInputId,
        trigger: InterruptTriggerMode,
    ) -> AxVmResult {
        if let Some(existing) = self.external_irq_routes.get(&source) {
            if existing.input == input && existing.trigger == trigger {
                return Ok(());
            }
            return ax_err!(
                AlreadyExists,
                alloc::format!(
                    "external interrupt source {source} is already connected to input {:?}",
                    existing.input
                )
            );
        }
        let delivery = if topology.delivery() == axvm_types::InterruptDelivery::Mediated {
            let claim = authority.claim_wired(
                topology,
                WiredIrqRequest::new(input, trigger, InterruptSharing::Exclusive),
            )?;
            let (line, registration) = topology.connect_irq(claim)?.into_parts();
            ExternalIrqDelivery::Mediated {
                line,
                _registration: registration,
            }
        } else {
            ExternalIrqDelivery::Direct
        };
        self.external_irq_routes.insert(
            source,
            ExternalIrqRoute {
                input,
                trigger,
                delivery,
            },
        );
        Ok(())
    }

    fn require_external_irq_line(
        &self,
        source: usize,
        input: ControllerInputId,
        trigger: InterruptTriggerMode,
    ) -> AxVmResult {
        let route = self.external_irq_routes.get(&source).ok_or_else(|| {
            ax_err_type!(
                NotFound,
                alloc::format!(
                    "external interrupt source {source} was not authorized by the VM plan"
                )
            )
        })?;
        if route.input != input || route.trigger != trigger {
            return ax_err!(
                InvalidInput,
                alloc::format!(
                    "external interrupt source {source} was planned for input {:?} with {:?}, not \
                     input {input:?} with {trigger:?}",
                    route.input,
                    route.trigger,
                )
            );
        }
        Ok(())
    }

    fn signal_external_interrupt(&self, source: usize) -> AxVmResult {
        let route = self.external_irq_routes.get(&source).ok_or_else(|| {
            ax_err_type!(
                NotFound,
                alloc::format!("external interrupt source {source} is not connected")
            )
        })?;
        let ExternalIrqDelivery::Mediated { line, .. } = &route.delivery else {
            return Err(ax_err_type!(
                Unsupported,
                alloc::format!(
                    "direct external interrupt source {source} has no software topology line"
                )
            ));
        };
        line.pulse()?;
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
    vm.with_resources_mut(|resources| {
        resources.arch_state_mut().require_external_irq_line(
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
