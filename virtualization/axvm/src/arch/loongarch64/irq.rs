//! LoongArch platform IRQ routing used by AxVM.

use std::{collections::BTreeMap, sync::Arc};

use ax_std::os::arceos::sync::IrqSafeMutex;
use axdevice_base::{
    ControllerInputId, InterruptControllerId, InterruptEndpoint, IrqError, IrqResult,
    VirtualInterruptController, WiredIrqInput, WiredIrqSink,
};
use axvm_types::InterruptTriggerMode;

const PCH_PIC_INPUT_COUNT: usize = 64;

struct LoongArchPchPicIrqSink {
    vm_id: usize,
    pic: Arc<axdevice::LoongArchPchPic>,
}

impl LoongArchPchPicIrqSink {
    fn endpoint(input: ControllerInputId) -> InterruptEndpoint {
        InterruptEndpoint::Wired {
            controller: InterruptControllerId::new(0),
            input,
        }
    }
}

impl WiredIrqSink for LoongArchPchPicIrqSink {
    fn set_level(&self, input: ControllerInputId, asserted: bool) -> IrqResult {
        let vector = self.pic.set_irq_level(input.value(), asserted);
        if !asserted {
            return Ok(());
        }
        let Some(vector) = vector else {
            return Ok(());
        };
        crate::runtime::vcpus::queue_interrupt(self.vm_id, 0, vector).map_err(|error| {
            IrqError::Backend {
                endpoint: Self::endpoint(input),
                operation: "queue LoongArch PCH-PIC output",
                detail: std::format!("{error}"),
            }
        })
    }

    fn pulse(&self, input: ControllerInputId) -> IrqResult {
        self.set_level(input, true)?;
        self.set_level(input, false)
    }
}

/// Minimal adapter that lets DeviceRuntime resolve sources against the single
/// guest-visible PCH-PIC instance.
pub(crate) struct LoongArchInterruptDomain {
    sink: Arc<LoongArchPchPicIrqSink>,
    inputs: IrqSafeMutex<BTreeMap<usize, (InterruptTriggerMode, WiredIrqInput)>>,
}

impl LoongArchInterruptDomain {
    pub(crate) fn new(vm_id: usize, pic: Arc<axdevice::LoongArchPchPic>) -> Arc<Self> {
        Arc::new(Self {
            sink: Arc::new(LoongArchPchPicIrqSink { vm_id, pic }),
            inputs: IrqSafeMutex::new(BTreeMap::new()),
        })
    }
}

impl VirtualInterruptController for LoongArchInterruptDomain {
    fn id(&self) -> InterruptControllerId {
        InterruptControllerId::new(0)
    }

    fn wired_input(
        &self,
        input: ControllerInputId,
        trigger: InterruptTriggerMode,
    ) -> IrqResult<WiredIrqInput> {
        if input.value() >= PCH_PIC_INPUT_COUNT {
            return Err(IrqError::InvalidInput {
                endpoint: InterruptEndpoint::Wired {
                    controller: self.id(),
                    input,
                },
                operation: "open LoongArch PCH-PIC input",
                detail: std::format!(
                    "input {} is outside 0..{PCH_PIC_INPUT_COUNT}",
                    input.value()
                ),
            });
        }
        let mut inputs = self.inputs.lock();
        if let Some((registered_trigger, registered)) = inputs.get(&input.value()) {
            if *registered_trigger != trigger {
                return Err(IrqError::InvalidInput {
                    endpoint: InterruptEndpoint::Wired {
                        controller: self.id(),
                        input,
                    },
                    operation: "open LoongArch PCH-PIC input",
                    detail: std::format!(
                        "input {} is already registered as {registered_trigger:?}",
                        input.value()
                    ),
                });
            }
            return Ok(registered.clone());
        }

        let sink: Arc<dyn WiredIrqSink> = self.sink.clone();
        let registered = WiredIrqInput::new(self.id(), input, trigger, sink);
        inputs.insert(input.value(), (trigger, registered.clone()));
        Ok(registered)
    }
}

pub(crate) fn create_interrupt_domain(
    vm_id: usize,
    pic: Arc<axdevice::LoongArchPchPic>,
) -> Arc<LoongArchInterruptDomain> {
    LoongArchInterruptDomain::new(vm_id, pic)
}

/// Register the platform IRQ injector for LoongArch dynamic hypervisor builds.
pub(crate) fn register_platform_irq_injector() {
    ax_plat::irq::loongarch64_hv::register_virtual_irq_injector(inject_platform_irq);
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

fn inject_platform_irq(vm_id: usize, vcpu_id: usize, vector: usize, physical_irq: usize) {
    if let Err(err) = crate::runtime::vcpus::queue_pending_interrupt(
        vm_id,
        vcpu_id,
        crate::vm::PendingInterrupt::External {
            vector,
            physical_irq,
        },
    ) {
        warn!(
            "failed to queue LoongArch platform IRQ {vector:#x}/physical {physical_irq:#x} for \
             VM[{vm_id}] VCpu[{vcpu_id}]: {err:?}"
        );
    }
}
