//! VM-local PCH-PIC inputs, EIOINTC delivery, and vCPU bindings.

use alloc::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Weak},
    vec::Vec,
};

use ax_kspin::SpinRaw;
use axdevice::{
    ControllerInputId, ControllerRegistration, ControllerRole, DeviceManagerError,
    DeviceManagerResult, InterruptControllerId, InterruptEndpoint, InterruptTriggerMode, IrqError,
    IrqResult, LoongArchPchPic, LoongArchPchPicRuntimeOps, VcpuInterruptBinding,
    VcpuInterruptController, VcpuInterruptId, VcpuInterruptPort, WiredInterruptInputs,
    WiredIrqInput, WiredIrqSink,
};

use super::AxvmLoongArchVcpu;
use crate::vcpu::get_current_vcpu;

const PCH_PIC_INPUT_COUNT: usize = 64;
const PRIMARY_EIOINTC: VcpuInterruptId = VcpuInterruptId::new(0);
const PENDING_VECTOR_LIMIT: usize = 256;

/// Combined registration adapter for the guest PCH-PIC and EIOINTC.
pub(super) struct LoongArchInterruptController {
    id: InterruptControllerId,
    pch_pic: Arc<LoongArchPchPic>,
    state: Arc<SpinRaw<LoongArchInterruptState>>,
}

struct LoongArchInterruptState {
    inputs: BTreeMap<ControllerInputId, WiredIrqInput>,
    ports: BTreeMap<VcpuInterruptId, VcpuInterruptPort>,
    pending: BTreeMap<VcpuInterruptId, VecDeque<usize>>,
}

impl LoongArchInterruptController {
    /// Creates an adapter around one guest PCH-PIC register model.
    pub(super) fn new(id: InterruptControllerId, pch_pic: Arc<LoongArchPchPic>) -> Self {
        Self {
            id,
            pch_pic,
            state: Arc::new(SpinRaw::new(LoongArchInterruptState {
                inputs: BTreeMap::new(),
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

    fn route_output_vectors(&self, vectors: &[usize]) -> DeviceManagerResult {
        queue_eiointc_vectors(&self.state, PRIMARY_EIOINTC, vectors)
    }
}

impl WiredInterruptInputs for LoongArchInterruptController {
    fn input(
        &self,
        input: ControllerInputId,
        trigger: InterruptTriggerMode,
    ) -> IrqResult<WiredIrqInput> {
        if input.value() >= PCH_PIC_INPUT_COUNT {
            return Err(IrqError::InvalidInput {
                endpoint: InterruptEndpoint::Wired {
                    controller: self.id,
                    input,
                },
                operation: "open PCH-PIC input",
                detail: alloc::format!("PCH-PIC exposes inputs 0..{}", PCH_PIC_INPUT_COUNT - 1),
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
                operation: "open PCH-PIC input",
                expected: existing.trigger(),
                actual: trigger,
            });
        }

        let created = WiredIrqInput::new(
            self.id,
            input,
            trigger,
            Arc::new(PchPicInputSink {
                controller: self.id,
                pch_pic: self.pch_pic.clone(),
                state: Arc::downgrade(&self.state),
            }),
        );
        state.inputs.insert(input, created.clone());
        Ok(created)
    }
}

impl VcpuInterruptController for LoongArchInterruptController {
    fn attach_vcpu(
        &self,
        port: VcpuInterruptPort,
    ) -> DeviceManagerResult<Arc<dyn VcpuInterruptBinding>> {
        let id = port.id();
        let mut state = self.state.lock();
        if state.ports.insert(id, port).is_some() {
            return Err(DeviceManagerError::ResourceConflict {
                operation: "attach LoongArch EIOINTC",
                detail: alloc::format!("vCPU {} is already attached", id.value()),
            });
        }
        state.pending.entry(id).or_default();
        Ok(Arc::new(LoongArchVcpuBinding {
            id,
            state: self.state.clone(),
        }))
    }
}

impl LoongArchPchPicRuntimeOps for LoongArchInterruptController {
    fn service_output_events(&self) -> DeviceManagerResult {
        let mut vectors = Vec::new();
        self.pch_pic.drain_output_events(|event| {
            if event.is_asserted() {
                vectors.push(event.vector());
            }
        });
        self.route_output_vectors(&vectors)
    }
}

struct PchPicInputSink {
    controller: InterruptControllerId,
    pch_pic: Arc<LoongArchPchPic>,
    state: Weak<SpinRaw<LoongArchInterruptState>>,
}

impl PchPicInputSink {
    fn route_input(&self, input: ControllerInputId, asserted: bool) -> IrqResult {
        let Some(vector) = self.pch_pic.set_irq_level(input.value(), asserted) else {
            return Ok(());
        };
        if !asserted {
            return Ok(());
        }
        let state = self.state.upgrade().ok_or_else(|| IrqError::Backend {
            endpoint: InterruptEndpoint::Wired {
                controller: self.controller,
                input,
            },
            operation: "route PCH-PIC output",
            detail: "the EIOINTC delivery state has been released".into(),
        })?;
        queue_eiointc_vectors(&state, PRIMARY_EIOINTC, &[vector]).map_err(|error| {
            IrqError::Backend {
                endpoint: InterruptEndpoint::Wired {
                    controller: self.controller,
                    input,
                },
                operation: "route PCH-PIC output",
                detail: alloc::format!("{error}"),
            }
        })
    }
}

impl WiredIrqSink for PchPicInputSink {
    fn set_level(&self, input: ControllerInputId, asserted: bool) -> IrqResult {
        self.route_input(input, asserted)
    }

    fn pulse(&self, input: ControllerInputId) -> IrqResult {
        self.route_input(input, true)
    }
}

struct LoongArchVcpuBinding {
    id: VcpuInterruptId,
    state: Arc<SpinRaw<LoongArchInterruptState>>,
}

impl VcpuInterruptBinding for LoongArchVcpuBinding {
    fn load(&self) -> DeviceManagerResult {
        Ok(())
    }

    fn save(&self) -> DeviceManagerResult {
        Ok(())
    }

    fn synchronize(&self) -> DeviceManagerResult {
        loop {
            let Some(vector) = self
                .state
                .lock()
                .pending
                .get_mut(&self.id)
                .and_then(VecDeque::pop_front)
            else {
                return Ok(());
            };
            if let Err(error) = inject_eiointc_vector(self.id, vector) {
                self.state
                    .lock()
                    .pending
                    .entry(self.id)
                    .or_default()
                    .push_front(vector);
                return Err(error);
            }
        }
    }
}

impl Drop for LoongArchVcpuBinding {
    fn drop(&mut self) {
        let mut state = self.state.lock();
        state.ports.remove(&self.id);
        state.pending.remove(&self.id);
    }
}

fn queue_eiointc_vectors(
    state: &SpinRaw<LoongArchInterruptState>,
    target: VcpuInterruptId,
    vectors: &[usize],
) -> DeviceManagerResult {
    if vectors.is_empty() {
        return Ok(());
    }
    let port = {
        let mut state = state.lock();
        let port = state.ports.get(&target).cloned().ok_or_else(|| {
            DeviceManagerError::ResourceNotFound {
                operation: "route PCH-PIC output",
                resource: alloc::format!("EIOINTC for vCPU {}", target.value()),
            }
        })?;
        let pending = state.pending.entry(target).or_default();
        let available = PENDING_VECTOR_LIMIT.saturating_sub(pending.len());
        if vectors.len() > available {
            return Err(DeviceManagerError::ResourceConflict {
                operation: "route PCH-PIC output",
                detail: alloc::format!(
                    "vCPU {} EIOINTC queue has room for {available} vectors, received {}",
                    target.value(),
                    vectors.len()
                ),
            });
        }
        pending.extend(vectors.iter().copied());
        port
    };
    port.wake()
}

fn inject_eiointc_vector(target: VcpuInterruptId, vector: usize) -> DeviceManagerResult {
    let vcpu = get_current_vcpu::<AxvmLoongArchVcpu>().ok_or_else(|| {
        DeviceManagerError::UnexpectedResponse {
            operation: "inject EIOINTC vector",
            detail: "no LoongArch vCPU is current".into(),
        }
    })?;
    if vcpu.id() != target.value() {
        return Err(DeviceManagerError::UnexpectedResponse {
            operation: "inject EIOINTC vector",
            detail: alloc::format!(
                "current vCPU {} does not match target {}",
                vcpu.id(),
                target.value()
            ),
        });
    }
    vcpu.get_arch_vcpu()
        .deliver_controller_interrupt(vector)
        .map_err(|error| DeviceManagerError::UnexpectedResponse {
            operation: "inject EIOINTC vector",
            detail: alloc::format!("{error:?}"),
        })
}
