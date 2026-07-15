//! VM-local IOAPIC inputs and local-APIC delivery bindings.

use alloc::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::{Arc, Weak},
};

use ax_kspin::SpinRaw;
use axdevice::{
    ControllerInputId, ControllerRegistration, ControllerRole, DeviceManagerError,
    DeviceManagerResult, InterruptControllerId, InterruptEndpoint, InterruptTriggerMode, IrqError,
    IrqResult, VcpuInterruptBinding, VcpuInterruptController, VcpuInterruptId, VcpuInterruptPort,
    WiredInterruptInputs, WiredIrqInput, WiredIrqSink, X86IoApicDeviceOps, X86IoApicRuntimeOps,
};
use x86_vlapic::{IoApicEoi, IoApicInterrupt};

use super::AxvmX86Vcpu;
use crate::vcpu::get_current_vcpu;

const IOAPIC_INPUT_COUNT: usize = 24;
const PRIMARY_LOCAL_APIC: VcpuInterruptId = VcpuInterruptId::new(0);
const PENDING_MESSAGE_LIMIT: usize = 256;

/// Combined registration adapter for the guest IOAPIC and per-vCPU local APICs.
pub(super) struct X86InterruptController {
    id: InterruptControllerId,
    ioapic: Arc<dyn X86IoApicDeviceOps>,
    state: Arc<SpinRaw<X86InterruptState>>,
}

struct X86InterruptState {
    inputs: BTreeMap<ControllerInputId, WiredIrqInput>,
    asserted_levels: BTreeSet<ControllerInputId>,
    ports: BTreeMap<VcpuInterruptId, VcpuInterruptPort>,
    pending: BTreeMap<VcpuInterruptId, VecDeque<IoApicInterrupt>>,
}

impl X86InterruptController {
    /// Creates an adapter around one guest IOAPIC register model.
    pub(super) fn new(id: InterruptControllerId, ioapic: Arc<dyn X86IoApicDeviceOps>) -> Self {
        Self {
            id,
            ioapic,
            state: Arc::new(SpinRaw::new(X86InterruptState {
                inputs: BTreeMap::new(),
                asserted_levels: BTreeSet::new(),
                ports: BTreeMap::new(),
                pending: BTreeMap::new(),
            })),
        }
    }

    /// Builds the topology capabilities contributed by this controller.
    pub(super) fn registration(self: &Arc<Self>) -> ControllerRegistration {
        ControllerRegistration::new(self.id, ControllerRole::Default)
            .with_wired_inputs(self.clone())
            .with_vcpu_controller(self.clone())
    }

    fn signal_input(&self, input: ControllerInputId) -> DeviceManagerResult<bool> {
        let Some(interrupt) = self.ioapic.assert_gsi(input.value()) else {
            return Ok(false);
        };
        self.queue_message(PRIMARY_LOCAL_APIC, interrupt)?;
        Ok(true)
    }

    fn queue_message(
        &self,
        target: VcpuInterruptId,
        interrupt: IoApicInterrupt,
    ) -> DeviceManagerResult {
        queue_local_apic_message(&self.state, target, interrupt)
    }

    fn complete_interrupt(&self, vector: u8) -> DeviceManagerResult<Option<IoApicEoi>> {
        let Some(eoi) = self.ioapic.end_of_interrupt(vector) else {
            return Ok(None);
        };
        let remains_asserted = self
            .state
            .lock()
            .asserted_levels
            .contains(&ControllerInputId::new(eoi.gsi));
        let redelivery = eoi.pending.or_else(|| {
            remains_asserted
                .then(|| self.ioapic.assert_gsi(eoi.gsi))
                .flatten()
        });
        if let Some(interrupt) = redelivery {
            self.queue_message(PRIMARY_LOCAL_APIC, interrupt)?;
        }
        Ok(Some(IoApicEoi {
            gsi: eoi.gsi,
            pending: redelivery,
        }))
    }
}

impl WiredInterruptInputs for X86InterruptController {
    fn input(
        &self,
        input: ControllerInputId,
        trigger: InterruptTriggerMode,
    ) -> IrqResult<WiredIrqInput> {
        if input.value() >= IOAPIC_INPUT_COUNT {
            return Err(IrqError::InvalidInput {
                endpoint: InterruptEndpoint::Wired {
                    controller: self.id,
                    input,
                },
                operation: "open IOAPIC input",
                detail: alloc::format!("IOAPIC exposes inputs 0..{}", IOAPIC_INPUT_COUNT - 1),
            });
        }
        let mut state = self.state.lock();
        if let Some(existing) = state.inputs.get(&input).cloned() {
            if existing.trigger() == trigger {
                return Ok(existing);
            }
            return Err(IrqError::InvalidTriggerMode {
                endpoint: InterruptEndpoint::Wired {
                    controller: self.id,
                    input,
                },
                operation: "open IOAPIC input",
                expected: existing.trigger(),
                actual: trigger,
            });
        }
        let created = WiredIrqInput::new(
            self.id,
            input,
            trigger,
            Arc::new(X86WiredInputSink {
                controller: self.id,
                ioapic: self.ioapic.clone(),
                state: Arc::downgrade(&self.state),
            }),
        );
        state.inputs.insert(input, created.clone());
        Ok(created)
    }
}

impl VcpuInterruptController for X86InterruptController {
    fn attach_vcpu(
        &self,
        port: VcpuInterruptPort,
    ) -> DeviceManagerResult<Arc<dyn VcpuInterruptBinding>> {
        let id = port.id();
        let mut state = self.state.lock();
        if state.ports.insert(id, port).is_some() {
            return Err(DeviceManagerError::ResourceConflict {
                operation: "attach x86 local APIC",
                detail: alloc::format!("vCPU {} is already attached", id.value()),
            });
        }
        state.pending.entry(id).or_default();
        Ok(Arc::new(X86VcpuBinding {
            id,
            state: self.state.clone(),
        }))
    }
}

impl X86IoApicRuntimeOps for X86InterruptController {
    fn vector_for_gsi(&self, gsi: usize) -> Option<u8> {
        self.ioapic.vector_for_gsi(gsi)
    }

    fn signal_gsi(&self, gsi: usize) -> DeviceManagerResult<bool> {
        self.signal_input(ControllerInputId::new(gsi))
    }

    fn end_of_interrupt(&self, vector: u8) -> DeviceManagerResult<Option<IoApicEoi>> {
        self.complete_interrupt(vector)
    }
}

struct X86WiredInputSink {
    controller: InterruptControllerId,
    ioapic: Arc<dyn X86IoApicDeviceOps>,
    state: Weak<SpinRaw<X86InterruptState>>,
}

impl X86WiredInputSink {
    fn signal(&self, input: ControllerInputId) -> IrqResult {
        let Some(interrupt) = self.ioapic.assert_gsi(input.value()) else {
            return Ok(());
        };
        let state = self.delivery_state(input)?;
        queue_local_apic_message(&state, PRIMARY_LOCAL_APIC, interrupt).map_err(|error| {
            IrqError::Backend {
                endpoint: InterruptEndpoint::Wired {
                    controller: self.controller,
                    input,
                },
                operation: "route IOAPIC message",
                detail: alloc::format!("{error}"),
            }
        })
    }

    fn delivery_state(
        &self,
        input: ControllerInputId,
    ) -> IrqResult<Arc<SpinRaw<X86InterruptState>>> {
        self.state.upgrade().ok_or_else(|| IrqError::Backend {
            endpoint: InterruptEndpoint::Wired {
                controller: self.controller,
                input,
            },
            operation: "route IOAPIC message",
            detail: "the local-APIC delivery state has been released".into(),
        })
    }
}

impl WiredIrqSink for X86WiredInputSink {
    fn set_level(&self, input: ControllerInputId, asserted: bool) -> IrqResult {
        let changed = {
            let state = self.delivery_state(input)?;
            let mut state = state.lock();
            if asserted {
                state.asserted_levels.insert(input)
            } else {
                state.asserted_levels.remove(&input);
                false
            }
        };
        if changed {
            self.signal(input)?;
        }
        Ok(())
    }

    fn pulse(&self, input: ControllerInputId) -> IrqResult {
        self.signal(input)
    }
}

fn queue_local_apic_message(
    state: &SpinRaw<X86InterruptState>,
    target: VcpuInterruptId,
    interrupt: IoApicInterrupt,
) -> DeviceManagerResult {
    let port = {
        let mut state = state.lock();
        let port = state.ports.get(&target).cloned().ok_or_else(|| {
            DeviceManagerError::ResourceNotFound {
                operation: "route IOAPIC message",
                resource: alloc::format!("local APIC for vCPU {}", target.value()),
            }
        })?;
        let pending = state.pending.entry(target).or_default();
        if pending.len() >= PENDING_MESSAGE_LIMIT {
            return Err(DeviceManagerError::ResourceConflict {
                operation: "route IOAPIC message",
                detail: alloc::format!("vCPU {} local-APIC message queue is full", target.value()),
            });
        }
        pending.push_back(interrupt);
        port
    };
    port.wake()
}

struct X86VcpuBinding {
    id: VcpuInterruptId,
    state: Arc<SpinRaw<X86InterruptState>>,
}

impl VcpuInterruptBinding for X86VcpuBinding {
    fn load(&self) -> DeviceManagerResult {
        Ok(())
    }

    fn save(&self) -> DeviceManagerResult {
        Ok(())
    }

    fn synchronize(&self) -> DeviceManagerResult {
        loop {
            let Some(interrupt) = self
                .state
                .lock()
                .pending
                .get_mut(&self.id)
                .and_then(VecDeque::pop_front)
            else {
                return Ok(());
            };
            if let Err(error) = inject_local_apic_message(self.id, interrupt) {
                self.state
                    .lock()
                    .pending
                    .entry(self.id)
                    .or_default()
                    .push_front(interrupt);
                return Err(error);
            }
        }
    }
}

impl Drop for X86VcpuBinding {
    fn drop(&mut self) {
        let mut state = self.state.lock();
        state.ports.remove(&self.id);
        state.pending.remove(&self.id);
    }
}

fn inject_local_apic_message(
    target: VcpuInterruptId,
    interrupt: IoApicInterrupt,
) -> DeviceManagerResult {
    let vcpu = get_current_vcpu::<AxvmX86Vcpu>().ok_or_else(|| {
        DeviceManagerError::UnexpectedResponse {
            operation: "inject local APIC message",
            detail: "no x86 vCPU is current".into(),
        }
    })?;
    if vcpu.id() != target.value() {
        return Err(DeviceManagerError::UnexpectedResponse {
            operation: "inject local APIC message",
            detail: alloc::format!(
                "current vCPU {} does not match target {}",
                vcpu.id(),
                target.value()
            ),
        });
    }
    vcpu.get_arch_vcpu()
        .deliver_ioapic_interrupt(
            usize::from(interrupt.vector),
            if interrupt.level_triggered {
                InterruptTriggerMode::LevelTriggered
            } else {
                InterruptTriggerMode::EdgeTriggered
            },
        )
        .map_err(|error| DeviceManagerError::UnexpectedResponse {
            operation: "inject local APIC message",
            detail: alloc::format!("{error}"),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_input_does_not_keep_controller_state_alive() {
        let ioapic = Arc::new(axdevice::X86IoApicDevice::new(
            x86_vlapic::X86GuestPhysAddr::from_usize(0xfec0_0000),
            Some(0x1000),
        ));
        let controller = Arc::new(X86InterruptController::new(
            InterruptControllerId::new(0),
            ioapic,
        ));
        let state = Arc::downgrade(&controller.state);
        let input = controller
            .input(
                ControllerInputId::new(0),
                InterruptTriggerMode::EdgeTriggered,
            )
            .expect("the first IOAPIC input must be valid");

        drop(input);
        drop(controller);

        assert!(state.upgrade().is_none());
    }
}
