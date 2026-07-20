//! Interrupt-topology capabilities exposed by a GICv3 controller.

use alloc::{collections::BTreeMap, sync::Arc};

use arm_vgic::{
    EventId, GicAffinity, GicV3Controller, GicV3VcpuBinding, GicV3VcpuWake, GicVcpuId, ItsDeviceId,
    PpiId, SpiId, TriggerMode, VgicError, VgicResult,
};
use ax_kspin::SpinRaw;
use axdevice_base::{
    ControllerInputId, InterruptControllerId, InterruptEndpoint, InterruptTriggerMode, IrqError,
    IrqResult, MessageInterruptSink, MsiDeviceId, MsiEndpoint, MsiEventId, MsiMessage,
    WiredIrqInput, WiredIrqSink,
};

use super::error::{device_manager_error, irq_error};
use crate::{
    DeviceManagerResult, MessageInterruptInputs, VcpuInterruptBinding, VcpuInterruptController,
    VcpuInterruptPort, WiredInterruptInputs,
};

pub(super) struct GicV3TopologyAdapter {
    id: InterruptControllerId,
    controller: Arc<GicV3Controller>,
    wired_inputs: SpinRaw<BTreeMap<ControllerInputId, WiredIrqInput>>,
    private_inputs: SpinRaw<BTreeMap<(GicVcpuId, PpiId), WiredIrqInput>>,
}

impl GicV3TopologyAdapter {
    pub(super) fn new(id: InterruptControllerId, controller: Arc<GicV3Controller>) -> Self {
        Self {
            id,
            controller,
            wired_inputs: SpinRaw::new(BTreeMap::new()),
            private_inputs: SpinRaw::new(BTreeMap::new()),
        }
    }

    pub(super) const fn id(&self) -> InterruptControllerId {
        self.id
    }

    fn private_input(
        &self,
        vcpu: GicVcpuId,
        ppi: PpiId,
        input_id: ControllerInputId,
        trigger: InterruptTriggerMode,
    ) -> IrqResult<WiredIrqInput> {
        if let Some(existing) = self.private_inputs.lock().get(&(vcpu, ppi)).cloned() {
            return require_matching_trigger(existing, trigger);
        }
        self.controller
            .configure_ppi_input(vcpu, ppi, trigger_mode(trigger))
            .map_err(|error| irq_error(self.id, Some(input_id), "configure PPI input", error))?;
        let created = WiredIrqInput::new(
            self.id,
            input_id,
            trigger,
            Arc::new(GicV3PrivateWiredSink {
                id: self.id,
                input: input_id,
                vcpu,
                ppi,
                controller: self.controller.clone(),
            }),
        );
        let mut inputs = self.private_inputs.lock();
        let input = if let Some(existing) = inputs.get(&(vcpu, ppi)).cloned() {
            require_matching_trigger(existing, trigger)?
        } else {
            inputs.insert((vcpu, ppi), created.clone());
            created
        };
        Ok(input)
    }
}

impl WiredInterruptInputs for GicV3TopologyAdapter {
    fn input(
        &self,
        input: ControllerInputId,
        trigger: InterruptTriggerMode,
    ) -> IrqResult<WiredIrqInput> {
        if let Some((vcpu, ppi)) = private_input_parts(self.id, input)? {
            return self.private_input(vcpu, ppi, input, trigger);
        }
        if let Some(existing) = self.wired_inputs.lock().get(&input).cloned() {
            return require_matching_trigger(existing, trigger);
        }
        let spi = input_spi(self.id, input)?;
        self.controller
            .configure_spi_input(spi, trigger_mode(trigger))
            .map_err(|error| irq_error(self.id, Some(input), "configure SPI input", error))?;
        let created = WiredIrqInput::new(
            self.id,
            input,
            trigger,
            Arc::new(GicV3WiredSink {
                id: self.id,
                controller: self.controller.clone(),
            }),
        );
        let mut inputs = self.wired_inputs.lock();
        if let Some(existing) = inputs.get(&input).cloned() {
            return require_matching_trigger(existing, trigger);
        }
        inputs.insert(input, created.clone());
        Ok(created)
    }
}

impl MessageInterruptInputs for GicV3TopologyAdapter {
    fn connect(&self, device: MsiDeviceId, event: MsiEventId) -> IrqResult<MsiEndpoint> {
        self.controller
            .configure_msi_input(
                ItsDeviceId::new(device.value()),
                EventId::new(event.value()),
            )
            .map_err(|error| irq_error(self.id, None, "configure MSI input", error))?;
        Ok(MsiEndpoint::new(
            self.id,
            MsiMessage::new(device, event),
            Arc::new(GicV3MessageSink {
                id: self.id,
                controller: self.controller.clone(),
            }),
        ))
    }
}

impl VcpuInterruptController for GicV3TopologyAdapter {
    fn attach_vcpu(
        &self,
        port: VcpuInterruptPort,
    ) -> DeviceManagerResult<Arc<dyn VcpuInterruptBinding>> {
        let binding = self
            .controller
            .attach_vcpu(
                GicVcpuId::new(port.id().value()),
                GicAffinity::from_mpidr(port.affinity().value()),
                Arc::new(GicV3WakeAdapter { port }),
            )
            .map_err(device_manager_error)?;
        Ok(Arc::new(GicV3BindingAdapter { binding }))
    }
}

struct GicV3WiredSink {
    id: InterruptControllerId,
    controller: Arc<GicV3Controller>,
}

impl WiredIrqSink for GicV3WiredSink {
    fn set_level(&self, input: ControllerInputId, asserted: bool) -> IrqResult {
        let spi = input_spi(self.id, input)?;
        self.controller
            .set_spi_level(spi, asserted)
            .map_err(|error| irq_error(self.id, Some(input), "set SPI level", error))
    }

    fn pulse(&self, input: ControllerInputId) -> IrqResult {
        let spi = input_spi(self.id, input)?;
        self.controller
            .pulse_spi(spi)
            .map_err(|error| irq_error(self.id, Some(input), "pulse SPI", error))
    }
}

struct GicV3MessageSink {
    id: InterruptControllerId,
    controller: Arc<GicV3Controller>,
}

struct GicV3PrivateWiredSink {
    id: InterruptControllerId,
    input: ControllerInputId,
    vcpu: GicVcpuId,
    ppi: PpiId,
    controller: Arc<GicV3Controller>,
}

impl WiredIrqSink for GicV3PrivateWiredSink {
    fn set_level(&self, input: ControllerInputId, asserted: bool) -> IrqResult {
        self.require_input(input)?;
        self.controller
            .set_ppi_level(self.vcpu, self.ppi, asserted)
            .map_err(|error| irq_error(self.id, Some(input), "set PPI level", error))
    }

    fn pulse(&self, input: ControllerInputId) -> IrqResult {
        self.require_input(input)?;
        self.controller
            .pulse_ppi(self.vcpu, self.ppi)
            .map_err(|error| irq_error(self.id, Some(input), "pulse PPI", error))
    }
}

impl GicV3PrivateWiredSink {
    fn require_input(&self, input: ControllerInputId) -> IrqResult {
        if input == self.input {
            Ok(())
        } else {
            Err(IrqError::InvalidInput {
                endpoint: InterruptEndpoint::Wired {
                    controller: self.id,
                    input,
                },
                operation: "signal GICv3 PPI",
                detail: "line belongs to a different Redistributor input".into(),
            })
        }
    }
}

impl MessageInterruptSink for GicV3MessageSink {
    fn signal(&self, message: MsiMessage) -> IrqResult {
        self.controller
            .signal_msi(
                ItsDeviceId::new(message.device().value()),
                EventId::new(message.event().value()),
            )
            .map_err(|error| irq_error(self.id, None, "signal MSI", error))
    }
}

struct GicV3WakeAdapter {
    port: VcpuInterruptPort,
}

impl GicV3VcpuWake for GicV3WakeAdapter {
    fn wake(&self) -> VgicResult {
        self.port.wake().map_err(|error| VgicError::Backend {
            operation: "wake vCPU",
            detail: alloc::format!("{error}"),
        })
    }
}

struct GicV3BindingAdapter {
    binding: GicV3VcpuBinding,
}

impl VcpuInterruptBinding for GicV3BindingAdapter {
    fn load(&self) -> DeviceManagerResult {
        self.binding.load().map_err(device_manager_error)
    }

    fn save(&self) -> DeviceManagerResult {
        self.binding.save().map_err(device_manager_error)
    }

    fn synchronize(&self) -> DeviceManagerResult {
        self.binding.synchronize().map_err(device_manager_error)
    }
}

fn require_matching_trigger(
    input: WiredIrqInput,
    requested: InterruptTriggerMode,
) -> IrqResult<WiredIrqInput> {
    if input.trigger() == requested {
        Ok(input)
    } else {
        Err(IrqError::InvalidTriggerMode {
            endpoint: InterruptEndpoint::Wired {
                controller: input.controller(),
                input: input.input(),
            },
            operation: "open GICv3 input",
            expected: input.trigger(),
            actual: requested,
        })
    }
}

fn input_spi(controller: InterruptControllerId, input: ControllerInputId) -> IrqResult<SpiId> {
    let raw = u32::try_from(input.value()).map_err(|_| IrqError::InvalidInput {
        endpoint: InterruptEndpoint::Wired { controller, input },
        operation: "connect GICv3 input",
        detail: "controller input does not fit in a GIC INTID".into(),
    })?;
    SpiId::new(raw)
        .map_err(|error| irq_error(controller, Some(input), "connect GICv3 input", error))
}

fn trigger_mode(mode: InterruptTriggerMode) -> TriggerMode {
    match mode {
        InterruptTriggerMode::EdgeTriggered => TriggerMode::Edge,
        InterruptTriggerMode::LevelTriggered => TriggerMode::Level,
    }
}

pub(super) fn private_input_id(
    controller: InterruptControllerId,
    vcpu: GicVcpuId,
    ppi: PpiId,
) -> IrqResult<ControllerInputId> {
    const PRIVATE_INPUT_TAG: usize = 1usize << (usize::BITS - 1);

    let encoded = vcpu
        .raw()
        .checked_mul(32)
        .and_then(|value| value.checked_add(ppi.raw() as usize))
        .filter(|value| *value < PRIVATE_INPUT_TAG)
        .ok_or_else(|| IrqError::InvalidInput {
            endpoint: InterruptEndpoint::Controller(controller),
            operation: "connect GICv3 PPI",
            detail: "vCPU and PPI identifiers cannot be represented as a private input".into(),
        })?;
    Ok(ControllerInputId::new(PRIVATE_INPUT_TAG | encoded))
}

fn private_input_parts(
    controller: InterruptControllerId,
    input: ControllerInputId,
) -> IrqResult<Option<(GicVcpuId, PpiId)>> {
    const PRIVATE_INPUT_TAG: usize = 1usize << (usize::BITS - 1);

    if input.value() & PRIVATE_INPUT_TAG == 0 {
        return Ok(None);
    }
    let encoded = input.value() & !PRIVATE_INPUT_TAG;
    let vcpu = GicVcpuId::new(encoded / 32);
    let raw_ppi = u8::try_from(encoded % 32).map_err(|_| IrqError::InvalidInput {
        endpoint: InterruptEndpoint::Wired { controller, input },
        operation: "decode GICv3 private input",
        detail: "encoded PPI does not fit u8".into(),
    })?;
    let ppi = PpiId::new(raw_ppi)
        .map_err(|error| irq_error(controller, Some(input), "decode GICv3 private input", error))?;
    Ok(Some((vcpu, ppi)))
}
